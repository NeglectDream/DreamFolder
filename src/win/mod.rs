//! Win32 原始层：窗口、绘制、进程初始化。
//!
//! 刻意只依赖 `windows-sys`（纯 FFI 声明），不使用任何 GUI 框架，
//! 目的是让链接产物尽可能小、进程初始化路径尽可能短。

pub mod init;
pub mod paint;
pub mod wnd;
pub mod dragdrop;
pub mod droptarget;
pub mod shellmenu;
pub mod search;
pub mod gdiplus;
pub mod appicon;
pub mod shortcut;

use windows_sys::Win32::Foundation::HWND;

/// UTF-8 → 以 NUL 结尾的 UTF-16 缓冲。返回的 Vec 必须比用它的调用活得久。
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 不带结尾 NUL 的宽字符缓冲（用于需要显式长度的 API）。
pub fn to_wide_raw(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// 路径 → NUL 结尾宽串，必要时加上 `\\?\` 前缀以突破 MAX_PATH。
///
/// `\\?\` 会关掉系统对路径的规范化，所以只在确实有必要时加：
/// 已经带前缀、含 `..` 这类需要规范化的成分、或本身不长，都原样返回。
/// 调用方拿到的是普通路径时，行为与 `to_wide` 完全一致。
pub fn to_wide_path(p: &std::path::Path) -> Vec<u16> {
    let s = p.to_string_lossy();
    if s.starts_with(r"\\?\") || s.starts_with(r"\\.\") {
        return to_wide(&s);
    }
    // 按 UTF-16 码元计长（`\\?\` 的判定就是看码元数），并留出追加文件名的余量
    let units: usize = s.chars().map(|c| c.len_utf16()).sum();
    let needs_prefix = units >= 248
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if !needs_prefix {
        return to_wide(&s);
    }
    let normalized = s.replace('/', "\\");
    match normalized.strip_prefix(r"\\") {
        // UNC：\\server\share → \\?\UNC\server\share
        Some(rest) => to_wide(&format!(r"\\?\UNC\{rest}")),
        None if normalized.len() >= 3 && normalized.as_bytes()[1] == b':' => {
            to_wide(&format!(r"\\?\{normalized}"))
        }
        // 相对路径等：加了前缀反而更糟，交给系统自己处理
        None => to_wide(&s),
    }
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
