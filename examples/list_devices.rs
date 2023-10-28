extern crate serial;

use std::io;
use serial::SerialPort;

fn main() -> io::Result<()> {
	println!("Available DEVICEs: {:?}", SerialPort::list_devices());
	Ok(())
}
