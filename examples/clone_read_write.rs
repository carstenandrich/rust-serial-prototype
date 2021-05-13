// Copyright 2021 Carsten Andrich <base64decode("Y2Fyc3Rlblx4NDBhbmRyaWNoXHgyZW5hbWU=")>
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
// by opening a COM port with 10 millisecond read/write timeout and reading/
// writing a couple of times.

extern crate serial;

use std::env;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};
use serial::SerialPort;

fn main() {
	let args: Vec<String> = env::args().collect();
	if args.len() != 2 {
		println!("Usage: {} COMn", args[0]);
		return;
	}

	// open port with small timeout duration to get timeouts below
	let mut com = SerialPort::open(
			&args[1], Some(Duration::from_millis(10))
		).expect("Opening COM port failed");
	let mut com_clone = com.try_clone().expect("Cloning COM port failed");

	let t = thread::spawn(move || {
		println!("{:?}", Instant::now());
		let mut buf = [0u8; 1024];
		for _ in 0..50 {
			let res = com.read(&mut buf);
			println!("< {:?} {:?}", Instant::now(), res);
		}
	});

	println!("\n{:?}", Instant::now());
	let buf = [0u8; 1024];
	for _ in 0..20 {
		let res = com_clone.write(&buf);
		println!("> {:?} {:?}", Instant::now(), res);
	}

	t.join().unwrap();
}
