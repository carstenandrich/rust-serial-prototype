extern crate winapi;

use std::ffi::{OsStr, OsString};
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::{Duration, Instant};

use winapi::ctypes::c_void;
use winapi::shared::minwindef::{DWORD, FALSE, TRUE};
use winapi::shared::ntdef::NULL;
use winapi::shared::winerror::{ERROR_IO_PENDING, ERROR_OPERATION_ABORTED, ERROR_SEM_TIMEOUT, WAIT_TIMEOUT};
use winapi::um::commapi::{SetCommMask, SetCommState, SetCommTimeouts, WaitCommEvent};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING, FlushFileBuffers, QueryDosDeviceW, ReadFile, WriteFile};
use winapi::um::handleapi::{CloseHandle, DuplicateHandle, INVALID_HANDLE_VALUE};
use winapi::um::ioapiset::{CancelIo, GetOverlappedResult};
use winapi::um::minwinbase::OVERLAPPED;
use winapi::um::processthreadsapi::GetCurrentProcess;
use winapi::um::synchapi::{CreateEventW, CreateMutexW, ReleaseMutex, WaitForSingleObject};
use winapi::um::winbase::{CBR_256000, COMMTIMEOUTS, DCB, FILE_FLAG_OVERLAPPED, INFINITE, NOPARITY, ONESTOPBIT, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, GENERIC_READ, GENERIC_WRITE, HANDLE, MAXDWORD};

// character received event mask for WaitCommEvent(), which is missing from
// winapi 0.3.9
// https://docs.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-waitcommevent#parameters
const EV_RXCHAR: DWORD = 0x0001;

pub struct SerialPort {
	comdev: HANDLE,
	event_read: HANDLE,
	event_write: HANDLE,
	mutex_read: HANDLE,
	timeout_read: Option<Duration>,
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
		// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
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
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// compute read timeout in millisecons for WaitForSingleObject()
		// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject#parameters
		let timeout_read_ms: DWORD = match timeout {
			None => INFINITE,
			Some(dur) if dur == Duration::new(0, 0) => 0,
			Some(dur) if dur <= Duration::from_millis(1) => 1,
			// clip read timeouts at INFINITE - 1 == MAXDWORD - 1
			Some(dur) if dur >= Duration::from_millis(INFINITE as u64) => INFINITE - 1,
			Some(dur) => dur.as_millis() as DWORD
		};

		// compute write timeout in millisecons for COMMTIMEOUTS
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts#members
		let timeout_write_ms: DWORD = match timeout {
			// zero is no (i.e., infinite) timeout
			None => 0,
			// COMMTIMEOUTS does not support non-blocking write, so set
			// smallest possible timeout (1 ms) instead for all Durations
			// up to 1 ms, including zero Duration
			Some(dur) if dur <= Duration::from_millis(1) => 1,
			Some(dur) if dur >= Duration::from_millis(MAXDWORD as u64) => MAXDWORD,
			Some(dur) => dur.as_millis() as DWORD
		};

		// populate COMMTIMEOUTS struct
		// https://docs.microsoft.com/en-us/windows/win32/devio/time-outs
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts
		let mut timeouts = COMMTIMEOUTS {
			// read timeouts are handled via WaitForSingleObject(), so
			// configure non-blocking read regardless of timeout_read_ms value
			ReadIntervalTimeout: MAXDWORD,
			ReadTotalTimeoutMultiplier: 0,
			ReadTotalTimeoutConstant: 0,
			// set write timeout computed above
			WriteTotalTimeoutMultiplier: 0,
			WriteTotalTimeoutConstant: timeout_write_ms as DWORD,
		};

		// set timeouts
		if unsafe { SetCommTimeouts(comdev, &mut timeouts) } == 0 {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// set event mask to EV_RXCHAR, so WaitCommEvent() can be used to wait
		// until input is available
		if unsafe { SetCommMask(comdev, EV_RXCHAR) } == 0 {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// create unnamed event objects for asynchronous I/O
		let event_read = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_read == NULL {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}
		let event_write = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_write == NULL {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// create unnamed mutex object for reading from COM port
		let mutex_read = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createmutexw
			CreateMutexW(ptr::null_mut(), FALSE, ptr::null_mut())
		};
		if mutex_read == NULL {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(comdev) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_write) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		Ok(Self {
			comdev,
			event_read,
			event_write,
			mutex_read,
			timeout_read: timeout,
			timeout_read_ms
		})
	}

	pub fn try_clone(&self) -> io::Result<Self> {
		// create new unnamed event objects for asynchronous I/O
		let event_read = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_read == NULL {
			return Err(io::Error::last_os_error());
		}
		let event_write = unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-createeventw
			CreateEventW(ptr::null_mut(), TRUE, FALSE, ptr::null_mut())
		};
		if event_write == NULL {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// duplicate mutex object
		// https://docs.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle
		let mut mutex_read = INVALID_HANDLE_VALUE;
		let process = unsafe { GetCurrentProcess() };
		if unsafe { DuplicateHandle(
			process,
			self.mutex_read,
			process,
			&mut mutex_read,
			0,
			FALSE,
			DUPLICATE_SAME_ACCESS
		)} == 0 {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_write) };
			debug_assert_ne!(_res, 0);
			return Err(error)
		}

		// duplicate communications device handle
		// https://docs.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle
		let mut comdev = INVALID_HANDLE_VALUE;
		if unsafe { DuplicateHandle(
			process,
			self.comdev,
			process,
			&mut comdev,
			0,
			FALSE,
			DUPLICATE_SAME_ACCESS
		)} == 0 {
			// close open handles and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { CloseHandle(event_read) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(event_write) };
			debug_assert_ne!(_res, 0);
			let _res = unsafe { CloseHandle(mutex_read) };
			debug_assert_ne!(_res, 0);
			Err(error)
		} else {
			// return cloned self on success
			Ok(Self {
				comdev,
				event_read,
				event_write,
				mutex_read,
				timeout_read: self.timeout_read,
				timeout_read_ms: self.timeout_read_ms
			})
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
			if unsafe { QueryDosDeviceW(
				name_wide.as_ptr(),
				path_wide.as_mut_ptr(),
				path_wide.len() as DWORD
			) as usize } > 0 {
				devices.push(name);
			}
		}

		devices
	}

	pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
		// get time before acquiring mutex to update read timeout later
		let entry = Instant::now();

		// acquire read mutex (may block up to self.timeout_read_ms)
		match unsafe {
			WaitForSingleObject(self.mutex_read, self.timeout_read_ms)
		} {
			WAIT_FAILED => return Err(io::Error::last_os_error()),
			WAIT_OBJECT_0 => (),
			WAIT_TIMEOUT => {
				return Err(io::Error::new(io::ErrorKind::TimedOut,
					"WaitForSingleObject() timed out"))
			},
			WAIT_ABANDONED => unimplemented!("WAIT_ABANDONED occurred"),
			_ if cfg!(debug_assertions) => panic!("illegal WaitForSingleObject() return value"),
			_ => unreachable!()
		}

		// even when holding the mutex, WaitCommEvent() may return spuriously
		// with a subsequent ReadFile(self.comdev, ...) returning 0, indicating
		// that a timeout occurred. to counter this, call ReadFile() until
		// a read succeeds or the read times out.
		loop {
			// compute read timeout in ms, accounting for time already elapsed
			let elapsed = entry.elapsed();
			let timeout_ms: c_int = match self.timeout_read {
				None => INFINITE,
				Some(timeout) if elapsed > timeout => {
					return Err(io::Error::new(io::ErrorKind::TimedOut,
						"reading from COM port timed out"));
				},
				Some(timeout) if timeout - elapsed <= Duration::from_millis(1) => 1,
				Some(timeout) if timeout - elapsed >= Duration::from_millis(INFINITE as u64) => INFINITE - 1,
				Some(timeout) => (timeout - elapsed).as_millis() as c_int
			};
		}

		// call WaitCommEvent() to issue overlapped I/O request blocking until
		// EV_RXCHAR event occurs
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event_read;
		let mut evt_mask: DWORD = 0;
		match unsafe {
			// implicitly resets event to non-signaled before returning
			WaitCommEvent(self.comdev, &mut evt_mask, &mut overlapped)
		} {
			FALSE if unsafe { GetLastError() } != ERROR_IO_PENDING => {
				// release mutex and return original error on failure
				let error = io::Error::last_os_error();
				let _res = unsafe { ReleaseMutex(self.mutex_read) };
				debug_assert_ne!(_res, 0);
				return Err(error);
			},
			FALSE => (),
			// FIXME: if WaitCommEvent() returns TRUE, the subsequent
			//        WaitForSingleObject() may be superfluous
			TRUE => unimplemented!("WaitCommEvent() returned TRUE: {:}", evt_mask),
			_ => unreachable!()
		}

		// compute updated read timeout, accounting for time spent waiting for
		// read mutex, so total timeout does not exceed self.timeout_read_ms
		let waited_ms = instant_start.elapsed().as_millis();
		let timeout_read_ms = if waited_ms < self.timeout_read_ms as u128 {
			self.timeout_read_ms - waited_ms as DWORD
		} else {
			0
		};

		// wait for WaitCommEvent() to complete or timeout to occur
		// https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject
		match unsafe { WaitForSingleObject(self.event_read, timeout_read_ms) } {
			WAIT_FAILED => {
				// release mutex and return original error on failure
				let error = io::Error::last_os_error();
				let _res = unsafe { ReleaseMutex(self.mutex_read) };
				debug_assert_ne!(_res, 0);
				return Err(error);
			},
			WAIT_OBJECT_0 => {
				let mut _undef: DWORD = 0;
				if unsafe { GetOverlappedResult(
					self.comdev,
					&mut overlapped,
					&mut _undef,
					FALSE
				)} == 0 {
					// release mutex and return original error on failure
					let error = io::Error::last_os_error();
					let _res = unsafe { ReleaseMutex(self.mutex_read) };
					debug_assert_ne!(_res, 0);
					return Err(error);
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
				// https://docs.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelio
				if unsafe { CancelIo(self.comdev) } == 0 {
					// release mutex and return original error on failure
					let error = io::Error::last_os_error();
					let _res = unsafe { ReleaseMutex(self.mutex_read) };
					debug_assert_ne!(_res, 0);
					return Err(error);
				}
				// Check if I/O operation was actually cancelled or
				// if it raced to completion before cancellation
				// occurred.
				// https://docs.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelio#remarks
				let mut _undef: DWORD = 0;
				if unsafe { GetOverlappedResult(
					self.comdev,
					&mut overlapped,
					&mut _undef,
					FALSE
				)} == 0 {
					// release mutex and return original error on failure
					let errcode = unsafe { GetLastError() };
					if errcode != ERROR_OPERATION_ABORTED {
						// release mutex and return original error on failure
						let _res = unsafe { ReleaseMutex(self.mutex_read) };
						debug_assert_ne!(_res, 0);
						return Err(io::Error::from_raw_os_error(errcode as i32));
					}
				} else {
					println!("WaitCommEvent() cancelled but succeeded: evt_mask={:}", evt_mask);
				}

				// release mutex
				let _res = unsafe { ReleaseMutex(self.mutex_read) };
				debug_assert_ne!(_res, 0);

				return Err(io::Error::new(io::ErrorKind::TimedOut,
					"WaitCommEvent() timed out"))
			},
			// WAIT_ABANDONED must not occur, because self.comdev isn't a mutex
			_ if cfg!(debug_assertions) => panic!("illegal WaitForSingleObject() return value"),
			_ => unreachable!()
		}

		// queue async read
		let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
		overlapped.hEvent = self.event_read;
		// async read request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile
		if unsafe { ReadFile(
			self.comdev,
			buf.as_mut_ptr() as *mut c_void,
			buf.len() as DWORD,
			ptr::null_mut(),
			&mut overlapped
		)} == FALSE {
			let errcode = unsafe { GetLastError() };
			if errcode != ERROR_IO_PENDING {
				// release mutex and return original error on failure
				let _res = unsafe { ReleaseMutex(self.mutex_read) };
				debug_assert_ne!(_res, 0);
				return Err(io::Error::from_raw_os_error(errcode as i32));
			}
		}

		// wait for completion
		let mut len: DWORD = 0;
		if unsafe {
			// https://docs.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult
			GetOverlappedResult(self.comdev, &mut overlapped, &mut len, FALSE)
		} == FALSE {
			// release mutex and return original error on failure
			let error = io::Error::last_os_error();
			let _res = unsafe { ReleaseMutex(self.mutex_read) };
			debug_assert_ne!(_res, 0);
			return Err(error);
		}

		// release mutex
		let _res = unsafe { ReleaseMutex(self.mutex_read) };
		debug_assert_ne!(_res, 0);

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
		overlapped.hEvent = self.event_write;
		// async write request can (theoretically) succeed immediately, queue
		// successfully, or fail. even if it returns TRUE, the number of bytes
		// written should be retrieved via GetOverlappedResult().
		// https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-writefile
		if unsafe { WriteFile(
			self.comdev,
			buf.as_ptr() as *const c_void,
			buf.len() as DWORD,
			ptr::null_mut(),
			&mut overlapped
		)} == FALSE {
			let errcode = unsafe { GetLastError() };
			if errcode != ERROR_IO_PENDING {
				return Err(io::Error::from_raw_os_error(errcode as i32));
			}
		}

		// wait for completion
		// https://docs.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult
		let mut len: DWORD = 0;
		if unsafe { GetOverlappedResult(
			self.comdev,
			&mut overlapped,
			&mut len,
			TRUE
		)} == FALSE {
			// WriteFile() may fail with ERROR_SEM_TIMEOUT, which is not
			// io::ErrorKind::TimedOut prior to Rust 1.46, so create a custom
			// error with kind TimedOut to simplify subsequent error handling.
			// https://github.com/rust-lang/rust/pull/71756
			let errcode = unsafe { GetLastError() };
			if_rust_version! { < 1.46 {
				if errcode == ERROR_SEM_TIMEOUT {
					return Err(io::Error::new(io::ErrorKind::TimedOut,
						"WriteFile() timed out (ERROR_SEM_TIMEOUT)"));
				}
			}}
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
		// close all handles
		// https://docs.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-closehandle
		let _res = unsafe { CloseHandle(self.comdev) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.event_read) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.event_write) };
		debug_assert_ne!(_res, 0);
		let _res = unsafe { CloseHandle(self.mutex_read) };
		debug_assert_ne!(_res, 0);
	}
}
