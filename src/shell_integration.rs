//! 接管系统文件管理：注册表读写。
//!
//! 接管的意思是：在文件夹上双击 / 右键时，Shell 把 DreamFolder 当成
//! 打开它的程序。做法是往 `Folder` 类的 `shell` 子树里写一个动词，并把
//! `Folder\shell` 的默认值指向它。
//!
//! **注册表根的坑**（原实现踩了这个坑，导致「管理员也启用不了」）：
//! `HKEY_USERS` 下面只有各用户的 SID 和 `.DEFAULT` / `.CLASSES`，
//! 直接写 `HKEY_USERS\Software\Classes\...` 会建出一个 Shell 永远不读的
//! 孤儿键。正确的三个落点是：
//!
//! | 根 | 路径 | 生效范围 | 需要管理员 |
//! | --- | --- | --- | --- |
//! | `HKEY_CURRENT_USER` | `Software\Classes\...` | 当前用户 | 否 |
//! | `HKEY_LOCAL_MACHINE` | `Software\Classes\...` | 全机 | 是 |
//! | `HKEY_USERS\.DEFAULT` | `Software\Classes\...` | 新用户/系统默认 | 是 |
//!
//! 三个根按「先能写先生效」的顺序尝试：HKCU 一定写得进去，所以**普通权限下
//! 接管也是有效的**；HKLM 与 `.DEFAULT` 写不进去不算失败，只是范围小一点。
//! 落盘之后一律**回读校验**（`RegQueryValueExW`），因为注册表 API 返回
//! `ERROR_SUCCESS` 却因为键被重定向而没真正落到目标位置的情况是存在的。
//!
//! 写入范围严格限定在三个类之下，**不碰** `*`（所有文件）、不碰任何
//! ProgID/FileExts：「接管文件夹」不该顺带改掉文件关联。

use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, KEY_WRITE,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use crate::win::to_wide;

/// 接管状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeoverState {
    /// 已接管，且命令指向当前 exe。
    Active,
    /// 读到了配置，但不是我们写的那一份。
    Inactive,
    /// 三个根都读不到（权限或键不存在）。
    Unknown,
}

/// 一个注册表根。数组顺序即接管时的尝试顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    CurrentUser,
    LocalMachine,
    DefaultProfile,
}

impl Scope {
    pub const ALL: [Scope; 3] = [
        Scope::CurrentUser,
        Scope::LocalMachine,
        Scope::DefaultProfile,
    ];

    fn hkey(self) -> HKEY {
        match self {
            Scope::CurrentUser => HKEY_CURRENT_USER,
            Scope::LocalMachine => HKEY_LOCAL_MACHINE,
            Scope::DefaultProfile => HKEY_USERS,
        }
    }

    fn sub(self) -> String {
        match self {
            Scope::DefaultProfile => r".DEFAULT\Software\Classes".to_string(),
            _ => r"Software\Classes".to_string(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Scope::CurrentUser => "当前用户",
            Scope::LocalMachine => "全机（所有用户）",
            Scope::DefaultProfile => "系统默认配置文件（新用户）",
        }
    }

    fn needs_admin(self) -> bool {
        self != Scope::CurrentUser
    }
}

/// 我方动词名。同时用作菜单显示名的默认值和 `Folder\shell` 的默认值。
const VERB: &str = "DreamFolder";
/// 「在新窗口打开」的备用动词。
const NEWWINDOW: &str = "newwindow";

/// 三个受影响的类（相对 `Software\Classes`）。
const CLASS_FOLDER: &str = r"Folder";
const CLASS_DIRECTORY: &str = r"Directory";
const CLASS_DRIVE: &str = r"Drive";

/// `...\Classes\Folder\shell`
fn folder_shell(scope: Scope) -> String {
    format!(r"{}\{CLASS_FOLDER}\shell", scope.sub())
}
fn directory_verb(scope: Scope) -> String {
    format!(r"{}\{CLASS_DIRECTORY}\shell\{VERB}", scope.sub())
}
fn drive_verb(scope: Scope) -> String {
    format!(r"{}\{CLASS_DRIVE}\shell\{VERB}", scope.sub())
}
fn folder_verb(scope: Scope) -> String {
    format!(r"{}\{VERB}", folder_shell(scope))
}
fn folder_newwindow(scope: Scope) -> String {
    format!(r"{}\{NEWWINDOW}", folder_shell(scope))
}

// ------------------------------------------------------------------ 注册表原语

/// 打开（必要时创建）一个键。
unsafe fn open_or_create(root: HKEY, sub: &str, access: u32) -> Option<HKEY> {
    let sub_w = to_wide(sub);
    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition = 0u32;
    let rc = RegCreateKeyExW(
        root,
        sub_w.as_ptr(),
        0,
        std::ptr::null(),
        REG_OPTION_NON_VOLATILE,
        access,
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

/// 只打开已存在的键（不创建）。
unsafe fn open_existing(root: HKEY, sub: &str, access: u32) -> Option<HKEY> {
    let sub_w = to_wide(sub);
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = RegOpenKeyExW(root, sub_w.as_ptr(), 0, access, &mut hkey);
    if rc == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// 写一个 `REG_SZ` 值（`name = None` 表示该键的默认值）。
unsafe fn write_string(hkey: HKEY, name: Option<&str>, value: &str) -> bool {
    let value_w = to_wide(value);
    let name_w = name.map(to_wide);
    let rc = RegSetValueExW(
        hkey,
        name_w.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
        0,
        REG_SZ,
        value_w.as_ptr() as *const u8,
        (value_w.len() * std::mem::size_of::<u16>()) as u32,
    );
    rc == ERROR_SUCCESS
}

/// 读一个字符串值。不存在或类型不对都返回 `None`。
unsafe fn read_string(root: HKEY, sub: &str, name: Option<&str>) -> Option<String> {
    let hkey = open_existing(root, sub, KEY_READ)?;
    let name_w = name.map(to_wide);
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * std::mem::size_of::<u16>()) as u32;
    let mut ty = 0u32;
    let rc = RegQueryValueExW(
        hkey,
        name_w.as_ref().map(|v| v.as_ptr()).unwrap_or(std::ptr::null()),
        std::ptr::null(),
        &mut ty,
        buf.as_mut_ptr() as *mut u8,
        &mut cb,
    );
    RegCloseKey(hkey);
    if rc != ERROR_SUCCESS || ty != REG_SZ || cb < 2 {
        return None;
    }
    let len = (cb as usize / 2).min(buf.len());
    Some(crate::win::from_wide(&buf[..len]))
}

/// 删除整棵子树。键本就不存在不算错。
unsafe fn delete_tree(root: HKEY, sub: &str) {
    let sub_w = to_wide(sub);
    RegDeleteTreeW(root, sub_w.as_ptr());
}

/// 告诉 Shell「关联变了」，让它刷新图标与动词缓存。
///
/// 不通知的话，接管/恢复之后资源管理器要等到下次登录才认新配置。
fn notify_shell_changed() {
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
}

/// 本程序的可执行文件路径（去掉 verbatim 前缀，Shell 不认）。
fn self_exe() -> String {
    std::env::current_exe()
        .map(|p| crate::fs::strip_verbatim(&p).to_string_lossy().into_owned())
        .unwrap_or_else(|_| "DreamFolder.exe".to_string())
}

/// `"%exe%" "%1"` —— 主命令。
fn open_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// `"%exe%" /OpenInNewWindow "%1"` —— 在新窗口打开。
fn newwindow_command(exe: &str) -> String {
    format!("\"{exe}\" /OpenInNewWindow \"%1\"")
}

// ------------------------------------------------------------------ 状态

/// 检查当前是否已接管。
pub fn state() -> TakeoverState {
    let exe = self_exe();
    for scope in [Scope::CurrentUser, Scope::LocalMachine] {
        let Some(verb) = (unsafe { read_string(scope.hkey(), &folder_shell(scope), None) }) else {
            // 这个根下面没有 `Folder\shell`：未配置，继续看下一个根
            continue;
        };
        if verb.trim() != VERB {
            return TakeoverState::Inactive;
        }
        let cmd = unsafe {
            read_string(
                scope.hkey(),
                &format!(r"{}\command", folder_verb(scope)),
                None,
            )
        };
        let ok = cmd
            .as_deref()
            .map(|c| command_targets(c, &exe))
            .unwrap_or(false);
        return if ok {
            TakeoverState::Active
        } else {
            TakeoverState::Inactive
        };
    }
    TakeoverState::Unknown
}

/// 命令串是否指向 `exe`（只比对程序路径部分，参数不参与）。
fn command_targets(cmd: &str, exe: &str) -> bool {
    let cmd = cmd.trim_start();
    let program = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        cmd.split_whitespace().next().unwrap_or("")
    };
    program.eq_ignore_ascii_case(exe)
}

// ------------------------------------------------------------------ 接管 / 恢复

/// 在某个根下写入全部动词，并回读校验。
fn register_root(scope: Scope, exe: &str) -> Result<(), String> {
    let root = scope.hkey();
    unsafe {
        // 1) 默认动词指向我方的 verb
        let shell = open_or_create(root, &folder_shell(scope), KEY_READ | KEY_WRITE)
            .ok_or_else(|| format!("无法写入 {}（需要管理员权限）", folder_shell(scope)))?;
        let ok = write_string(shell, None, VERB);
        RegCloseKey(shell);
        if !ok {
            return Err(format!("无法设置 {} 的默认动词", folder_shell(scope)));
        }

        // 2) 主动词：菜单显示名 + command
        for (key, title, cmd) in [
            (folder_verb(scope), "用 DreamFolder 打开", open_command(exe)),
            (
                folder_newwindow(scope),
                "在新窗口打开(&E)",
                newwindow_command(exe),
            ),
            (directory_verb(scope), "用 DreamFolder 打开", open_command(exe)),
            (drive_verb(scope), "用 DreamFolder 打开", open_command(exe)),
        ] {
            let h = open_or_create(root, &key, KEY_READ | KEY_WRITE)
                .ok_or_else(|| format!("无法创建 {key}"))?;
            write_string(h, None, title);
            RegCloseKey(h);

            let cmd_key = format!(r"{key}\command");
            let h = open_or_create(root, &cmd_key, KEY_READ | KEY_WRITE)
                .ok_or_else(|| format!("无法创建 {cmd_key}"))?;
            let ok = write_string(h, None, &cmd);
            RegCloseKey(h);
            if !ok {
                return Err(format!("无法写入 {cmd_key}"));
            }
        }
    }

    // 3) 回读校验：写进去了不等于 Shell 会读到我们写的那份
    let verb = unsafe { read_string(root, &folder_shell(scope), None) }.unwrap_or_default();
    if verb.trim() != VERB {
        return Err(format!(
            "写入后回读不一致：{}\\{} 的默认值是「{}」",
            scope.sub(),
            folder_shell(scope),
            verb
        ));
    }
    let cmd = unsafe { read_string(root, &format!(r"{}\command", folder_verb(scope)), None) }
        .unwrap_or_default();
    if !command_targets(&cmd, exe) {
        return Err(format!("写入后回读不一致：命令是「{cmd}」"));
    }
    Ok(())
}

/// 在某个根下抹掉我方的全部痕迹。
fn unregister_root(scope: Scope) {
    let root = scope.hkey();
    unsafe {
        for key in [
            folder_verb(scope),
            folder_newwindow(scope),
            directory_verb(scope),
            drive_verb(scope),
        ] {
            delete_tree(root, &key);
        }
        // 默认动词复位。只在键已经存在时写——为了写一个 "none" 去凭空
        // 造一个 `Folder\shell` 出来没有意义。
        if let Some(shell) = open_existing(root, &folder_shell(scope), KEY_WRITE) {
            write_string(shell, None, "none");
            RegCloseKey(shell);
        }
    }
}

/// 执行接管。
///
/// 返回真正写成功的作用域（至少含 [`Scope::CurrentUser`]，否则就是失败）。
pub fn take_over() -> Result<Vec<Scope>, String> {
    let exe = self_exe();
    let mut done: Vec<Scope> = Vec::new();
    let mut first_error: Option<String> = None;

    for scope in Scope::ALL {
        match register_root(scope, &exe) {
            Ok(()) => done.push(scope),
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }

    if done.is_empty() {
        return Err(first_error.unwrap_or_else(|| "接管失败：没有可写的注册表位置".to_string()));
    }
    notify_shell_changed();
    Ok(done)
}

/// 恢复系统默认（撤销 [`take_over`] 做的全部改动）。
pub fn restore() -> Result<Vec<Scope>, String> {
    let mut done: Vec<Scope> = Vec::new();
    for scope in Scope::ALL {
        // 恢复是「尽量删干净」：某个根写不进去不影响其它根，
        // 也不该让用户看到一条「恢复失败」——他本来就要删东西。
        let before = unsafe { read_string(scope.hkey(), &folder_shell(scope), None) };
        unregister_root(scope);
        let after = unsafe { read_string(scope.hkey(), &folder_shell(scope), None) };
        // 默认值不再是我们的 verb（或者键没了）就算这个根清干净了
        let clean = after.map(|v| v.trim() != VERB).unwrap_or(true);
        if clean && before.is_some() {
            done.push(scope);
        }
    }
    notify_shell_changed();
    Ok(done)
}

/// 接管成功后给用户看的说明。
/// 接管成功但部分范围没写入时，把缺的范围列出来（需要管理员权限的那些）。
///
/// 全部写成功时返回 `None`，调用方就不弹任何窗了。
pub fn admin_missing(scopes: &[Scope]) -> Option<String> {
    let names: Vec<&str> = Scope::ALL
        .iter()
        .copied()
        .filter(|s| !scopes.contains(s) && s.needs_admin())
        .map(|s| s.label())
        .collect();
    if names.is_empty() {
        None
    } else {
        Some(names.join("、"))
    }
}

// ------------------------------------------------------------------ 桌面快捷方式

/// 创建桌面快捷方式（主菜单「添加桌面图标」）。返回生成的 `.lnk` 路径。
///
/// 桌面位置走 `SHGetKnownFolderPath`：桌面被 OneDrive 或组策略重定向后，
/// `%USERPROFILE%\Desktop` 这个路径**根本不存在**——原实现就是在这里失败的。
pub fn create_desktop_shortcut() -> Result<PathBuf, String> {
    let desktop = crate::fs::shell::known_folder(crate::fs::shell::KnownFolder::Desktop)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|p| PathBuf::from(p).join("Desktop"))
                .filter(|p| p.is_dir())
        })
        .ok_or_else(|| "找不到桌面目录".to_string())?;

    let exe = std::env::current_exe().map_err(|e| format!("无法定位本程序：{e}"))?;
    let workdir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let lnk = desktop.join("DreamFolder.lnk");

    // 图标优先用生成好的 .ico（带 logo）；生成失败就让快捷方式用 exe 自己的图标
    let icon = crate::win::appicon::ensure_ico_file();
    crate::win::shortcut::create_link(
        &lnk,
        &exe,
        &workdir,
        icon.as_deref(),
        "DreamFolder 多标签文件管理器",
    )?;
    Ok(lnk)
}
