// WARNING: THIS PROGRAM WILL WRITE A BUNCH OF NULL CHARACTERS (0x00) ON A
//          SERIAL PORT OF YOUR CHOICE. DO **NOT** USE IT ON ANY SERIAL PORT
//          UNLESS YOU'RE **ABSOLUTELY** SURE THAT THIS WILL NOT HAVE ANY
//          UNDESIRED EFFECTS ON THAT PORT OR DEVICES ACCESSED VIA IT.

extern crate serial;

use std::env;
use std::io::{Read, Write};
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

	// open serial port and move into Arc
	let ser = SerialPort::open(
			&args[1], Some(Duration::from_millis(1000))
		).expect("opening serial port failed");
	let ser = Arc::new(ser);

	let start = Instant::now();
	let mut threads = Vec::new();

	// read from serial port in separate threads
	for n in 0..2 {
		let ser_clone = ser.clone();
		threads.push(thread::spawn(move || {
			let mut buf = [0u8; 1024];
			for _ in 0..NUM_IO_OPS {
				let res = (&*ser_clone).read(&mut buf);
				println!("<{} {:?} {:?}", n, start.elapsed(), res);
			}
		}));
	}

	// write to serial port in separate threads
	for n in 0..2 {
		let ser_clone = ser.clone();
		threads.push(thread::spawn(move || {
			let buf = [0u8; 1024];
			for _ in 0..NUM_IO_OPS {
				let res = (&*ser_clone).write(&buf);
				println!(">{} {:?} {:?}", n, start.elapsed(), res);
			}
		}));
	}

	// join all spawned threads
	for t in threads {
		t.join().unwrap();
	}
}
