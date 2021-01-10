extern crate libc;
#[cfg(target_os = "linux")]
extern crate udev;

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::FromRawFd;
use std::time::Duration;

use libc::{O_RDWR, O_NOCTTY, TIOCEXCL};

// Linux-specific imports and definitions required for TIOCGEXCL ioctl, which
// is not exported by libc crate
#[cfg(target_os = "linux")]
use libc::{c_int, c_ulong};
#[cfg(target_os = "linux")]
const TIOCGEXCL: c_ulong = 0x80045440;

pub struct SerialPort {
	fh: File
}

impl SerialPort {
	pub fn open<T>(dev_path: &T, timeout: Option<Duration>) -> io::Result<Self>
			where T: AsRef<OsStr> + ?Sized {
		let dev_cstr = CString::new(dev_path.as_ref().as_bytes()).unwrap();
		let fd = unsafe { libc::open(dev_cstr.as_ptr(), O_RDWR | O_NOCTTY, 0) };
		if fd < 0 {
			return Err(io::Error::last_os_error());
		}

		// requesting exclusive TTY access via TIOCEXCL below is insufficient to
		// avoid simultaneous access by users with CAP_SYS_ADMIN, which allows
		// to bypass TIOCEXCL. Linux offers the TIOCGEXCL ioctl to check whether
		// TIOCEXCL is set for the TTY associated with an fd. therefore, if this
		// is Linux, use TIOGEXCL to check whether TIOCEXCL is set and return
		// EBUSY, if so.
		// https://manpages.debian.org/unstable/manpages-dev/tty_ioctl.4.en.html#Exclusive_mode
		if cfg!(target_os = "linux") {
			let mut arg: c_int = 0;
			if unsafe { libc::ioctl(fd, TIOCGEXCL, &mut arg) } == 0 {
				if arg > 0 {
					return Err(io::Error::new(io::ErrorKind::Other,
						"Device or resource busy"));
				}
			}
		}

		// get exclusive TTY access
		// http://man7.org/linux/man-pages/man4/tty_ioctl.4.html
		if unsafe { libc::ioctl(fd, TIOCEXCL) } < 0 {
			return Err(io::Error::last_os_error());
		}

		// compute timeout in tenths of second
		let timeout_decis: u8 = match timeout {
			None => 0,
			Some(dur) if dur < Duration::from_millis(100) => 1,
			Some(dur) if dur > Duration::from_millis(25500) => 255,
			Some(dur) => {
				(dur.as_secs()       *  10) as u8 +
				(dur.subsec_millis() / 100) as u8
			}
		};

		// set raw mode, speed, and timeout settings, see:
		// http://man7.org/linux/man-pages/man3/termios.3.html
		// FIXME: the highest speed supported by POSIX-compliant termios is
		//        38400 baud, which may be insufficient for high measurement
		//        and/or navigation rates in combination with a large set of
		//        enabled output messages. Linux appears to ignore the baud
		//        rate setting for CDC devices, but this is not guaranteed for
		//        other POSIX systems.
		// TODO: check the linux source, whether the baud rate setting is
		//       actually being ignored or some other voodoo happens:
		//       https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/usb/class/cdc-acm.c?h=linux-5.6.y#n1052
		let mut termios: libc::termios = unsafe { mem::zeroed() };
		termios.c_cflag = libc::B38400 | libc::CS8 | libc::CLOCAL | libc::CREAD;
		// configure "read with timeout" behavior if timeout is given or
		// "blocking read" if not. we do not want "read with interbyte timeout".
		termios.c_cc[libc::VMIN] = if timeout_decis == 0 { 1 } else { 0 };
		termios.c_cc[libc::VTIME] = timeout_decis;
		if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } < 0 {
			return Err(io::Error::last_os_error());
		}

		Ok(Self { fh: unsafe { File::from_raw_fd(fd) }})
	}

	#[cfg(not(target_os = "linux"))]
	pub fn list_devices() -> Vec<OsString> {
		unimplemented!("Enumerating serial devices is only supported on Linux and Windows");
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
		Ok(Self { fh: self.fh.try_clone()? })
	}
}

impl io::Read for SerialPort {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		self.fh.read(buf)
	}
}

impl io::Write for SerialPort {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		self.fh.write(buf)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.fh.flush()
	}
}
