//! Win32 原始层：窗口、绘制、进程初始化。
//!
//! 刻意只依赖 `windows-sys`（纯 FFI 声明），不使用任何 GUI 框架，
//! 目的是让链接产物尽可能小、进程初始化路径尽可能短。

pub mod init;
pub mod paint;
pub mod wnd;

use windows_sys::Win32::Foundation::HWND;

/// UTF-8 → 以 NUL 结尾的 UTF-16 缓冲。返回的 Vec 必须比用它的调用活得久。
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 不带结尾 NUL 的宽字符缓冲（用于需要显式长度的 API）。
pub fn to_wide_raw(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 从定长 UTF-16 缓冲（NUL 结尾）转回 String。
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 从指针读 NUL 结尾宽串。调用方保证指针有效。
///
/// # Safety
/// `p` 必须指向一块以 NUL 结尾的有效 UTF-16 内存。
pub unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

/// 取主模块句柄。冷启动时这是最便宜的方式（不查磁盘）。
pub fn module_handle() -> *mut core::ffi::c_void {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    unsafe { GetModuleHandleW(std::ptr::null()) }
}

/// 判断窗口句柄是否有效。
pub fn hwnd_valid(h: HWND) -> bool {
    !h.is_null()
}
