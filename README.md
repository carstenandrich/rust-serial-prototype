# Prototype for Platform-Independent Serial Port Rust Crate

**TL;DR: This is experimental code. Use at your own risk!**

This repository contains a work-in-progress prototype for a platform-independent serial port library written in Rust.
Currently, it's a playpen to determine a straightforward [`std::io::TcpStream`](https://doc.rust-lang.org/std/net/struct.TcpStream.html)-like API along with implementations that perform identically on Linux and Windows.

This repository does not intend to be become an independent crate, but aims to be upstreamed into an overhauled [serialport crate](https://crates.io/crates/serialport).

Targeted features include:

  * [POSIX-like timeout behavior on Windows](https://gitlab.com/susurrus/serialport-rs/-/merge_requests/78)
  * Overlapped I/O for parallel read/write on Windows
