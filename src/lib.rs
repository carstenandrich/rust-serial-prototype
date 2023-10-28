#[cfg(windows)]

use std::ffi::OsStr;
use std::io;
use std::time::Duration;

mod sys;

pub struct SerialPort(sys::SerialPort);

impl SerialPort {
	pub fn open<T>(dev_path: &T, timeout: Option<Duration>) -> io::Result<Self>
			where T: AsRef<OsStr> + ?Sized {
		sys::SerialPort::open(dev_path, timeout).map(Self)
	}

	pub fn try_clone(&self) -> io::Result<Self> {
		self.0.try_clone().map(Self)
	}
}

impl io::Read for SerialPort {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		self.0.read(buf)
	}
}

impl io::Read for &SerialPort {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		self.0.read(buf)
	}
}

impl io::Write for SerialPort {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		self.0.write(buf)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.0.flush()
	}
}

impl io::Write for &SerialPort {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		self.0.write(buf)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.0.flush()
	}
}
