extern crate serial;

use std::env;
use std::io::Read;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use serial::SerialPort;

const NUM_IO_OPS: usize = 50;

fn main() {
	let args: Vec<String> = env::args().collect();
	if args.len() != 2 {
		#[cfg(unix)]
		println!("Usage: {} /dev/ttyN", args[0]);
		#[cfg(windows)]
		println!("Usage: {} COMn", args[0]);
		return;
	}

	// open serial port and clone once
	let ser = SerialPort::open(
			&args[1], Some(Duration::from_millis(1000))
		).expect("opening serial port failed");
	let mut ser_clone = ser.try_clone().expect("cloning serial port failed");

	let start = Instant::now();
	let mut threads = Vec::new();

	// read from cloned serial port in separate thread
	threads.push(thread::spawn(move || {
		let mut buf = [0u8; 1024];
		for _ in 0..NUM_IO_OPS {
			let res = ser_clone.read(&mut buf);
			println!("<C {:?} {:?}", start.elapsed(), res);
		}
	}));

	// move original serial port into Arc
	let ser = Arc::new(ser);

	// read from original serial port in separate thread
	let ser_clone = ser.clone();
	threads.push(thread::spawn(move || {
		let mut buf = [0u8; 1024];
		for _ in 0..NUM_IO_OPS {
			let res = (&*ser_clone).read(&mut buf);
			println!("<R {:?} {:?}", start.elapsed(), res);
		}
	}));

	// read from original serial port in main thread
	let mut buf = [0u8; 1024];
	for _ in 0..NUM_IO_OPS {
		let res = (&*ser).read(&mut buf);
		println!("<M {:?} {:?}", start.elapsed(), res);
	}

	// join all spawned threads
	for t in threads {
		t.join().unwrap();
	}
}
