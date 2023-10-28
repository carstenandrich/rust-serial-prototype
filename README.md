# Prototype for Platform-Independent Serial Port Rust Crate

**TL;DR: This is experimental code. Use at your own risk!**

This repository contains a work-in-progress prototype for a platform-independent serial port library written in Rust.
Currently, it's a playpen to determine a straightforward [`std::io::TcpStream`](https://doc.rust-lang.org/std/net/struct.TcpStream.html)-like API along with implementations that perform identically on Linux and Windows.

This repository does not intend to be become an independent crate, but aims to be upstreamed into an overhauled [serialport crate](https://crates.io/crates/serialport).

Targeted features include:

  * [POSIX-like timeout behavior on Windows](https://gitlab.com/susurrus/serialport-rs/-/merge_requests/78)
  * Overlapped I/O for parallel read/write on Windows

## Win32 API Reference and Examples

  * <https://stackoverflow.com/questions/20183510/wait-for-data-on-com-port>
  * <https://learn.microsoft.com/en-us/previous-versions/ff802693(v=msdn.10)?redirectedfrom=MSDN> (<https://msdn.microsoft.com/en-us/library/ff802693.aspx>)
  * <https://docs.microsoft.com/en-us/windows/win32/devio/communications-events>
  * <https://docs.microsoft.com/en-us/windows/win32/devio/monitoring-communications-events>
  * <https://docs.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject>
  * <https://docs.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers>
  * <https://docs.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-comstat>
  * <https://docs.microsoft.com/en-us/previous-versions/ff802693(v=msdn.10)?redirectedfrom=MSDN#serial-status>
  * <https://docs.microsoft.com/en-us/windows/win32/sync/using-mutex-objects>
