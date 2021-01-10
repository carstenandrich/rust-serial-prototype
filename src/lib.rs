#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::SerialPort;
#[cfg(windows)]
pub use windows::SerialPort;

