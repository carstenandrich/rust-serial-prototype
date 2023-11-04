extern crate libc;
#[cfg(target_os = "linux")]
extern crate udev;

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

use libc::{c_int, c_void, INT_MAX};

pub struct SerialPort {
	fd: c_int,
	timeout_read: Option<Duration>,
	timeout_write: Option<Duration>
}

const TTY_FLAGS: c_int = libc::O_RDWR
                       | libc::O_CLOEXEC
                       | libc::O_NOCTTY
                       | libc::O_NONBLOCK;

impl SerialPort {
	pub fn open<T>(dev_path: &T, timeout: Option<Duration>) -> io::Result<Self>
			where T: AsRef<OsStr> + ?Sized {
		let dev_cstr = CString::new(dev_path.as_ref().as_bytes()).unwrap();
		let fd = unsafe { libc::open(dev_cstr.as_ptr(), TTY_FLAGS, 0) };
		if fd < 0 {
			return Err(io::Error::last_os_error());
		}

		// get exclusive TTY access
		// http://man7.org/linux/man-pages/man4/tty_ioctl.4.html
		if unsafe { libc::ioctl(fd, libc::TIOCEXCL) } != 0 {
			return Err(io::Error::last_os_error());
		}

		// requesting exclusive TTY access via TIOCEXCL above is insufficient to
		// avoid simultaneous access by users with CAP_SYS_ADMIN, which allows
		// to bypass TIOCEXCL. therefore, use flock() to place an additional
		// exclusive advisory lock on the TTY device.
		// https://stackoverflow.com/questions/49636520/how-do-you-check-if-a-serial-port-is-open-in-linux/49687230#49687230
		// https://stackoverflow.com/questions/30316722/what-is-the-best-practice-for-locking-serial-ports-and-other-devices-in-linux/34937038#34937038
		// https://man7.org/linux/man-pages/man2/flock.2.html
		if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
			// TODO: indicate that file is locked on EWOULDBLOCK
			return Err(io::Error::last_os_error());
		}

		// set raw mode, speed, and timeout settings ("polling read"), see:
		// http://man7.org/linux/man-pages/man3/termios.3.html
		let mut termios: libc::termios = unsafe { mem::zeroed() };
		termios.c_cflag = libc::B38400 | libc::CS8 | libc::CLOCAL | libc::CREAD;
		if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
			return Err(io::Error::last_os_error());
		}

		Ok(Self {
			fd,
			timeout_read: timeout,
			timeout_write: timeout
		})
	}

	#[cfg(not(target_os = "linux"))]
	pub fn list_devices() -> Vec<OsString> {
		unimplemented!("Enumerating serial devices is only supported on Linux");
	}

	#[cfg(target_os = "linux")]
	pub fn list_devices() -> Vec<OsString> {
		let mut devices: Vec<OsString> = Vec::new();

		// iterate over all TTY devices
		let mut enumerator = udev::Enumerator::new().unwrap();
		enumerator.match_subsystem("tty").unwrap();
		for device in enumerator.scan_devices().unwrap() {
			// skip this device if it doesn't have a device name (e.g. /dev/ttyACM0)
			let devname = match device.property_value("DEVNAME") {
				Some(id_model) => id_model,
				None => continue
			};

			// add to device list
			devices.push(devname.to_os_string());
		}

		devices
	}

	pub fn try_clone(&self) -> io::Result<Self> {
		// duplicate file descriptor (F_DUPFD_CLOEXEC requires POSIX.1-2008)
		let fd = unsafe { libc::fcntl(self.fd, libc::F_DUPFD_CLOEXEC, 0) };
		if fd < 0 {
			return Err(io::Error::last_os_error());
		}

		// set TTY flags for duplicate fd. tries to set file creation flags that
		// are silently ignored.
		if unsafe { libc::fcntl(fd, libc::F_SETFL, TTY_FLAGS) } != 0 {
			let error = io::Error::last_os_error();

			let _res = unsafe { libc::close(fd) };
			debug_assert_ne!(_res, 0);

			return Err(error);
		}

		Ok(Self {
			fd,
			timeout_read: self.timeout_read,
			timeout_write: self.timeout_write
		})
	}

	pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
		let mut pollfd = libc::pollfd {
			fd: self.fd,
			events: libc::POLLIN,
			revents: 0
		};

		let entry = Instant::now();
		loop {
			// compute read timeout in ms, accounting for time already elapsed
			let elapsed = entry.elapsed();
			let timeout_ms: c_int = match self.timeout_read {
				None => -1,
				Some(timeout) if elapsed > timeout => {
					return Err(io::Error::new(io::ErrorKind::TimedOut,
						"reading from TTY timed out"));
				},
				Some(timeout) if timeout - elapsed <= Duration::from_millis(1) => 1,
				Some(timeout) if timeout - elapsed >= Duration::from_millis(INT_MAX as u64) => INT_MAX,
				Some(timeout) => (timeout - elapsed).as_millis() as c_int
			};

			// block until data is available or timeout occurs
			match unsafe { libc::poll(&mut pollfd, 1, timeout_ms) } {
				-1 => return Err(io::Error::last_os_error()),
				0 => return Err(io::Error::new(io::ErrorKind::TimedOut,
						"reading from TTY timed out")),
				_ => ()
			}

			// on Linux poll() sets POLLERR and POLLHUP if tty disappears
			if pollfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
				return Err(io::Error::new(io::ErrorKind::UnexpectedEof,
					"TTY was closed or disconnected"));
			}

			// try to read() from tty. if multiple threads poll() in parallel,
			// they are released simultaneously and race for the read(), which
			// will likely succeed only on one thread.
			let len = unsafe {
				libc::read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len())
			};
			debug_assert!(len <= buf.len() as isize);
			match len {
				// POSIX allows read() to return either 0 or -1 with EAGAIN if
				// no data is available, so handle both options as such, see:
				// https://man7.org/linux/man-pages/man3/termios.3.html
				-1 => {
					let error = io::Error::last_os_error();
					if error.kind() != io::ErrorKind::WouldBlock {
						return Err(error);
					}
				},
				0 if buf.len() == 0 => return Ok(0),
				0 => (),
				_ => return Ok(len as usize)
			}
		}
	}

	pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
		let mut pollfd = libc::pollfd {
			fd: self.fd,
			events: libc::POLLOUT,
			revents: 0
		};

		let entry = Instant::now();
		loop {
			// compute write timeout in ms, accounting for time already elapsed
			let elapsed = entry.elapsed();
			let timeout_ms: c_int = match self.timeout_write {
				None => -1,
				Some(timeout) if elapsed > timeout => {
					return Err(io::Error::new(io::ErrorKind::TimedOut,
						"writing to TTY timed out"));
				},
				Some(timeout) if timeout - elapsed <= Duration::from_millis(1) => 1,
				Some(timeout) if timeout - elapsed >= Duration::from_millis(INT_MAX as u64) => INT_MAX,
				Some(timeout) => (timeout - elapsed).as_millis() as c_int
			};

			// block until tty becomes writable or timeout occurs
			match unsafe { libc::poll(&mut pollfd, 1, timeout_ms) } {
				-1 => return Err(io::Error::last_os_error()),
				0 => return Err(io::Error::new(io::ErrorKind::TimedOut,
						"writing to TTY timed out")),
				_ => ()
			}

			// on Linux poll() sets POLLERR and POLLHUP if tty disappears
			if pollfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
				return Err(io::Error::new(io::ErrorKind::UnexpectedEof,
					"TTY was closed or disconnected"));
			}

			// try to write() to tty. if multiple threads poll() in parallel,
			// they are released simultaneously and race for the write(), which
			// may not succeed on all threads if the TTY's output buffer is
			// full.
			let len = unsafe {
				libc::write(self.fd, buf.as_ptr() as *const c_void, buf.len())
			};
			debug_assert!(len <= buf.len() as isize);
			match len {
				-1 => {
					let error = io::Error::last_os_error();
					if error.kind() != io::ErrorKind::WouldBlock {
						return Err(error);
					}
				},
				0 if buf.len() == 0 => return Ok(0),
				// FIXME: does len == 0 indicate timeout just like for read()?
				0 => (),
				_ => return Ok(len as usize)
			}
		}
	}

	pub fn flush(&self) -> io::Result<()> {
		match unsafe { libc::fsync(self.fd) } {
			-1 => Err(io::Error::last_os_error()),
			0 => Ok(()),
			_ if cfg!(debug_assertions) => panic!("fsync() returned invalid value"),
			_ => unreachable!()
		}
	}
}

impl Drop for SerialPort {
	fn drop(&mut self) {
		let _res = unsafe { libc::close(self.fd) };
		debug_assert_eq!(_res, 0);
	}
}
