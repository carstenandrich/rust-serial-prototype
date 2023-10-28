extern crate windows_sys;

use std::ffi::{c_void, OsStr, OsString};
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::{
	Devices::Communication::*,
	Foundation::*,
	Storage::FileSystem::*,
	System::IO::*,
	System::Threading::*,
	System::WindowsProgramming::*
};

const MAXDWORD: u32 = u32::MAX;

pub struct SerialPort {
	comdev: HANDLE,
	event: HANDLE
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

		// create unnamed event object for asynchronous I/O
		let event = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), FALSE, FALSE, ptr::null_mut())
		};
		if event == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// configure COM port for raw communication
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-dcb
		let mut dcb: DCB = unsafe { mem::zeroed() };
		dcb.DCBlength = mem::size_of::<DCB>() as u32;
		// set fBinary field
		dcb._bitfield = 0x0000_0001;
		dcb.BaudRate = CBR_256000;
		dcb.ByteSize = 8;
		dcb.StopBits = ONESTOPBIT;
		dcb.Parity = NOPARITY;
		if unsafe { SetCommState(comdev, &mut dcb) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event) };
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
				ReadTotalTimeoutMultiplier: MAXDWORD,
				ReadTotalTimeoutConstant: dur_ms as u32,
				// MAXDWORD is *not* a reserved WriteTotalTimeoutMultiplier
				// value, i.e., setting it incurs an very long write timeout
				WriteTotalTimeoutMultiplier: 0,
				WriteTotalTimeoutConstant: dur_ms as u32,
			}
		} else {
			// blocking read/write without timeout
			// FIXME: read() blocks until the read buffer is full
			COMMTIMEOUTS {
				ReadIntervalTimeout: 0,
				ReadTotalTimeoutMultiplier: 0,
				ReadTotalTimeoutConstant: 0,
				WriteTotalTimeoutMultiplier: 0,
				WriteTotalTimeoutConstant: 0,
			}
		};

		// set timeouts
		if unsafe { SetCommTimeouts(comdev, &mut timeouts) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		Ok(Self { comdev, event })
	}

	pub fn try_clone(&self) -> io::Result<Self> {
		// create new unnamed event object for asynchronous I/O
		let event = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/synchapi/nf-synchapi-createeventa
			CreateEventW(ptr::null_mut(), FALSE, FALSE, ptr::null_mut())
		};
		if event == 0 {
			return Err(io::Error::last_os_error());
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

			let _res = unsafe { CloseHandle(event) };
			debug_assert_ne!(_res, 0);

			Err(error)
		} else {
			Ok(Self { comdev, event })
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
				path_wide.as_mut_ptr(),	path_wide.len() as u32) } as usize;
			if len > 0 {
				devices.push(name);
			}
		}

		devices
	}

	pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
		// queue async read
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile
			ReadFile(self.comdev, buf.as_mut_ptr() as *mut c_void,
				buf.len() as u32, ptr::null_mut(), &mut overlapped)
		};

		// async read request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		if res == FALSE && unsafe { GetLastError() } != ERROR_IO_PENDING {
			return Err(io::Error::last_os_error());
		}

		// wait for completion
		let mut len: u32 = 0;
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

	pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
		// queue async write
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefile
			WriteFile(self.comdev, buf.as_ptr(),
				buf.len() as u32, ptr::null_mut(), &mut overlapped)
		};

		// async write request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		if res == FALSE && unsafe { GetLastError() } != ERROR_IO_PENDING {
			return Err(io::Error::last_os_error());
		}

		// wait for completion
		let mut len: u32 = 0;
		let res: BOOL = unsafe {
			// https://docs.microsoft.com/de-de/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult
			GetOverlappedResult(self.comdev, &mut overlapped, &mut len, TRUE)
		};
		if res == FALSE {
			// minimum supported rust version (MSRV) is 1.46, because WriteFile()
			// may fail with ERROR_SEM_TIMEOUT, which is
			// std::io::ErrorKind::TimedOut only since Rust 1.46, see:
			// https://github.com/rust-lang/rust/pull/71756
			let errcode = unsafe { GetLastError() };
			return Err(io::Error::from_raw_os_error(errcode as i32));
		}

		match len {
			0 if buf.len() == 0 => Ok(0),
			0 => Err(io::Error::new(io::ErrorKind::TimedOut,
					"WriteFile() timed out (0 bytes written)")),
			_ => Ok(len as usize)
		}
	}

	pub fn flush(&self) -> io::Result<()> {
		// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-purgecomm#remarks
		match unsafe { FlushFileBuffers(self.comdev) } {
			0 => Err(io::Error::last_os_error()),
			_ => Ok(()),
		}
	}
}

impl Drop for SerialPort {
	fn drop(&mut self) {
		// https://docs.microsoft.com/de-de/windows/win32/api/handleapi/nf-handleapi-closehandle
		let _res = unsafe { CloseHandle(self.comdev) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.event) };
		debug_assert_ne!(_res, 0);
	}
}
