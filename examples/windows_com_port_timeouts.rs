// Copyright 2020 Carsten Andrich <base64decode("Y2Fyc3Rlblx4NDBhbmRyaWNoXHgyZW5hbWU=")>
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

// WARNING: THIS PROGRAM WILL WRITE A BUNCH OF NULL CHARACTERS (0x00) ON A
//          WINDOWS COM PORT OF YOUR CHOICE. DO **NOT** USE IT ON ANY COM PORT
//          UNLESS YOU'RE **ABSOLUTELY** SURE THAT THIS WILL NOT HAVE ANY
//          UNDESIRED EFFECTS ON THAT PORT OR DEVICES ACCESSED VIA IT.
//
// This example illustrates the Windows COM port read/write timeout behavior
// by opening a COM port with 1 millisecond read/write timeout and reading/
// writing a couple of times.
//
// According to this document, read/write timeouts are not considered errors:
// https://docs.microsoft.com/en-us/windows/win32/devio/time-outs
// > It is not treated as an error when a time-out occurs during a read or
// > write operation (that is, the read or write function's return value
// > indicates success). The count of bytes actually read or written is
// > reported by ReadFile or WriteFile
//
// This is true for ReadFile() on COM ports, which return TRUE and sets
// lpNumberOfBytesRead to 0 if a timeout occurs. However, WriteFile() returns
// FALSE and GetLastError() indicates ERR_SEM_TIMEOUT (121) on timeout (tested
// on Windows 10 version 1809).

// FIXME: build fails on non-windows platforms for lack of fn main()
#![cfg(windows)]

extern crate windows_sys;

use std::ffi::{c_void, OsStr};
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::time::Duration;

use windows_sys::{
	Win32::Devices::Communication::*,
	Win32::Foundation::*,
	Win32::Storage::FileSystem::*,
	Win32::System::WindowsProgramming::*
};

const MAXDWORD: u32 = u32::MAX;

pub struct SerialPort {
	handle: HANDLE
}

impl SerialPort {
	pub fn open<T>(port_name: &T, timeout: Option<Duration>) -> io::Result<Self>
			where T: AsRef<OsStr> + ?Sized {
		// construct prefixed COM port name to support COMn with n > 9
		let mut name = Vec::<u16>::new();
		name.extend(OsStr::new("\\\\.\\").encode_wide());
		name.extend(port_name.as_ref().encode_wide());
		name.push(0);

		// open COM port as raw HANDLE
		let handle = unsafe {
			CreateFileW(name.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0,
				ptr::null_mut(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, 0 as HANDLE)
		};
		if handle == INVALID_HANDLE_VALUE {
			return Err(io::Error::last_os_error());
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
		if unsafe { SetCommState(handle, &mut dcb) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(handle) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		// populate COMMTIMEOUTS struct from Option<Duration>
		// https://docs.microsoft.com/en-us/windows/win32/devio/time-outs
		// https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts
		let mut timeouts = if let Some(dur) = timeout {
			let mut dur_ms = dur.as_secs() * 1000
			           + dur.subsec_millis() as u64;

			// clip dur_ms to valid range from 1 to MAXDWORD
			if dur_ms < 1 {
				dur_ms = 1;
			} else if dur_ms > MAXDWORD as u64 {
				dur_ms = MAXDWORD as u64;
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
			COMMTIMEOUTS {
				ReadIntervalTimeout: 0,
				ReadTotalTimeoutMultiplier: 0,
				ReadTotalTimeoutConstant: 0,
				WriteTotalTimeoutMultiplier: 0,
				WriteTotalTimeoutConstant: 0,
			}
		};

		// set timouts
		if unsafe { SetCommTimeouts(handle, &mut timeouts) } == 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { CloseHandle(handle) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		Ok(Self { handle })
	}
}

impl Drop for SerialPort {
	fn drop(&mut self) {
		// https://docs.microsoft.com/de-de/windows/win32/api/handleapi/nf-handleapi-closehandle
		let _res = unsafe { CloseHandle(self.handle) };
		debug_assert_ne!(_res, 0);
	}
}

impl io::Read for SerialPort {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let mut len: u32 = 0;
		let res: BOOL = unsafe {
			ReadFile(self.handle, buf.as_mut_ptr() as *mut c_void,
				buf.len() as u32, &mut len, ptr::null_mut())
		};

		match res {
			0 => Err(io::Error::last_os_error()),
			_ if len == 0 => Err(io::Error::new(io::ErrorKind::TimedOut,
					"ReadFile() returned TRUE with 0 bytes read")),
			_ => Ok(len as usize)
		}
	}
}

impl io::Write for SerialPort {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let mut len: u32 = 0;
		let res: BOOL = unsafe {
			WriteFile(self.handle, buf.as_ptr(),
				buf.len() as u32, &mut len, ptr::null_mut())
		};

		match res {
			0 => Err(io::Error::last_os_error()),
			_ if len == 0 => Err(io::Error::new(io::ErrorKind::TimedOut,
					"WriteFile() returned TRUE with 0 bytes written")),
			_ => Ok(len as usize)
		}
	}

	fn flush(&mut self) -> io::Result<()> {
		match unsafe { FlushFileBuffers(self.handle) } {
			0 => Err(io::Error::last_os_error()),
			_ => Ok(()),
		}
	}
}

use std::env;
use std::io::{Read, Write};
use std::time::Instant;

fn main() {
	let args: Vec<String> = env::args().collect();
	if args.len() != 2 {
		println!("Usage: {} COMn", args[0]);
		return;
	}

	// open port with smallest possible timeout duration to get timeouts below
	let mut com = SerialPort::open(
			&args[1], Some(Duration::from_millis(1))
		).expect("Opening COM port failed");

	println!("{:?}", Instant::now());
	let mut buf = [0u8; 1024];
	for _ in 0..50 {
		let res = com.read(&mut buf);
		println!("{:?} {:?}", Instant::now(), res);
	}

	println!("\n{:?}", Instant::now());
	let buf = [0u8; 1024];
	for _ in 0..20 {
		let res = com.write(&buf);
		println!("{:?} {:?}", Instant::now(), res);
	}
}
