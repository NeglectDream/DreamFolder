//! Shell 集成：用默认程序打开、属性页、打开方式、系统资源管理器。

use std::path::Path;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use crate::win::to_wide;

const SW_SHOWNORMAL: i32 = 1;

/// ShellExecuteW 的返回值 > 32 才算成功。
///
/// **每个字符串都必须 NUL 结尾**：`ShellExecuteW` 没有长度参数，全靠
/// 结尾的 0 定界。以前这里用的是 `to_wide_raw`（不补 0），Shell 会一路
/// 读到缓冲区外的垃圾，返回值永远 ≤ 32——「双击打不开、打开方式无效、
/// 属性弹不出来」全是这一个原因。
fn invoke(hwnd: HWND, verb: &str, file: &Path, params: Option<&str>) -> Result<(), String> {
    let verb_w = to_wide(verb);
    // Shell 不认 verbatim 前缀，直接传过去一律「找不到文件」
    let file = crate::fs::strip_verbatim(file);
    let file_w = to_wide(&file.to_string_lossy());
    // 可选参数：None 时传 NULL，而不是「空 Vec 的 dangling 指针」
    let params_w = params.map(to_wide);
    let params_ptr = params_w
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or(std::ptr::null());

    let ret = unsafe {
        ShellExecuteW(
            hwnd,
            verb_w.as_ptr(),
            file_w.as_ptr(),
            params_ptr,
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    let code = ret as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(describe_shell_execute(code))
    }
}

fn describe_shell_execute(code: isize) -> String {
    match code {
        0 => "内存不足".to_string(),
        2 => "找不到文件".to_string(),
        3 => "找不到路径".to_string(),
        5 => "拒绝访问".to_string(),
        8 => "内存不足".to_string(),
        26 => "共享冲突".to_string(),
        27 => "文件关联不完整".to_string(),
        28 => "DDE 操作超时".to_string(),
        29 => "DDE 操作失败".to_string(),
        30 => "DDE 忙".to_string(),
        31 => "没有关联的应用程序".to_string(),
        32 => "无法加载关联的 DLL".to_string(),
        _ => format!("调用失败（代码 {code}）"),
    }
}

/// 用系统默认程序打开文件，或进入目录。
pub fn open_default(hwnd: HWND, path: &Path) -> Result<(), String> {
    // 先按关联打开；没有关联时 Shell 会返回 SE_ERR_NOASSOC，
    // 这时退回「打开方式」动词，让系统弹出选择程序对话框——资源管理器就是这么做的。
    let first = invoke(hwnd, "open", path, None);
    if first.is_ok() {
        return Ok(());
    }
    match invoke(hwnd, "openas", path, None) {
        Ok(()) => Ok(()),
        // 两个都失败就报最初的原因，它对用户更有信息量
        Err(_) => first,
    }
}

/// 打开项目属性对话框。
pub fn show_properties(hwnd: HWND, path: &Path) -> Result<(), String> {
    invoke(hwnd, "properties", path, None)
}

/// 打开 URL（http/https/mailto 等）。供菜单里的「反馈」使用。
pub fn open_url(url: &str) -> Result<(), String> {
    let verb = to_wide("open");
    let target = to_wide(url);
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if ret as isize > 32 {
        Ok(())
    } else {
        Err(describe_shell_execute(ret as isize))
    }
}

/// 用指定程序打开一批文件（把一个或多个文件当参数传给它）。
///
/// 用于「拖到某个 .exe 上用它打开」。`lpParameters` 是**一整条命令行**，
/// 所以每段自己加引号：路径里有空格时不加引号会被拆成多个参数。
pub fn run_with_program(program: &Path, args: &[std::path::PathBuf]) -> Result<(), String> {
    let program = crate::fs::strip_verbatim(program);
    let mut cmdline = String::new();
    for a in args {
        if !cmdline.is_empty() {
            cmdline.push(' ');
        }
        let s = crate::fs::strip_verbatim(a).to_string_lossy().to_string();
        if s.contains(' ') || s.contains('\t') {
            cmdline.push('"');
            cmdline.push_str(&s);
            cmdline.push('"');
        } else {
            cmdline.push_str(&s);
        }
    }
    let workdir = program.parent().map(|p| p.to_path_buf());
    if args.is_empty() {
        return invoke(std::ptr::null_mut(), "open", &program, None);
    }
    let verb = to_wide("open");
    let file = to_wide(&program.to_string_lossy());
    let params = to_wide(&cmdline);
    let cwd = workdir.map(|d| to_wide(&d.to_string_lossy()));
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            cwd.as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null()),
            SW_SHOWNORMAL,
        )
    };
    if ret as isize > 32 {
        Ok(())
    } else {
        Err(describe_shell_execute(ret as isize))
    }
}

/// `DROPFILES` 头部大小（`pFiles: DWORD` + `pt: POINT` + `fNC: BOOL` + `fWide: BOOL`）。
const DROPFILES_SIZE: usize = 20;

/// 拼装 CF_HDROP 的载荷：`DROPFILES` 头 + 双 NUL 结尾的宽字符路径列表。
///
/// 字段偏移按 WinUser.h 的定义逐个对齐：
///
/// ```text
/// offset 0  DWORD pFiles     —— 字符串区起点（= 20）
/// offset 4  LONG  pt.x
/// offset 8  LONG  pt.y
/// offset 12 BOOL  fNC
/// offset 16 BOOL  fWide      —— 必须为 TRUE，否则系统按 ANSI 解析宽字符串，
///                              `C:\...` 会被读成 `C`，粘贴时 Shell 报
///                              「操作失败（代码 0x2）」
/// ```
///
/// 抽成纯函数是为了能被单测逐字节校验——原先直接往 `GlobalLock` 出来的
/// 指针上按 `u32` 下标 `.add(n)` 写，把 pt/fNC/fWide 全写错了位置，
/// 连路径首字符都被压成 NUL。
fn build_dropfiles(paths: &[std::path::PathBuf]) -> Vec<u8> {
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(p.to_string_lossy().encode_utf16());
        wide.push(0);
    }
    wide.push(0); // 列表整体再补一个 NUL

    let mut buf = vec![0u8; DROPFILES_SIZE + wide.len() * 2];
    let header = buf.as_mut_ptr() as *mut u32;
    unsafe {
        *header = DROPFILES_SIZE as u32; // pFiles = 0
        *header.add(1) = 0; // pt.x = 4
        *header.add(2) = 0; // pt.y = 8
        *header.add(3) = 0; // fNC  = 12
        *header.add(4) = 1; // fWide = 16，宽字符
        let dst = buf.as_mut_ptr().add(DROPFILES_SIZE) as *mut u16;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
    }
    buf
}

/// 把路径放到系统剪贴板（CF_HDROP 格式），供别的程序粘贴。
pub fn set_clipboard_files(paths: &[std::path::PathBuf]) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Ole::CF_HDROP;

    let payload = build_dropfiles(paths);

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return false;
        }

        let hmem = GlobalAlloc(GMEM_MOVEABLE, payload.len());
        if hmem.is_null() {
            CloseClipboard();
            return false;
        }
        let base = GlobalLock(hmem) as *mut u8;
        if base.is_null() {
            CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), base, payload.len());

        GlobalUnlock(hmem);
        SetClipboardData(CF_HDROP as u32, hmem);
        CloseClipboard();
        true
    }
}

/// 把纯文本放到剪贴板（CF_UNICODETEXT），供「复制路径」使用。
pub fn set_clipboard_text(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    /// CF_UNICODETEXT：windows-sys 把它放在 `Win32_System_Ole`，
    /// 为了一个常量不值得多引一个模块，直接按 WinUser.h 写值。
    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return false;
        }

        // CF_UNICODETEXT 要求以 NUL 结尾，末尾补一个
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        wide.push(0);

        let hmem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        if hmem.is_null() {
            CloseClipboard();
            return false;
        }
        let dst = GlobalLock(hmem) as *mut u16;
        if dst.is_null() {
            CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());

        GlobalUnlock(hmem);
        SetClipboardData(CF_UNICODETEXT, hmem);
        CloseClipboard();
        true
    }
}

/// 读剪贴板里的纯文本（CF_UNICODETEXT），供地址栏 Ctrl+V 使用。
pub fn get_clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return None;
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let mut out = None;
        if !handle.is_null() {
            let src = GlobalLock(handle) as *const u16;
            if !src.is_null() {
                let mut len = 0usize;
                while *src.add(len) != 0 {
                    len += 1;
                }
                let slice = core::slice::from_raw_parts(src, len);
                out = Some(String::from_utf16_lossy(slice));
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        out
    }
}

/// 读剪贴板中的文件列表（用于粘贴）。
pub fn get_clipboard_files() -> Vec<std::path::PathBuf> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_HDROP;
    use windows_sys::Win32::UI::Shell::DragQueryFileW;

    let mut out = Vec::new();
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP as u32) == 0 {
            return out;
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return out;
        }
        let handle = GetClipboardData(CF_HDROP as u32);
        if !handle.is_null() {
            let hdrop = GlobalLock(handle) as *mut core::ffi::c_void;
            if !hdrop.is_null() {
                let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
                for i in 0..count {
                    let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                    if len == 0 {
                        continue;
                    }
                    let mut buf = vec![0u16; (len + 1) as usize];
                    DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
                    out.push(std::path::PathBuf::from(crate::win::from_wide(&buf)));
                }
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
    }
    out
}

// ------------------------------------------------------------------ 已知文件夹

/// 系统已知文件夹（`KNOWNFOLDERID` 的一个子集）。
///
/// 不用 `%USERPROFILE%\Desktop` 这种拼法：桌面、文档、图片都可能被 OneDrive
/// 或组策略重定向到别的卷，只有问 Shell 才拿得准。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownFolder {
    /// 用户主文件夹。
    Profile,
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
}

impl KnownFolder {
    /// 对应的 `KNOWNFOLDERID` 值，取自 `KnownFolders.h`。
    fn id(self) -> windows_sys::core::GUID {
        use windows_sys::core::GUID;
        match self {
            KnownFolder::Profile => GUID::from_u128(0x5E6C858F_0E22_4760_9AFE_EA3317B67173),
            KnownFolder::Desktop => GUID::from_u128(0xB4BFCC3A_DB2C_424C_B029_7FE99A87C641),
            KnownFolder::Downloads => GUID::from_u128(0x374DE290_123F_4565_9164_39C4925E467B),
            KnownFolder::Documents => GUID::from_u128(0xFDD39AD0_238F_46AF_ADB4_6C85480369C7),
            KnownFolder::Pictures => GUID::from_u128(0x33E28130_4E1E_4676_835A_98395C3BC3BB),
            KnownFolder::Music => GUID::from_u128(0x4BD8D571_6D19_48D3_BE97_422220080E43),
            KnownFolder::Videos => GUID::from_u128(0x18989B1D_99B5_455B_841C_AB7C74E4DDFC),
        }
    }
}

/// 解析已知文件夹的绝对路径。失败返回 `None`。
///
/// 返回的内存由 `SHGetKnownFolderPath` 用 `CoTaskMemAlloc` 分配，
/// 必须用 `CoTaskMemFree` 释放——这里在转换完成后立刻释放，不持有裸指针。
pub fn known_folder(folder: KnownFolder) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;

    let id = folder.id();
    let mut raw: *mut u16 = std::ptr::null_mut();
    // KF_FLAG_DEFAULT = 0，当前用户令牌传 null
    let hr = unsafe { SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut raw) };
    if hr < 0 || raw.is_null() {
        return None;
    }

    // PWSTR 是 NUL 结尾的宽字符串，先量长度再切片
    let mut len = 0usize;
    unsafe {
        while *raw.add(len) != 0 {
            len += 1;
        }
    }
    let slice = unsafe { std::slice::from_raw_parts(raw, len) };
    let path = std::path::PathBuf::from(crate::win::from_wide(slice));

    unsafe { CoTaskMemFree(raw as *const core::ffi::c_void) };

    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// DROPFILES 头逐字段校验：pFiles=20、pt={0,0}、fNC=0、fWide=1，
    /// 字符串区从 20 起且双 NUL 结尾。
    ///
    /// 这组偏移曾经全写错位——`fWide` 被清成 0 后系统按 ANSI 解析宽路径，
    /// `C:\...` 变成 `C`，粘贴时 Shell 报「操作失败（代码 0x2）」。
    #[test]
    fn dropfiles_header_layout() {
        let payload = build_dropfiles(&[PathBuf::from(r"C:\dir\file.txt")]);
        let u32_at = |off: usize| u32::from_le_bytes(payload[off..off + 4].try_into().unwrap());

        assert_eq!(u32_at(0), 20, "pFiles 必须指向字符串区");
        assert_eq!(u32_at(4), 0, "pt.x");
        assert_eq!(u32_at(8), 0, "pt.y");
        assert_eq!(u32_at(12), 0, "fNC");
        assert_eq!(u32_at(16), 1, "fWide 必须为 TRUE");

        // 字符串区：宽字符，逐个对上，末尾双 NUL
        let wide: Vec<u16> = payload[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let expect: Vec<u16> = r"C:\dir\file.txt".encode_utf16().chain([0, 0]).collect();
        assert_eq!(wide, expect);
    }

    /// 多个路径之间用一个 NUL 分隔，整体双 NUL 结尾。
    #[test]
    fn dropfiles_multiple_paths() {
        let payload = build_dropfiles(&[PathBuf::from("C:/a"), PathBuf::from("C:/b")]);
        let wide: Vec<u16> = payload[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = String::from_utf16(&wide[..6]).unwrap();
        assert_eq!(s, "C:/a\0C"); // 第一个路径后是单个 NUL
        assert_eq!(&wide[wide.len() - 2..], &[0, 0], "整体双 NUL 结尾");
    }
}
