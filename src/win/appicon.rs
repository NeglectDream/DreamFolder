//! 应用图标：仓库里那张 `assets/logo.png` 就是唯一的图标源。
//!
//! 项目刻意**不**引入 `.rc` 资源（那要 `rc.exe` 或构建脚本，和「零构建
//! 脚本、零新依赖」的取舍冲突）。所以三个用户可见的图标面——窗口类、
//! `WM_SETICON`、桌面快捷方式——全部在运行时从这张 PNG 现算：
//!
//! - 窗口/任务栏要的是 `HICON`：`GdipCreateHICONFromBitmap`。
//! - 桌面快捷方式要的是文件路径：把 logo 栅格化成 16/32/48/256 四档，
//!   写成一个真正的 BMP-in-ICO 到 Profile 目录。
//!
//! 结果都缓存：`HICON` 存进程级 `OnceLock`，图标文件按修改时间复用。
//! 取不到时一律退化成系统图标（`wnd::explorer_icons()`），永不返回空。

use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, HICON, SM_CXICON, SM_CXSMICON,
};

use super::gdiplus::Image;

/// 唯一图标源。130×127、8bit RGBA。
const LOGO_PNG: &[u8] = include_bytes!("../../assets/logo.png");

/// 图标文件名（落在 Profile 目录下）。
pub const ICO_NAME: &str = "dreamfolder.ico";

/// .ico 里要带的四档尺寸（Windows 在各处分别用这四个）。
const ICO_SIZES: [i32; 4] = [16, 32, 48, 256];

/// 小图标（标题栏 / Alt+Tab 列表）。进程内共享，**不要** DestroyIcon。
pub fn small() -> HICON {
    cached_icon(SM_CXSMICON, &SMALL)
}

/// 大图标（任务栏 / Alt+Tab / 窗口类）。
pub fn large() -> HICON {
    cached_icon(SM_CXICON, &LARGE)
}

static SMALL: OnceLock<isize> = OnceLock::new();
static LARGE: OnceLock<isize> = OnceLock::new();

/// 从内嵌 PNG 解码出位图。每次调用都重新解码，调用方自己缓存。
fn logo() -> Option<Image> {
    Image::from_bytes(LOGO_PNG)
}

/// 按系统指标取尺寸生成 HICON 并缓存。
///
/// 用 `SM_CXICON`/`SM_CXSMICON` 而不是写死 32/16：高 DPI 下这两个值
/// 会跟着变（150% 时大图标是 48），照它生成才不会让系统去拉伸位图。
fn cached_icon(metric: i32, slot: &'static OnceLock<isize>) -> HICON {
    if let Some(v) = slot.get() {
        return *v as HICON;
    }
    let side = unsafe { GetSystemMetrics(metric) };
    let side = if side > 0 { side } else { 32 };
    let icon = logo()
        .and_then(|img| {
            let h = img.to_hicon_at(side);
            if h.is_null() {
                None
            } else {
                Some(h)
            }
        })
        // 退化：从 explorer.exe 抽系统图标。同样是进程共享句柄，不用释放。
        .unwrap_or_else(|| unsafe { super::wnd::explorer_icons().0 });
    let _ = slot.set(icon as isize);
    icon
}

/// 确保 Profile 目录里有一份最新的 `.ico`，返回它的路径。
///
/// 桌面快捷方式需要一个**文件**才能设图标，所以这里把内嵌 PNG 栅格化
/// 落盘。文件已存在且不比 exe 旧时直接复用，避免每次启动都写盘。
///
/// 任何一步失败都退化：先试 BMP-in-ICO，再试 PNG-in-ICO，都失败返回 `None`
/// （调用方用 exe 自身的图标）。
pub fn ensure_ico_file() -> Option<PathBuf> {
    let dir = crate::cfg::paths::profile_dir();
    let path = dir.join(ICO_NAME);

    // 复用判定：图标存在，且比本 exe 新。exe 换了（升级）就重新生成。
    if let Ok(meta) = std::fs::metadata(&path) {
        let ico_time = meta.modified().ok();
        let exe_time = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        let fresh = match (ico_time, exe_time) {
            (Some(a), Some(b)) => a >= b,
            (Some(_), None) => true,
            _ => false,
        };
        if fresh && meta.len() > 22 {
            return Some(path);
        }
    }

    let img = logo()?;
    let mut frames: Vec<(i32, Vec<u8>)> = Vec::with_capacity(ICO_SIZES.len());
    for side in ICO_SIZES {
        if let Some(px) = img.rasterize(side) {
            frames.push((side, px));
        }
    }
    if !frames.is_empty() {
        if super::gdiplus::write_ico(&frames, &path).is_some() {
            return Some(path);
        }
    }
    // 退化：PNG-in-ICO（Vista 起支持），只带一个 256 尺寸的条目。
    super::gdiplus::write_png_ico(LOGO_PNG, 256, &path).map(|_| path)
}

/// `WM_SETICON` 那三档一起设。小图标要同时占 `ICON_SMALL` 与 `ICON_SMALL2`，
/// 否则任务栏在某些 DPI 下会退回类图标。
///
/// # Safety
/// `hwnd` 必须是有效窗口句柄。
pub unsafe fn apply_to_window(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, ICON_BIG, ICON_SMALL, ICON_SMALL2, WM_SETICON,
    };

    let big = large();
    let sm = small();
    if !big.is_null() {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize);
    }
    if !sm.is_null() {
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, sm as isize);
        SendMessageW(hwnd, WM_SETICON, ICON_SMALL2 as usize, sm as isize);
    }
}
