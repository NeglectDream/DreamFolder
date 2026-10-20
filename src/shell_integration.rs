//! 接管系统文件管理：注册表读写。
//!
//! 完全对齐原软件 `接管资源管理器.bat` 的行为与位置
//! （`HKU\.DEFAULT\Software\Classes\Folder\shell`），
//! 并额外支持当前用户的 `HKCU\Software\Classes\Folder\shell`
//! 以便在不提权的情况下测试。
//!
//! 同时提供原软件命令行里出现的 `/replace_explorer_all`
//! 与 `/restore_explorer_all` 两个动作的实现。

use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HKEY};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY_CURRENT_USER,
    HKEY_USERS, KEY_ALL_ACCESS, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

use crate::win::to_wide;

/// 接管状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeoverState {
    /// 已接管（当前用户或系统默认均有配置）。
    Active,
    /// 未接管。
    Inactive,
    /// 读取失败（权限或键不存在）。
    Unknown,
}

const SHELL_KEY_DEFAULT: &str = r"Software\Classes\Folder\shell";
const SHELL_KEY_HKCU: &str = r"Software\Classes\Folder\shell";
const MENU_KEY_NAME: &str = "360FileBrowser";
const NEWWINDOW_KEY_NAME: &str = "newwindow";

/// 打开/创建注册表键。
///
/// # Safety
/// 仅在 Win32 注册表 API 语境中使用。
unsafe fn create_key(root: HKEY, sub: &str) -> Option<HKEY> {
    let sub_w = to_wide(sub);
    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition = 0u32;
    let rc = RegCreateKeyExW(
        root,
        sub_w.as_ptr(),
        0,
        std::ptr::null(),
        REG_OPTION_NON_VOLATILE,
        KEY_ALL_ACCESS,
        std::ptr::null(),
        &mut hkey,
        &mut disposition,
    );
    if rc == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// 写一个 REG_SZ 值。
///
/// # Safety
/// `hkey` 必须是有效打开的键句柄。
unsafe fn set_string(hkey: HKEY, name: Option<&str>, value: &str) -> bool {
    let value_w = to_wide(value);
    let name_w = name.map(to_wide);
    let name_ptr = match &name_w {
        Some(v) => v.as_ptr(),
        None => std::ptr::null(),
    };
    let bytes = value_w.len() * std::mem::size_of::<u16>();
    let rc = RegSetValueExW(
        hkey,
        name_ptr,
        0,
        REG_SZ,
        value_w.as_ptr() as *const u8,
        bytes as u32,
    );
    rc == ERROR_SUCCESS
}

/// 本程序的可执行文件路径（注册到 shell 命令里）。
fn self_exe() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "360Folder.exe".to_string())
}

/// 检查是否已接管。
pub fn state() -> TakeoverState {
    let sub = to_wide(SHELL_KEY_DEFAULT);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_USERS,
            sub.as_ptr(),
            0,
            KEY_WRITE,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return TakeoverState::Unknown;
    }
    unsafe { RegCloseKey(hkey) };

    // 子键存在即认为已接管
    let sub2 = to_wide(&format!("{SHELL_KEY_DEFAULT}\\{MENU_KEY_NAME}"));
    let rc2 = unsafe {
        RegOpenKeyExW(
            HKEY_USERS,
            sub2.as_ptr(),
            0,
            KEY_WRITE,
            &mut hkey,
        )
    };
    if rc2 == ERROR_SUCCESS {
        unsafe { RegCloseKey(hkey) };
        TakeoverState::Active
    } else {
        TakeoverState::Inactive
    }
}

/// 执行接管。返回是否成功。
///
/// 涵盖原 .bat 的三件事：
///   1. `Folder\shell` 默认值设为 `360FileBrowser`
///   2. 添加「360文件夹」右键项与命令
///   3. 添加「在新窗口打开」子项与命令
pub fn take_over() -> Result<(), String> {
    let exe = self_exe();
    unsafe {
        // 1) 默认动词
        let shell = create_key(HKEY_USERS, SHELL_KEY_DEFAULT)
            .ok_or_else(|| "无法写入 HKU\\Software\\Classes\\Folder\\shell（需要管理员权限）".to_string())?;
        set_string(shell, None, MENU_KEY_NAME);
        RegCloseKey(shell);

        // 2) 主菜单项
        let menu_path = format!("{SHELL_KEY_DEFAULT}\\{MENU_KEY_NAME}");
        let menu = create_key(HKEY_USERS, &menu_path)
            .ok_or_else(|| "无法创建 360FileBrowser 菜单项".to_string())?;
        set_string(menu, None, "360文件夹");
        RegCloseKey(menu);

        let cmd_path = format!("{menu_path}\\command");
        let cmd = create_key(HKEY_USERS, &cmd_path)
            .ok_or_else(|| "无法创建 command 子键".to_string())?;
        set_string(cmd, None, &format!("\"{exe}\" \"%1\""));
        RegCloseKey(cmd);

        // 3) 在新窗口打开
        let nw_path = format!("{SHELL_KEY_DEFAULT}\\{NEWWINDOW_KEY_NAME}");
        let nw = create_key(HKEY_USERS, &nw_path)
            .ok_or_else(|| "无法创建 newwindow 菜单项".to_string())?;
        set_string(nw, None, "在新窗口打开(&E)");
        RegCloseKey(nw);

        let nw_cmd_path = format!("{nw_path}\\command");
        let nw_cmd = create_key(HKEY_USERS, &nw_cmd_path)
            .ok_or_else(|| "无法创建 newwindow command".to_string())?;
        set_string(nw_cmd, None, &format!("\"{exe}\" /OpenInNewWindow \"%1\""));
        RegCloseKey(nw_cmd);
    }
    Ok(())
}

/// 恢复系统默认（对应 .bat 的 [2] 分支）。
pub fn restore() -> Result<(), String> {
    unsafe {
        // 删除两个菜单项
        for name in [MENU_KEY_NAME, NEWWINDOW_KEY_NAME] {
            let path = format!("{SHELL_KEY_DEFAULT}\\{name}");
            let path_w = to_wide(&path);
            RegDeleteTreeW(HKEY_USERS, path_w.as_ptr());
        }

        // 默认值恢复为系统默认的 "none"
        let shell = create_key(HKEY_USERS, SHELL_KEY_DEFAULT).ok_or_else(|| {
            "无法写入 HKU\\Software\\Classes\\Folder\\shell（需要管理员权限）".to_string()
        })?;
        set_string(shell, None, "none");
        RegCloseKey(shell);
    }
    Ok(())
}

/// 接管当前用户（无需管理员，用于测试）。
pub fn take_over_current_user() -> Result<(), String> {
    let exe = self_exe();
    unsafe {
        let shell = create_key(HKEY_CURRENT_USER, SHELL_KEY_HKCU)
            .ok_or_else(|| "无法写入 HKCU\\Software\\Classes\\Folder\\shell".to_string())?;
        set_string(shell, None, MENU_KEY_NAME);
        RegCloseKey(shell);

        let menu_path = format!("{SHELL_KEY_HKCU}\\{MENU_KEY_NAME}");
        let menu = create_key(HKEY_CURRENT_USER, &menu_path)
            .ok_or_else(|| "无法创建菜单项".to_string())?;
        set_string(menu, None, "360文件夹");
        RegCloseKey(menu);

        let cmd_path = format!("{menu_path}\\command");
        let cmd = create_key(HKEY_CURRENT_USER, &cmd_path)
            .ok_or_else(|| "无法创建 command".to_string())?;
        set_string(cmd, None, &format!("\"{exe}\" \"%1\""));
        RegCloseKey(cmd);
    }
    Ok(())
}

/// 恢复当前用户。
pub fn restore_current_user() -> Result<(), String> {
    unsafe {
        let path = format!("{SHELL_KEY_HKCU}\\{MENU_KEY_NAME}");
        let path_w = to_wide(&path);
        RegDeleteTreeW(HKEY_CURRENT_USER, path_w.as_ptr());
        let nw = format!("{SHELL_KEY_HKCU}\\{NEWWINDOW_KEY_NAME}");
        let nw_w = to_wide(&nw);
        RegDeleteTreeW(HKEY_CURRENT_USER, nw_w.as_ptr());
    }
    Ok(())
}

/// 创建桌面快捷方式（对应主菜单「添加桌面图标」）。
pub fn create_desktop_shortcut() -> Result<(), String> {
    // 用 PowerShell 生成 .lnk，避免引入 COM 接口样板代码。
    // 这是冷启动路径之外的一次性操作，代价可接受。
    let exe = self_exe();
    let desktop = std::env::var("USERPROFILE")
        .map(|p| format!("{p}\\Desktop"))
        .unwrap_or_else(|_| ".".to_string());
    let lnk = format!("{desktop}\\360文件夹.lnk");

    let script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{lnk}'); \
         $s.TargetPath = '{exe}'; \
         $s.WorkingDirectory = '{}'; \
         $s.Save()",
        Path::new(&exe)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    );

    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .status()
        .map_err(|e| format!("无法启动 PowerShell：{e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err("创建快捷方式失败".to_string())
    }
}

/// 判断给定路径是否为「本程序自己」，用于接管后自检。
pub fn is_self_registered() -> bool {
    matches!(state(), TakeoverState::Active)
}
