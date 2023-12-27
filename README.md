# Prototype for Platform-Independent Serial Port Rust Crate

**TL;DR: This is experimental code. Use at your own risk!**

This repository contains a work-in-progress prototype for a platform-independent serial port library written in Rust.
Currently, it's a playpen to determine a straightforward [`std::io::TcpStream`](https://doc.rust-lang.org/std/net/struct.TcpStream.html)-like API along with implementations that perform (almost) identically on Linux and Windows.


# Feature Comparison

Comparison table of features supported by this implementation and other serial port crates.
Please create a pull request to fix inaccuracies or add more relevant features.
Each feature is described in detail below the table.

Feature \ Crate                        | this | `serial` | `serial2` | `serialport`
-------------------------------------- | ---- | -------- | --------- | ------------
Portable read timeouts                 | ✅   | ❌       | ✅        | ❌
Reliable locking (on POSIX)            | ✅   | ❌       | ❌        | ❌
`impl std::io::{Read,Write}`           | ✅   | ✅       | ✅        | ✅
Concurrent I/O (on Windows)            | ✅   | ❌       | ✅        | ❌
`read(&self, …)` and `write(&self, …)` | ❌   | ❌       | ✅        | ❌
`.try_clone()`                         | ✅   | ❌       | ❌        | ✅
`impl std::io::{Read,Write} for &Self` | ✅   | ❌       | ❌        | ❌
???                                    | ❓   | ❓       | ❓        | ❓

## Portable read timeouts

Crate implements portable read timeouts that behave similarly on POSIX and Windows.

[POSIX-compliant `poll()`](https://pubs.opengroup.org/onlinepubs/9699919799/functions/poll.html) always returns immediately if data can be read.
It only waits for the timeout to expire while zero byte can be read from the serial port.
The author considers this the most desirable behavior, as it enables efficient yet low-latency parsing of arbitrarily sized data.

Windows serial port timeout settings configured via the [`COMMTIMEOUTS` structure](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts) are more complicated.
As currently implemented by the [`serial`](https://github.com/dcuddeback/serial-rs/blob/v0.4.0/serial-windows/src/com.rs#L170) and [`serialport`](https://github.com/serialport/serialport-rs/blob/v4.2.2/src/windows/com.rs#L245) crates, a read will only return before the specified timeout expires if and only if the user supplied buffer is full.
I.e., if a single unused byte is remaining in the buffer, the read will return stale data only when the timeout expires.
Obviously, this behavior is suboptimal as it either incurs large latencies or requires inefficiently low timeouts.
Additionally, it impedes portability, as POSIX and Windows behave very differently.

### Fix and Known Issue

Windows supports [`COMMTIMEOUTS` values (see "Remarks")](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-commtimeouts#remarks) that yield low-latency, POSIX-like behavior, i.e., return data as soon as it becomes available.
Unfortunately, these settings also result in splitting reads into two syscalls with the first only returning a single byte (the second bullet point in the "Remarks" section of above link is meant literally).

A workaround to this could be possible via use of [`WaitCommEvent()`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-waitcommevent) (see [`src/sys/windows_experimental.rs`](./src/sys/windows_experimental.rs)), however that approach leads to a significant increase in implementation complexity and the author's workaround implementation is currently broken.

## Reliable locking (on POSIX)

Crate ensures mutually exclusive serial port access via realiable locking.
Windows enforces this by default (a COM port can only be opened once).

On POSIX locking must be implemented explicitly.
On Linux `libc::ioctl(fd, libc::TIOCEXCL)` is sufficient for non-privileged users, however `CAP_SYS_ADMIN` implicitly bypasses `TIOCEXCL`.
For these privileged users, an additional `libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)` is necessary.

## `impl std::io::{Read,Write}`

Crate implements Rust's [`std::io::{Read,Write}` traits](https://doc.rust-lang.org/std/io/index.html) for read/write operations.
This enables interoperability with anything that relies on these standard I/O traits.
A noteworthy example is [`std::io::BufReader`](https://doc.rust-lang.org/std/io/struct.BufReader.html).
Via [`std::io::BufRead::lines()`](`https://doc.rust-lang.org/std/io/trait.BufRead.html#method.lines`) this feature enables straightforward and portable line processing:

```
let mut serial = SerialPort::open("/dev/ttyACM0").unwrap();
let mut reader = BufReader::new(serial);
loop {
    let mut line = String::new();
    let len = reader.read_line(&mut line).unwrap();
};
```

## Concurrent I/O (on Windows)

Crate supports concurrent I/O operations, e.g., simultaneous `read()` and `write()` from separate threads.
While this is trivial on POSIX, the Windows API requires the use of [overlapped I/O](https://learn.microsoft.com/en-us/windows/win32/Sync/synchronization-and-overlapped-input-and-output) functions.
Without overlapped I/O, a `read()` that is currently blocked for lack of input will also block a subsequently issued `write()` until the `read()` returns.

## `read(&self, …)` and `write(&self, …)`

Crate implements independent `read(&self, …)` and `write(&self, …)` methods taking `&self` instead of `&mut self`.
While not implementing the `std::io::{Read,Write}` traits (see above for benefits), this enables straightforward multi-threading.

## `.try_clone()`

Crate supports cloning `self` by duplicating the backing file descriptor (POSIX) or handle (Windows).
Enables simultaneous use of the underlying serial port from multiple threads (most useful for independent read/write operations).


## `impl std::io::{Read,Write} for &Self`

Crate implements `std::io::Read` and `std::io::Write` not just for `Self` but also for `&Self` like [`std::net::TcpStream` does](https://doc.rust-lang.org/std/net/struct.TcpStream.html#impl-Read-for-%26TcpStream).
Enables using a non-mutable reference for `read()` and `write()`, which allows simultaneous I/O operations from multiple threads *without* cloning `self`.

Example using scoped thread:

```
let mut serial = SerialPort::open("/dev/ttyACM0").unwrap();
thread::scope(|s| {
	let mut serial_ref = &serial;
	s.spawn(move || {
		serial_ref.read(&mut [0u8; 64]).unwrap();
	});
	serial.read(&mut [0u8; 64]).unwrap();
}
```

Example using `std::sync::Arc`:

```
let mut serial_arc = Arc::new(SerialPort::open("/dev/ttyACM0").unwrap());
let serial_arc_moved = serial_arc.clone();
let t = thread::spawn(move || {
	serial_arc_moved.as_ref().read(&mut [0u8; 64]).unwrap();
});
serial_arc.as_ref().read(&mut [0u8; 64]).unwrap();
t.join().unwrap();
```


# Misc Developer Information

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
