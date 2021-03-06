extern crate winapi;

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::Duration;

use winapi::ctypes::c_void;
use winapi::shared::minwindef::{BOOL, DWORD, FALSE, TRUE};
use winapi::shared::ntdef::NULL;
use winapi::shared::winerror::{ERROR_IO_PENDING, ERROR_SEM_TIMEOUT, WAIT_TIMEOUT};
use winapi::um::commapi::{SetCommMask, SetCommState, SetCommTimeouts, WaitCommEvent};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING, FlushFileBuffers, QueryDosDeviceW, ReadFile, WriteFile};
use winapi::um::handleapi::{CloseHandle, DuplicateHandle, INVALID_HANDLE_VALUE};
use winapi::um::ioapiset::{CancelIo, GetOverlappedResult};
use winapi::um::minwinbase::OVERLAPPED;
use winapi::um::processthreadsapi::GetCurrentProcess;
use winapi::um::synchapi::{CreateEventW, WaitForSingleObject};
use winapi::um::winbase::{CBR_256000, COMMTIMEOUTS, DCB, FILE_FLAG_OVERLAPPED, INFINITE, NOPARITY, ONESTOPBIT, WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, GENERIC_READ, GENERIC_WRITE, HANDLE, MAXDWORD};

pub struct SerialPort {
	comdev: HANDLE,
	event_read: HANDLE,
	event_write: HANDLE,
	timeout_read_ms: DWORD
}

// HANDLE is type *mut c_void which does not implement Send and Sync, so
// implement it here, because sharing HANDLEs is inherently thread-safe
unsafe impl Send for SerialPort {}
unsafe impl Sync for SerialPort {}

impl SerialPort {
	pub fn open<T>(port_name: &T, timeout: Option<Duration>) -> io::Result<Self>
			where T: AsRef<OsStr> + ?Sized {
		// construct prefixed COM port name to support COMn with n > 9
		let mut name = Vec::<u16>::new();
		name.extend(OsStr::new("\\\\.\\").encode_wide());
		name.extend(port_name.as_ref().encode_wide());
		name.push(0);

		// open COM port as raw HANDLE
		let comdev = unsafe {
			CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0,
				ptr::null_mut(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, 0 as HANDLE)
		};
		if comdev == INVALID_HANDLE_VALUE {
			return Err(io::Error::last_os_error());
		}

		// configure COM port for raw communication
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-dcb
		let mut dcb: DCB = unsafe { mem::zeroed() };
		dcb.DCBlength = mem::size_of::<DCB>() as u32;
		dcb.set_fBinary(TRUE as u32);
		dcb.BaudRate = CBR_256000;
		dcb.ByteSize = 8;
		dcb.StopBits = ONESTOPBIT;
		dcb.Parity = NOPARITY;
		if unsafe { SetCommState(comdev, &mut dcb) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// populate COMMTIMEOUTS struct from Option<Duration>
		// https://docs.microsoft.com/en-us/windows/win32/devio/time-outs
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts
		let mut timeouts = if let Some(dur) = timeout {
			let mut dur_ms = dur.as_secs() * 1000
			               + dur.subsec_millis() as u64;

			// clip dur_ms to valid range from 1 to MAXDWORD - 1
			// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts#remarks
			if dur_ms < 1 {
				dur_ms = 1;
			} else if dur_ms >= MAXDWORD as u64 {
				dur_ms = (MAXDWORD - 1) as u64;
			}

			COMMTIMEOUTS {
				// return immediately if bytes are available (like POSIX would)
				// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts#remarks
				ReadIntervalTimeout: MAXDWORD,
				ReadTotalTimeoutMultiplier: 0,
				ReadTotalTimeoutConstant: 0,
				// MAXDWORD is *not* a reserved WriteTotalTimeoutMultiplier
				// value, i.e., setting it incurs an very long write timeout
				WriteTotalTimeoutMultiplier: 0,
				WriteTotalTimeoutConstant: dur_ms as DWORD,
			}
		} else {
			// blocking read/write without timeout
			COMMTIMEOUTS {
				ReadIntervalTimeout: MAXDWORD,
				ReadTotalTimeoutMultiplier: 0,
				ReadTotalTimeoutConstant: 0,
				WriteTotalTimeoutMultiplier: 0,
				WriteTotalTimeoutConstant: 0,
			}
		};

		let timeout_read_ms: DWORD = if let Some(dur) = timeout {
			let mut dur_ms = dur.as_secs() * 1000
			               + dur.subsec_millis() as u64;
			if dur_ms < INFINITE as u64 {
				dur_ms as DWORD
			} else {
				INFINITE - 1
			}
		} else {
			INFINITE
		};

		// set timeouts
		if unsafe { SetCommTimeouts(comdev, &mut timeouts) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// set event mask to EV_RXCHAR, so WaitCommEvent() can be used to wait
		// until input is available
		if unsafe { SetCommMask(comdev, 0x0001 as DWORD) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// create unnamed event objects for asynchronous I/O
		let event_read = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_read == NULL {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}
		let event_write = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_write == NULL {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		Ok(Self { comdev, event_read, event_write, timeout_read_ms })
	}

	pub fn try_clone(&self) -> io::Result<Self> {
		// create new unnamed event objects for asynchronous I/O
		let event_read = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_read == NULL {
			return Err(io::Error::last_os_error());
		}
		let event_write = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_write == NULL {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// duplicate communications device handle
		let mut comdev = INVALID_HANDLE_VALUE;
		let process = unsafe { GetCurrentProcess() };
		let res = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle
			DuplicateHandle(process, self.comdev, process, &mut comdev,
				0, FALSE, DUPLICATE_SAME_ACCESS)
		};

		if res == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_write) };
			debug_assert_ne!(_res, 0);

			Err(error)
		} else {
			Ok(Self { comdev, event_read, event_write, timeout_read_ms: self.timeout_read_ms })
		}
	}

	pub fn list_devices() -> Vec<OsString> {
		let mut devices = Vec::new();
		let mut path_wide = [0u16; 1024];

		// check result of QueryDosDeviceW() for COM0 thru COM255 to find
		// existing COM ports (see: https://stackoverflow.com/a/18691898)
		for n in 0 ..= 255 {
			// construct wide string for COMn
			let name = OsString::from(format!("COM{}", n));
			let mut name_wide: Vec<u16> = name.encode_wide().collect();
			name_wide.push(0);

			// QueryDosDeviceW() returns 0 if the COM port does not exist
			let len = unsafe { QueryDosDeviceW(name_wide.as_ptr(),
				path_wide.as_mut_ptr(),	path_wide.len() as DWORD) } as usize;
			if len > 0 {
				devices.push(name);
			}
		}

		devices
	}
}

impl Drop for SerialPort {
	fn drop(&mut self) {
		// https://docs.microsoft.com/de-de/windows/win32/api/handleapi/nf-handleapi-closehandle
		let _res = unsafe { CloseHandle(self.comdev) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.event_read) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.event_write) };
		debug_assert_ne!(_res, 0);
	}
}

impl io::Read for SerialPort {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		// call WaitCommEvent() to issue overlapped I/O request waiting for
		// until EV_RXCHAR event occurs
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event_read;
		let mut evt_mask: DWORD = 0;
		match unsafe {
			// implicitly resets event to non-singaled before returning
			WaitCommEvent(self.comdev, &mut evt_mask, &mut overlapped)
		} {
			FALSE if unsafe { GetLastError() } != ERROR_IO_PENDING => {
				return Err(io::Error::last_os_error());
			},
			FALSE => (),
			TRUE => {
				println!("WaitCommEvent() returned TRUE: {:}", evt_mask);
			},
			_ => unreachable!()
		}

		// wait for WaitCommEvent() to complete or timeout to occur
		// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject
		match unsafe { WaitForSingleObject(self.event_read, self.timeout_read_ms) } {
			WAIT_FAILED => return Err(io::Error::last_os_error()),
			WAIT_OBJECT_0 => {
				let mut _undef: DWORD = 0;
				if unsafe { GetOverlappedResult(self.comdev, &mut overlapped, &mut _undef, FALSE) } == 0 {
					return Err(io::Error::last_os_error());
				}
			},
			WAIT_TIMEOUT => {
				// waiting for WaitCommEvent() timed out, but the overlapped
				// I/O requests issued by WaitCommEvent() is still pending.
				// Because the OVERLAPPED structure goes out of scope when
				// this function returns, the request must be cancelled now to
				// prevent undefined behavior (e.g., future WaitCommEvent()
				// calls returning prematurely, likely because a zeroed
				// OVERLAPPED struct at the same address is used).
				// NOTE: CancelIo() only cancels I/O requests issued by the
				//       calling thread.
				if unsafe { CancelIo(self.comdev) } == 0 {
					return Err(io::Error::last_os_error());
				}
				return Err(io::Error::new(io::ErrorKind::TimedOut,
					"WaitCommEvent() timed out"))
			},
			// WAIT_ABANDONED must not occur, because self.comdev isn't a mutex
			_ => panic!("invalid WaitForSingleObject() return value")
		}

		// queue async read
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event_read;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile
			ReadFile(self.comdev, buf.as_mut_ptr() as *mut c_void,
				buf.len() as DWORD, ptr::null_mut(), &mut overlapped)
		};

		// async read request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		if res == FALSE && unsafe { GetLastError() } != ERROR_IO_PENDING {
			return Err(io::Error::last_os_error());
		}

		// wait for completion
		let mut len: DWORD = 0;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult
			GetOverlappedResult(self.comdev, &mut overlapped, &mut len, TRUE)
		};
		if res == FALSE {
			return Err(io::Error::last_os_error());
		}

		match len {
			0 if buf.len() == 0 => Ok(0),
			0 => Err(io::Error::new(io::ErrorKind::TimedOut,
					"ReadFile() timed out (0 bytes read)")),
			_ => Ok(len as usize)
		}
	}
}

impl io::Write for SerialPort {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		// queue async write
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event_write;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefile
			WriteFile(self.comdev, buf.as_ptr() as *const c_void,
				buf.len() as DWORD, ptr::null_mut(), &mut overlapped)
		};

		// async write request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		if res == FALSE && unsafe { GetLastError() } != ERROR_IO_PENDING {
			return Err(io::Error::last_os_error());
		}

		// wait for completion
		let mut len: DWORD = 0;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult
			GetOverlappedResult(self.comdev, &mut overlapped, &mut len, TRUE)
		};
		if res == FALSE {
			// WriteFile() may fail with ERROR_SEM_TIMEOUT, which is not
			// io::ErrorKind::TimedOut prior to Rust 1.46, so create a custom
			// error with kind TimedOut to simplify subsequent error handling.
			// https://github.com/rust-lang/rust/pull/71756
			let error = io::Error::last_os_error();
			// TODO: wrap if clause in if_rust_version! { < 1.46 { ... }}
			if error.raw_os_error().unwrap() as DWORD == ERROR_SEM_TIMEOUT
			&& error.kind() != io::ErrorKind::TimedOut {
				return Err(io::Error::new(io::ErrorKind::TimedOut,
					"WriteFile() timed out (ERROR_SEM_TIMEOUT)"));
			}
			return Err(error);
		}

		match len {
			0 if buf.len() == 0 => Ok(0),
			0 => Err(io::Error::new(io::ErrorKind::TimedOut,
					"WriteFile() timed out (0 bytes written)")),
			_ => Ok(len as usize)
		}
	}

	fn flush(&mut self) -> io::Result<()> {
		// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-purgecomm#remarks
		match unsafe { FlushFileBuffers(self.comdev) } {
			0 => Err(io::Error::last_os_error()),
			_ => Ok(()),
		}
	}
}
