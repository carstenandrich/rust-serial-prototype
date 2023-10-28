#[cfg(unix)]
mod unix;

#[cfg(all(windows, not(feature = "experimental")))]
mod windows;

#[cfg(all(windows, feature = "experimental"))]
mod windows_experimental;

#[cfg(unix)]
pub use unix::*;

#[cfg(all(windows, not(feature = "experimental")))]
pub use windows::*;

#[cfg(all(windows, feature = "experimental"))]
pub use windows_experimental::*;

#[cfg(not(any(unix, windows)))]
compile_error!("This crate supports Unix and Windows only.");
