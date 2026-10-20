//! 桌面快捷方式（`.lnk`）：手写 `IShellLinkW` + `IPersistFile` 的 vtable 调用。
//!
//! 为什么不用 PowerShell：`-Command` 的错误不进退出码——脚本里 `New-Object` 抛
//! 异常时 PowerShell 仍然返回 0，于是「创建失败」看起来像成功；而且拉起一个
//! PowerShell 进程要几百毫秒。
//!
//! 为什么不用 `windows` crate 的 COM 包装层：本项目的既定取舍是纯 `windows-sys`
//! （见 Cargo.toml 的说明），而这两个接口的结构体 windows-sys 没有导出。
//! 所以这里按下标调 vtable，手法与 [`crate::win::shellmenu`] 一致。
//!
//! vtable 布局（shobjidl.h，`IUnknown` 占 0..2）：
//! ```text
//! IShellLinkW : GetPath(3) GetIDList(4) SetIDList(5) GetDescription(6)
//!               SetDescription(7) GetWorkingDirectory(8) SetWorkingDirectory(9)
//!               GetArguments(10) SetArguments(11) GetHotkey(12) SetHotkey(13)
//!               GetShowCmd(14) SetShowCmd(15) GetIconLocation(16)
//!               SetIconLocation(17) SetRelativePath(18) Resolve(19) SetPath(20)
//! IPersistFile: GetClassID(3) IsDirty(4) Load(5) Save(6) SaveCompleted(7) GetCurFile(8)
//! ```

use std::path::Path;

use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

use super::to_wide;

const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x0002_1401_0000_0000_C000_000000000046);
const IID_ISHELL_LINK_W: GUID = GUID::from_u128(0x0002_14F9_0000_0000_C000_000000000046);
const IID_IPERSIST_FILE: GUID = GUID::from_u128(0x0000_010B_0000_0000_C000_000000000046);

/// vtable 下标。
mod idx {
    pub const RELEASE: usize = 2;
    pub const SET_DESCRIPTION: usize = 7;
    pub const SET_WORKING_DIRECTORY: usize = 9;
    pub const SET_ICON_LOCATION: usize = 17;
    pub const SET_PATH: usize = 20;
    pub const SAVE: usize = 6;
}

/// 从对象指针取出 vtable 第 `i` 项并转成函数指针。
///
/// # Safety
/// `this` 必须是一个有效的 COM 接口指针；`i` 必须对应签名 `F` 的实际槽位。
unsafe fn vtbl_fn<F: Copy>(this: *mut core::ffi::c_void, i: usize) -> F {
    let vtbl = *(this as *mut *const usize);
    std::mem::transmute_copy::<usize, F>(&*vtbl.add(i))
}

/// 调 `IUnknown::Release`。
///
/// # Safety
/// 同 [`vtbl_fn`]。
unsafe fn release(this: *mut core::ffi::c_void) {
    if this.is_null() {
        return;
    }
    let f: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 = vtbl_fn(this, idx::RELEASE);
    f(this);
}

/// 创建一个 `.lnk` 快捷方式。
///
/// - `lnk`：要写出的 `.lnk` 文件路径（父目录不存在时自动创建）
/// - `target`：被指向的可执行文件
/// - `workdir`：起始位置（资源管理器里「起始位置」那一栏）
/// - `icon`：图标来源文件；`None` 时用 `target` 自己的图标
/// - `desc`：备注（鼠标悬停时显示）
pub fn create_link(
    lnk: &Path,
    target: &Path,
    workdir: &Path,
    icon: Option<&Path>,
    desc: &str,
) -> Result<(), String> {
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("无法创建目录 {}：{e}", dir.display()))?;
    }

    // 快捷方式里不能写 verbatim 前缀（`\\?\C:\...`），Shell 不认
    let target = crate::fs::strip_verbatim(target);
    let target_w = to_wide(&target.to_string_lossy());
    let workdir_w = to_wide(&workdir.to_string_lossy());
    let desc_w = to_wide(desc);
    let lnk_w = to_wide(&lnk.to_string_lossy());
    let icon_owned = icon
        .map(|p| crate::fs::strip_verbatim(p))
        .map(|p| to_wide(&p.to_string_lossy()));

    unsafe {
        let mut link: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_SHELL_LINK,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ISHELL_LINK_W,
            &mut link,
        );
        if hr < 0 || link.is_null() {
            return Err(format!("无法创建 ShellLink 对象（HRESULT 0x{hr:08X}）"));
        }

        // SetPath(20) / SetDescription(7) / SetWorkingDirectory(9)
        let set_path: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16) -> i32 =
            vtbl_fn(link, idx::SET_PATH);
        if set_path(link, target_w.as_ptr()) < 0 {
            release(link);
            return Err("无法设置快捷方式的目标路径".to_string());
        }
        let set_desc: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16) -> i32 =
            vtbl_fn(link, idx::SET_DESCRIPTION);
        set_desc(link, desc_w.as_ptr());
        let set_wd: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16) -> i32 =
            vtbl_fn(link, idx::SET_WORKING_DIRECTORY);
        set_wd(link, workdir_w.as_ptr());
        // SetIconLocation(17)：第二个参数是图标索引，0 = 第一个
        if let Some(icon_w) = icon_owned.as_ref() {
            let set_icon: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, i32) -> i32 =
                vtbl_fn(link, idx::SET_ICON_LOCATION);
            set_icon(link, icon_w.as_ptr(), 0);
        }

        // QueryInterface(0) → IPersistFile
        let query: unsafe extern "system" fn(
            *mut core::ffi::c_void,
            *const GUID,
            *mut *mut core::ffi::c_void,
        ) -> i32 = vtbl_fn(link, 0);
        let mut persist: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = query(link, &IID_IPERSIST_FILE, &mut persist);
        if hr < 0 || persist.is_null() {
            release(link);
            return Err(format!("无法取到 IPersistFile（HRESULT 0x{hr:08X}）"));
        }

        // Save(6)：pszFileName, fRemember(TRUE)
        let save: unsafe extern "system" fn(*mut core::ffi::c_void, *const u16, i32) -> i32 =
            vtbl_fn(persist, idx::SAVE);
        let hr = save(persist, lnk_w.as_ptr(), 1);
        release(persist);
        release(link);

        if hr < 0 {
            return Err(format!("写入快捷方式失败（HRESULT 0x{hr:08X}）"));
        }
    }

    if !lnk.is_file() {
        return Err(format!("快捷方式没有生成：{}", lnk.display()));
    }
    Ok(())
}
