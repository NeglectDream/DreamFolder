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
const SHGFI_SYSICONINDEX: u32 = 0x0000_4000;

/// 系统「超大」图像列表（256px）。
const SHIL_JUMBO: i32 = 4;
/// `ImageList_Draw` 的透明标志。
const ILD_TRANSPARENT: u32 = 0x0000_0001;
/// `IID_IImageList`。
const IID_IIMAGE_LIST: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x46EB_5926_582E_4017_9FDF_E899_8DAA_0950);

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

/// 每个尺寸档最多缓存多少个图标。
///
/// 扩展名图标本来只有几十个，但 `.exe`/`.lnk` 这类「图标在文件自己身上」
/// 的条目是**按完整路径**缓存的，翻几个装满程序的目录就能堆到几百条。
/// 每个图标都是一个 GDI 对象，不设上限等于慢慢泄漏句柄。
const ICON_CACHE_MAX: usize = 384;

/// 写回缓存并维持上限。超出时清空整表，返回需要销毁的旧句柄。
///
/// 清空而不是做 LRU：命中率主要来自同一目录里的重复扩展名，
/// 整表清空的代价只是「下次重查一遍」，比维护 LRU 链表简单得多。
fn store_capped(
    table: &mut HashMap<IconKey, isize>,
    key: &IconKey,
    h: isize,
) -> Vec<isize> {
    let mut evicted = Vec::new();
    if table.len() >= ICON_CACHE_MAX {
        evicted.extend(table.drain().map(|(_, v)| v));
    }
    table.insert(key.clone(), h);
    evicted
}

/// 图标缓存。挂在 UI 状态上，随进程存活。
pub struct IconCache {
    small: HashMap<IconKey, isize>,
    large: HashMap<IconKey, isize>,
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

        // 先只读查缓存；命中即返回，避免与后面的可变借用重叠。
        let cached = if size == IconSize::Small {
            self.small.get(key)
        } else {
            self.large.get(key)
        };
        if let Some(&h) = cached {
            return h as *mut core::ffi::c_void;
        }

        // 未命中：先查询（需要 &mut self），再写回所在表。
        let h = self.query_by_key(key, size);
        // 真实路径查询会失败——文件刚被删、或索引里留下的是过期路径。
        // 这时退回到「无扩展名」那张通用图标，别让这一行空着。
        if h.is_null() && matches!(key, IconKey::Path(_)) {
            return self.get(&IconKey::NoExt, size);
        }
        let evicted = if size == IconSize::Small {
            store_capped(&mut self.small, key, h as isize)
        } else {
            store_capped(&mut self.large, key, h as isize)
        };
        // 挤出去的句柄当场销毁，别攒着泄漏
        for old in evicted {
            if old != 0 {
                unsafe {
                    use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
                    DestroyIcon(old as *mut core::ffi::c_void);
                }
            }
        }
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
            // 真实路径：已知文件夹、具体驱动器。属性不走 USEFILEATTRIBUTES，
            // 这样 Shell 会去看「这个目录到底是什么」（下载/图片/音乐图标各不相同）。
            IconKey::Path(path) => {
                let w = to_wide(path);
                let is_dir = Path::new(path).is_dir();
                let attrs = if is_dir {
                    FILE_ATTRIBUTE_DIRECTORY
                } else {
                    FILE_ATTRIBUTE_NORMAL
                };
                return unsafe { shell_icon(w.as_ptr(), attrs, size, false) };
            }
            // 虚拟项（此电脑/回收站/网络）没有路径，只能按 CSIDL 取 PIDL
            IconKey::Special(id) => return special_icon(*id, size),
            IconKey::Ext(ext) => (format!("x.{ext}"), FILE_ATTRIBUTE_NORMAL),
            IconKey::NoExt => ("x".to_string(), FILE_ATTRIBUTE_NORMAL),
            _ => ("x".to_string(), FILE_ATTRIBUTE_NORMAL),
        };
        let w = to_wide(&fake_name);
        unsafe { shell_icon(w.as_ptr(), attrs, size, true) }
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
    // 大图标优先取系统的 256px 版本。
    //
    // `SHGFI_LARGEICON` 只会给 32×32——界面按缩放比把它画到 96、144 甚至更大，
    // 结果就是糊成一团，这正是「图标没有严格按比例放大」的观感来源。
    // 系统本身有一份 256px 的图像列表，直接取来再缩小画，任何比例都清晰。
    if size == IconSize::Large {
        let big = jumbo_icon(path, attrs, use_attributes);
        if !big.is_null() {
            return big;
        }
    }

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

/// 取 Shell 特殊项图标：0 = 此电脑，1 = 回收站，2 = 网络。
///
/// 走 `SHGetStockIconInfo`：这三个是系统自带的「库存图标」，
/// 不用构造 PIDL，也就没有内存所有权问题——比 SHGetSpecialFolderLocation
/// 那条路少一次跨进程分配，不会因为释放时机出错而崩。
fn special_icon(id: u8, size: IconSize) -> *mut core::ffi::c_void {
    use windows_sys::Win32::UI::Shell::{
        SHGetStockIconInfo, SHSTOCKICONINFO, SHGSI_ICON, SHGSI_LARGEICON, SHGSI_SMALLICON,
        SIID_DESKTOPPC, SIID_NETWORKCONNECT, SIID_RECYCLER,
    };

    let siid = match id {
        0 => SIID_DESKTOPPC,
        1 => SIID_RECYCLER,
        _ => SIID_NETWORKCONNECT,
    };
    let mut flags = SHGSI_ICON;
    flags |= if size == IconSize::Small {
        SHGSI_SMALLICON
    } else {
        SHGSI_LARGEICON
    };

    unsafe {
        let mut info: SHSTOCKICONINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHSTOCKICONINFO>() as u32;
        if SHGetStockIconInfo(siid, flags, &mut info) < 0 || info.hIcon.is_null() {
            return std::ptr::null_mut();
        }
        info.hIcon
    }
}

/// 从系统 256px 图像列表里取一个图标。失败返回空指针，由调用方回退到 32px。
///
/// # Safety
/// `path` 必须是 NUL 结尾的宽串指针。
unsafe fn jumbo_icon(
    path: *const u16,
    attrs: u32,
    use_attributes: bool,
) -> *mut core::ffi::c_void {
    use windows_sys::Win32::UI::Shell::SHGetImageList;

    // 1) 先拿到图标在系统图像列表里的索引
    let mut info: SHFILEINFOW = std::mem::zeroed();
    let mut flags = SHGFI_SYSICONINDEX;
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
    if ret == 0 {
        return std::ptr::null_mut();
    }

    // 2) 系统 JUMBO 图像列表
    let mut himl: *mut core::ffi::c_void = std::ptr::null_mut();
    if SHGetImageList(SHIL_JUMBO, &IID_IIMAGE_LIST, &mut himl) < 0 || himl.is_null() {
        return std::ptr::null_mut();
    }

    // 3) IImageList::GetIcon。IUnknown 三个方法之后依次是
    //    Add / ReplaceIcon / SetOverlayImage / Replace / AddMasked / Draw / Remove / GetIcon，
    //    因此 GetIcon 落在索引 10。
    let vtbl = *(himl as *mut *const *const core::ffi::c_void);
    let get_icon: unsafe extern "system" fn(
        *mut core::ffi::c_void,
        i32,
        u32,
        *mut *mut core::ffi::c_void,
    ) -> i32 = std::mem::transmute(*vtbl.add(10));
    let mut hicon: *mut core::ffi::c_void = std::ptr::null_mut();
    let hr = get_icon(himl, info.iIcon, ILD_TRANSPARENT, &mut hicon);

    // 4) 释放图像列表。取出的 HICON 是独立对象，不受影响。
    let release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
        std::mem::transmute(*vtbl.add(2));
    release(himl);

    if hr < 0 {
        std::ptr::null_mut()
    } else {
        hicon
    }
}

/// 图标必须取自文件本身的扩展名。
///
/// 其余类型按扩展名查一次就够（`SHGFI_USEFILEATTRIBUTES`，完全不碰磁盘）；
/// 但这几类的图标是嵌在文件里的——按扩展名查只会得到一张没有意义的
/// 通用方块，而资源管理器显示的是程序自己的图标、快捷方式的目标图标。
const REAL_ICON_EXTS: [&str; 8] = ["exe", "lnk", "ico", "cur", "msi", "scr", "cpl", "dll"];

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

/// 带完整路径的图标键：exe/lnk 这些按真实文件取图标，其余仍按扩展名。
///
/// 注意 `IconKey::Path` 是按**完整路径**缓存的，同一个 exe 图标只查一次，
/// 但不同路径各占一格——所以图标缓存必须有上限（见 `ICON_CACHE_MAX`）。
pub fn key_for_entry(name: &str, full: &Path, is_dir: bool, is_drive: bool) -> IconKey {
    let key = key_for(name, is_dir, is_drive);
    if let IconKey::Ext(ext) = &key {
        if REAL_ICON_EXTS.contains(&ext.as_str()) {
            return IconKey::Path(full.to_string_lossy().into_owned());
        }
    }
    key
}

/// 让窗口知道自己的图标（大 / 小 / 小2 三档）。
///
/// 图标由 `win::appicon` 从内嵌 logo 现算，这里只是转一次手：
/// 以前是「拿 exe 路径去问 Shell 要图标」，而本项目没有 `.rc` 资源，
/// 问出来的永远是系统默认图标。共享句柄，不要 `DestroyIcon`。
pub fn apply_window_icons(hwnd: HWND) {
    unsafe { crate::win::appicon::apply_to_window(hwnd) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 驱动器与目录优先于扩展名判断。
    #[test]
    fn drive_and_folder_keys() {
        assert_eq!(key_for("C:\\", true, true), IconKey::Drive);
        assert_eq!(key_for("Documents", true, false), IconKey::Folder);
        // 目录名里带点也不能被当成扩展名
        assert_eq!(key_for("v1.2", true, false), IconKey::Folder);
    }

    /// 扩展名统一小写，无扩展名与隐藏文件各有归属。
    #[test]
    fn extension_keys() {
        assert_eq!(key_for("a.TXT", false, false), IconKey::Ext("txt".into()));
        assert_eq!(key_for("a.tar.gz", false, false), IconKey::Ext("gz".into()));
        assert_eq!(key_for("README", false, false), IconKey::NoExt);
        // 以点开头的文件（.gitignore）整体算「无扩展名」
        assert_eq!(key_for(".gitignore", false, false), IconKey::NoExt);
        // 末尾是点：点后面没有内容，不算扩展名
        assert_eq!(key_for("trailing.", false, false), IconKey::NoExt);
    }

    /// exe/lnk 这类图标藏在文件里，必须按完整路径取，才能拿到程序自己的图标。
    #[test]
    fn real_icon_extensions_use_path() {
        let p = Path::new("C:/tools/app.exe");
        assert_eq!(
            key_for_entry("app.exe", p, false, false),
            IconKey::Path("C:/tools/app.exe".into())
        );
        assert_eq!(
            key_for_entry("shortcut.lnk", Path::new("C:/x.lnk"), false, false),
            IconKey::Path("C:/x.lnk".into())
        );
        // 普通文件仍按扩展名共用一张图标，不吃满缓存
        assert_eq!(
            key_for_entry("note.txt", Path::new("C:/note.txt"), false, false),
            IconKey::Ext("txt".into())
        );
        // 目录不受影响
        assert_eq!(
            key_for_entry("app.exe", Path::new("C:/app.exe"), true, false),
            IconKey::Folder
        );
    }
}
