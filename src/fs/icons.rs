//! 图标与文件类型名缓存。
//!
//! 关键性能决策：对**文件**一律使用 `SHGFI_USEFILEATTRIBUTES`——
//! 让 Shell 只根据属性+扩展名返回图标，**完全不触碰磁盘**。
//! 真实路径查询只用于驱动器等极少数条目。
//!
//! 这是原软件冷启动慢的一个主要来源（逐文件 SHGetFileInfo），
//! 这里把它变成「按扩展名查一次，全目录复用」。

use std::collections::HashMap;
use std::path::Path;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW};

use super::entry::IconKey;
use crate::win::to_wide;

// SHGFI_* 标志（在本 crate 内自备，避免依赖 windows-sys 的常量导出差异）
const SHGFI_ICON: u32 = 0x0000_0100;
const SHGFI_LARGEICON: u32 = 0x0000_0000;
const SHGFI_SMALLICON: u32 = 0x0000_0001;
const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
const SHGFI_TYPENAME: u32 = 0x0000_0400;

const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

/// 图标尺寸档。UI 绘制时按需选档，避免把小图标拉大用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconSize {
    /// 16×16
    Small,
    /// 32×32（视图里的图标由此档按需缩放）
    Large,
}

/// 图标缓存。挂在 UI 状态上，随进程存活。
pub struct IconCache {
    small: HashMap<IconKey, isize>,
    large: HashMap<IconKey, isize>,
    /// 扩展名 → 类型名（"txt" → "文本文档"）
    type_names: HashMap<String, String>,
    /// 通用文件夹图标（一次查询，全程复用）
    folder_small: isize,
    folder_large: isize,
    /// 通用驱动器图标
    drive_small: isize,
    drive_large: isize,
}

impl IconCache {
    pub fn new() -> Self {
        IconCache {
            small: HashMap::with_capacity(64),
            large: HashMap::with_capacity(64),
            type_names: HashMap::with_capacity(64),
            folder_small: 0,
            folder_large: 0,
            drive_small: 0,
            drive_large: 0,
        }
    }

    /// 取图标句柄。**首次调用某扩展名时**才向 Shell 查询，之后全程命中缓存。
    pub fn get(&mut self, key: &IconKey, size: IconSize) -> *mut core::ffi::c_void {
        // 目录/驱动器走专用的一次性查询
        match key {
            IconKey::Folder => {
                return if size == IconSize::Small {
                    self.ensure_folder(IconSize::Small);
                    self.folder_small as *mut core::ffi::c_void
                } else {
                    self.ensure_folder(IconSize::Large);
                    self.folder_large as *mut core::ffi::c_void
                };
            }
            IconKey::Drive => {
                return if size == IconSize::Small {
                    self.ensure_drive(IconSize::Small);
                    self.drive_small as *mut core::ffi::c_void
                } else {
                    self.ensure_drive(IconSize::Large);
                    self.drive_large as *mut core::ffi::c_void
                };
            }
            IconKey::Pending => {
                // 未解析的按无扩展名处理
                return self.get(&IconKey::NoExt, size);
            }
            _ => {}
        }

        let table = if size == IconSize::Small {
            &mut self.small
        } else {
            &mut self.large
        };

        if let Some(&h) = table.get(key) {
            return h as *mut core::ffi::c_void;
        }

        let h = self.query_by_key(key, size);
        table.insert(key.clone(), h as isize);
        h
    }

    fn ensure_folder(&mut self, size: IconSize) {
        let filled = if size == IconSize::Small {
            self.folder_small != 0
        } else {
            self.folder_large != 0
        };
        if filled {
            return;
        }
        // 用一个真实存在的目录路径查询（%WINDIR% 一定在）
        let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let p = to_wide(&win);
        let h = unsafe {
            shell_icon(
                p.as_ptr(),
                FILE_ATTRIBUTE_DIRECTORY,
                size,
                false,
            )
        };
        if size == IconSize::Small {
            self.folder_small = h as isize;
        } else {
            self.folder_large = h as isize;
        }
    }

    fn ensure_drive(&mut self, size: IconSize) {
        let filled = if size == IconSize::Small {
            self.drive_small != 0
        } else {
            self.drive_large != 0
        };
        if filled {
            return;
        }
        let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let root = win.chars().take(3).collect::<String>(); // 例如 "C:\"
        let p = to_wide(&root);
        let h = unsafe {
            shell_icon(
                p.as_ptr(),
                FILE_ATTRIBUTE_DIRECTORY,
                size,
                false,
            )
        };
        if size == IconSize::Small {
            self.drive_small = h as isize;
        } else {
            self.drive_large = h as isize;
        }
    }

    /// 按扩展名查询图标（不访问磁盘）。
    fn query_by_key(&self, key: &IconKey, size: IconSize) -> *mut core::ffi::c_void {
        let (fake_name, attrs) = match key {
            IconKey::Ext(ext) => (format!("x.{ext}"), FILE_ATTRIBUTE_NORMAL),
            IconKey::NoExt => ("x".to_string(), FILE_ATTRIBUTE_NORMAL),
            _ => ("x".to_string(), FILE_ATTRIBUTE_NORMAL),
        };
        let w = to_wide(&fake_name);
        unsafe { shell_icon(w.as_ptr(), attrs, size, true) }
    }

    /// 取文件类型描述（"文本文档"），带缓存。
    pub fn type_name(&mut self, ext: &str) -> String {
        let key = ext.to_ascii_lowercase();
        if let Some(v) = self.type_names.get(&key) {
            return v.clone();
        }
        let name = if key.is_empty() {
            "文件".to_string()
        } else {
            let fake = to_wide(&format!("x.{key}"));
            unsafe { shell_type_name(fake.as_ptr()) }
        };
        self.type_names.insert(key, name.clone());
        name
    }

    /// 为目录补上类型描述。
    pub fn dir_type_name(&self) -> String {
        "文件夹".to_string()
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IconCache {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
        unsafe {
            for (_, h) in self.small.drain() {
                if h != 0 {
                    DestroyIcon(h as *mut core::ffi::c_void);
                }
            }
            for (_, h) in self.large.drain() {
                if h != 0 {
                    DestroyIcon(h as *mut core::ffi::c_void);
                }
            }
            for h in [
                self.folder_small,
                self.folder_large,
                self.drive_small,
                self.drive_large,
            ] {
                if h != 0 {
                    DestroyIcon(h as *mut core::ffi::c_void);
                }
            }
        }
    }
}

/// 向 Shell 要一个图标句柄。返回 0 表示失败。
///
/// # Safety
/// `path` 必须是 NUL 结尾的宽串指针。
unsafe fn shell_icon(
    path: *const u16,
    attrs: u32,
    size: IconSize,
    use_attributes: bool,
) -> *mut core::ffi::c_void {
    let mut info: SHFILEINFOW = std::mem::zeroed();
    let mut flags = SHGFI_ICON;
    flags |= if size == IconSize::Small {
        SHGFI_SMALLICON
    } else {
        SHGFI_LARGEICON
    };
    if use_attributes {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }

    let ret = SHGetFileInfoW(
        path,
        attrs,
        &mut info,
        std::mem::size_of::<SHFILEINFOW>() as u32,
        flags,
    );
    if ret == 0 || info.hIcon.is_null() {
        std::ptr::null_mut()
    } else {
        info.hIcon
    }
}

/// 取文件类型描述。
///
/// # Safety
/// `path` 必须是 NUL 结尾的宽串指针。
unsafe fn shell_type_name(path: *const u16) -> String {
    let mut info: SHFILEINFOW = std::mem::zeroed();
    let ret = SHGetFileInfoW(
        path,
        FILE_ATTRIBUTE_NORMAL,
        &mut info,
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
    );
    if ret == 0 {
        return "文件".to_string();
    }
    crate::win::from_wide(&info.szTypeName)
}

/// 从完整路径推断图标键。
///
/// 这是 UI 首次绘制某目录时的一次性成本：只做字符串处理，不做系统调用。
pub fn key_for(name: &str, is_dir: bool, is_drive: bool) -> IconKey {
    if is_drive {
        return IconKey::Drive;
    }
    if is_dir {
        return IconKey::Folder;
    }
    match name.rfind('.') {
        Some(pos) if pos > 0 && pos + 1 < name.len() => {
            IconKey::Ext(name[pos + 1..].to_ascii_lowercase())
        }
        _ => IconKey::NoExt,
    }
}

/// 用某个真实路径的图标（用于缩略图/自定义文件夹图标，按需调用）。
pub fn icon_for_real_path(path: &Path, size: IconSize) -> *mut core::ffi::c_void {
    let w = to_wide(&path.to_string_lossy());
    let attrs = if path.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    unsafe { shell_icon(w.as_ptr(), attrs, size, false) }
}

/// 取窗口图标（大图标），用于任务栏与 Alt-Tab。
pub fn app_icon() -> *mut core::ffi::c_void {
    let exe = std::env::current_exe().unwrap_or_default();
    let w = to_wide(&exe.to_string_lossy());
    unsafe { shell_icon(w.as_ptr(), FILE_ATTRIBUTE_NORMAL, IconSize::Large, false) }
}

/// 让窗口知道自己的图标（小图标用于标题栏）。
pub fn apply_window_icons(hwnd: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_SETICON};
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;

    let exe = std::env::current_exe().unwrap_or_default();
    let w = to_wide(&exe.to_string_lossy());
    unsafe {
        let big = shell_icon(w.as_ptr(), FILE_ATTRIBUTE_NORMAL, IconSize::Large, false);
        let small = shell_icon(w.as_ptr(), FILE_ATTRIBUTE_NORMAL, IconSize::Small, false);
        if !big.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, big as isize);
        }
        if !small.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small as isize);
        }
    }
}
