//! Shell 集成：用默认程序打开、属性页、打开方式、系统资源管理器。

use std::path::Path;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::ShellExecuteW;

use crate::win::to_wide_raw;

const SW_SHOWNORMAL: i32 = 1;

/// ShellExecuteW 的返回值 > 32 才算成功。
fn invoke(hwnd: HWND, verb: &str, file: &Path, params: Option<&str>) -> Result<(), String> {
    let verb_w = to_wide_raw(verb);
    let file_w = to_wide_raw(&file.to_string_lossy());
    let params_w: Vec<u16> = match params {
        Some(p) => to_wide_raw(p),
        None => Vec::new(),
    };
    let params_ptr = if params_w.is_empty() {
        std::ptr::null()
    } else {
        params_w.as_ptr()
    };

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
    invoke(hwnd, "open", path, None)
}

/// 弹出「打开方式」选择器。
pub fn open_with_dialog(hwnd: HWND, path: &Path) -> Result<(), String> {
    invoke(hwnd, "openas", path, None)
}

/// 打开项目属性对话框。
pub fn show_properties(hwnd: HWND, path: &Path) -> Result<(), String> {
    invoke(hwnd, "properties", path, None)
}

/// 以管理员身份运行。
pub fn run_as_admin(hwnd: HWND, path: &Path) -> Result<(), String> {
    invoke(hwnd, "runas", path, None)
}

/// 打印。
pub fn print(hwnd: HWND, path: &Path) -> Result<(), String> {
    invoke(hwnd, "print", path, None)
}

/// 在系统资源管理器中打开目录（对应菜单「接管系统文件管理」的反向操作）。
pub fn open_in_system_explorer(path: &Path) -> Result<(), String> {
    invoke(std::ptr::null_mut(), "open", path, None)
}

/// 打开控制面板的「文件夹选项」页面。
///
/// 原软件通过 `shell32.dll,Options_RunDLL` 调用（PE 字符串中的
/// `name Microsoft.FolderOptions` 即为此用途）。
pub fn open_folder_options() -> Result<(), String> {
    let verb = to_wide_raw("open");
    let file = to_wide_raw("rundll32.exe");
    let params = to_wide_raw("shell32.dll,Options_RunDLL 0");
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
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

/// 打开「系统属性」的回收站设置等常见位置（供后续扩展）。
pub fn open_url(url: &str) -> Result<(), String> {
    let verb = to_wide_raw("open");
    let target = to_wide_raw(url);
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

/// 把路径放到系统剪贴板（CF_HDROP 格式），供别的程序粘贴。
pub fn set_clipboard_files(paths: &[std::path::PathBuf]) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows_sys::Win32::System::Ole::CF_HDROP;

    // DROPFILES 结构：pFiles 偏移 + pt + fNC + fWide
    const DROPFILES_SIZE: usize = 20;

    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return false;
        }

        // 拼装双 NUL 结尾的宽字符路径列表
        let mut wide: Vec<u16> = Vec::new();
        for p in paths {
            wide.extend(p.to_string_lossy().encode_utf16());
            wide.push(0);
        }
        wide.push(0);

        let total = DROPFILES_SIZE + wide.len() * 2;
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total);
        if hmem.is_null() {
            CloseClipboard();
            return false;
        }
        let base = GlobalLock(hmem) as *mut u8;
        if base.is_null() {
            CloseClipboard();
            return false;
        }

        // DROPFILES { DWORD pFiles; POINT pt; BOOL fNC; BOOL fWide; }
        std::ptr::write_bytes(base, 0, DROPFILES_SIZE);
        *(base as *mut u32) = DROPFILES_SIZE as u32; // pFiles
        // pt = {0,0} 已由 write_bytes 清零
        *((base as *mut u32).add(4)) = 0; // pt.x
        *((base as *mut u32).add(5)) = 0; // pt.y
        *((base as *mut i32).add(6)) = 0; // fNC = FALSE
        *((base as *mut i32).add(7)) = 1; // fWide = TRUE

        let dst = base.add(DROPFILES_SIZE) as *mut u16;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());

        GlobalUnlock(hmem);
        SetClipboardData(CF_HDROP as u32, hmem);
        CloseClipboard();
        true
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
