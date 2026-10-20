//! 系统右键菜单：直接调 Shell 的 `IContextMenu`。
//!
//! 自己画一份菜单永远追不上系统——「发送到」「打开方式」「固定到快速访问」
//! 这些项是随已装软件动态注册的，还包括 WinRAR、TortoiseGit 之类的第三方
//! 扩展。这里把菜单整个交给 Shell，我们只负责在正确的位置把它弹出来、
//! 并把选中的命令号回传给它执行。
//!
//! `IShellFolder` / `IContextMenu` 的 vtable 在 windows-sys 里没有绑定，
//! 所以按 COM 约定的方法顺序取函数指针——顺序取自 shobjidl.h，不能改。

use core::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    CMINVOKECOMMANDINFOEX, CMF_NORMAL, SHBindToParent, SHParseDisplayName, ShellExecuteW,
};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    TrackPopupMenu, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

/// `IShellFolder` 里 `GetUIObjectOf` 的 vtable 槽位。
const ISF_GET_UI_OBJECT_OF: usize = 10;
/// `IShellFolder` 里 `CreateViewObject` 的槽位（拿背景菜单用）。
const ISF_CREATE_VIEW_OBJECT: usize = 8;
/// `IContextMenu` 里 `QueryContextMenu` 的槽位。
const ICM_QUERY_MENU: usize = 3;
/// `IContextMenu` 里 `InvokeCommand` 的槽位。
const ICM_INVOKE: usize = 4;

const IID_ISHELLFOLDER: u128 = 0x0002_14E6_0000_0000_C000_0000_0000_0046;
const IID_ICONTEXTMENU: u128 = 0x0002_14E4_0000_0000_C000_0000_0000_0046;

/// 从 COM 对象的 vtable 里取第 `index` 个方法。
///
/// # Safety
/// `obj` 必须是一个有效的 COM 接口指针，且 `index` 在其 vtable 范围内。
unsafe fn vfn<F>(obj: *mut c_void, index: usize) -> F {
    let vtbl = *(obj as *mut *const usize);
    std::mem::transmute_copy(&*vtbl.add(index))
}

/// 释放一个 COM 接口。
unsafe fn release(obj: *mut c_void) {
    if obj.is_null() {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = vfn(obj, 2);
    f(obj);
}

/// 当前光标的屏幕坐标。菜单要弹在鼠标处。
fn cursor_pos() -> POINT {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    pt
}

/// 对 `paths` 弹出系统右键菜单。
///
/// `paths` 为空表示在 `dir` 的空白处右键——此时弹的是「背景菜单」，
/// 里面有「新建」「粘贴」「属性」这些作用于文件夹本身的项。
pub fn show(hwnd: HWND, paths: &[PathBuf], dir: Option<&Path>) -> bool {
    unsafe {
        if paths.is_empty() {
            match dir {
                Some(d) => show_background(hwnd, d),
                None => false,
            }
        } else {
            show_for_items(hwnd, paths)
        }
    }
}

/// 拖动条目的右键菜单。
unsafe fn show_for_items(hwnd: HWND, paths: &[PathBuf]) -> bool {
    // 同一目录下的项才能放进一次菜单调用；取第一个的父目录，
    // 其余的若不在同一目录就只处理同目录那部分（资源管理器也这么做）
    let first = match paths.first() {
        Some(p) => p,
        None => return false,
    };
    // 盘符根（"C:\"）没有父目录：`Path::parent` 直接返回 None。
    // 以前这里就 return false 了，于是右键磁盘什么菜单都弹不出来。
    // 这种情况下不按「同目录」过滤，交给 SHBindToParent 自己找父命名空间
    // （盘符的父是「此电脑」，Shell 会给出完整的那套菜单）。
    let parent = first.parent().map(|p| p.to_path_buf());

    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
    let mut seen_children: Vec<*const ITEMIDLIST> = Vec::new();
    let mut folder: *mut c_void = std::ptr::null_mut();

    for p in paths {
        if let Some(dir) = parent.as_deref() {
            if p.parent() != Some(dir) {
                continue;
            }
        }
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let wide = crate::win::to_wide(&p.to_string_lossy());
        let hr = SHParseDisplayName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        );
        if hr < 0 || pidl.is_null() {
            continue;
        }
        pidls.push(pidl);

        // 拿父 IShellFolder 和该项自己的 child PIDL（child 指向 pidl 内部，不能单独释放）
        let iid = GUID::from_u128(IID_ISHELLFOLDER);
        let mut sf: *mut c_void = std::ptr::null_mut();
        let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
        let hr = SHBindToParent(pidl, &iid, &mut sf, &mut child);
        if hr == 0 && !sf.is_null() {
            if folder.is_null() {
                folder = sf;
            } else {
                release(sf);
            }
            seen_children.push(child as *const ITEMIDLIST);
        }
    }
    if folder.is_null() || seen_children.is_empty() {
        for p in &pidls {
            CoTaskMemFree(*p as *mut c_void);
        }
        return false;
    }

    // 取 IContextMenu
    let iid_cm = GUID::from_u128(IID_ICONTEXTMENU);
    let mut pcm: *mut c_void = std::ptr::null_mut();
    let get_ui: unsafe extern "system" fn(
        *mut c_void, HWND, u32, *const *const ITEMIDLIST, *const GUID, *mut u32, *mut *mut c_void,
    ) -> i32 = vfn(folder, ISF_GET_UI_OBJECT_OF);
    let hr = get_ui(
        folder,
        hwnd,
        seen_children.len() as u32,
        seen_children.as_ptr(),
        &iid_cm,
        std::ptr::null_mut(),
        &mut pcm,
    );

    for p in &pidls {
        CoTaskMemFree(*p as *mut c_void);
    }
    release(folder);

    if hr < 0 || pcm.is_null() {
        return false;
    }

    let ok = popup_and_invoke(hwnd, pcm);
    release(pcm);
    ok
}

/// 目录空白处的背景菜单。
unsafe fn show_background(hwnd: HWND, dir: &Path) -> bool {
    // 把目录本身解析成 PIDL，再绑出它自己的 IShellFolder
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    let wide = crate::win::to_wide(&dir.to_string_lossy());
    let hr = SHParseDisplayName(
        wide.as_ptr(),
        std::ptr::null_mut(),
        &mut pidl,
        0,
        std::ptr::null_mut(),
    );
    if hr < 0 || pidl.is_null() {
        return false;
    }

    let iid_sf = GUID::from_u128(IID_ISHELLFOLDER);
    let mut parent: *mut c_void = std::ptr::null_mut();
    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
    let hr = SHBindToParent(pidl, &iid_sf, &mut parent, &mut child);
    if hr < 0 || parent.is_null() {
        CoTaskMemFree(pidl as *mut c_void);
        return false;
    }

    // 父 → 该目录自己的 IShellFolder
    let bind: unsafe extern "system" fn(
        *mut c_void, *const ITEMIDLIST, *mut c_void, *const GUID, *mut *mut c_void,
    ) -> i32 = vfn(parent, 5);
    let mut folder: *mut c_void = std::ptr::null_mut();
    let hr = bind(parent, child, std::ptr::null_mut(), &iid_sf, &mut folder);
    release(parent);
    CoTaskMemFree(pidl as *mut c_void);
    if hr < 0 || folder.is_null() {
        return false;
    }

    // CreateViewObject 拿到的就是背景菜单
    let iid_cm = GUID::from_u128(IID_ICONTEXTMENU);
    let mut pcm: *mut c_void = std::ptr::null_mut();
    let create_view: unsafe extern "system" fn(
        *mut c_void, HWND, *const GUID, *mut *mut c_void,
    ) -> i32 = vfn(folder, ISF_CREATE_VIEW_OBJECT);
    let hr = create_view(folder, hwnd, &iid_cm, &mut pcm);
    release(folder);

    if hr < 0 || pcm.is_null() {
        return false;
    }
    let ok = popup_and_invoke(hwnd, pcm);
    release(pcm);
    ok
}

/// 弹菜单并把选中项交回 Shell 执行。返回是否真的执行了命令。
unsafe fn popup_and_invoke(hwnd: HWND, pcm: *mut c_void) -> bool {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return false;
    }

    // 1 起编号：InvokeCommand 里的 verb 就是这个号减去 first
    const ID_FIRST: u32 = 1;
    const ID_LAST: u32 = 0x7FFF;

    let query: unsafe extern "system" fn(*mut c_void, isize, u32, u32, u32, u32) -> i32 =
        vfn(pcm, ICM_QUERY_MENU);
    let hr = query(pcm, menu as isize, 0, ID_FIRST, ID_LAST, CMF_NORMAL);
    if hr < 0 {
        DestroyMenu(menu);
        return false;
    }

    let pt = cursor_pos();
    // 必须先把窗口弄到前台，否则点菜单外面菜单不会消失
    PostMessageW(hwnd, 0x0000_0000, 0, 0); // WM_NULL，让前台队列醒一下
    SetForegroundWindow(hwnd);

    // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
    crate::win::init::apply_menu_theme();
    let cmd = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    DestroyMenu(menu);

    if cmd == 0 {
        return false;
    }

    // 交给 Shell 执行。verb 用序号形式（低位字 = 命令号 - ID_FIRST）
    let mut info: CMINVOKECOMMANDINFOEX = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32;
    info.hwnd = hwnd;
    info.lpVerb = ((cmd as usize - ID_FIRST as usize) & 0xFFFF) as *const u8;
    info.nShow = SW_SHOWNORMAL as i32;
    // CMIC_MASK_UNICODE：表示要按宽字符解析 lpVerbW，不看 lpVerb
    info.fMask = 0x0000_4000;

    let invoke: unsafe extern "system" fn(*mut c_void, *const CMINVOKECOMMANDINFOEX) -> i32 =
        vfn(pcm, ICM_INVOKE);
    let hr = invoke(pcm, &info);
    hr == 0
}

/// 用系统默认程序打开一个路径。菜单里「打开」之外的兜底路径。
pub fn open_with_system(path: &Path) {
    let wide = crate::win::to_wide(&path.to_string_lossy());
    let op = crate::win::to_wide("open");
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}
