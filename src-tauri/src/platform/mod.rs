//! 应用需要操作系统做的事。每个平台一份实现（windows/、macos/），这里按当前系统导出。

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
