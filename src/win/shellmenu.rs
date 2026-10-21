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
use windows_sys::Win32::Foundation::{HWND, POINT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, FillRect, GetDC, GetSysColor,
    GetTextExtentPoint32W, GetTextMetricsW, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    TextOutW, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_MENUTEXT, HDC, HFONT, TEXTMETRICW,
    TRANSPARENT,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED};
use windows_sys::Win32::UI::Shell::{
    CMINVOKECOMMANDINFOEX, CMF_NORMAL, SHBindToParent, SHParseDisplayName,
};
use windows_sys::Win32::UI::Shell::Common::ITEMIDLIST;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, DrawIconEx, GetCursorPos,
    GetMenuItemCount, GetMenuStringW, GetSubMenu, InsertMenuW, PostMessageW, RemoveMenu,
    SetForegroundWindow, SetMenuItemInfoW, SystemParametersInfoW, TrackPopupMenu, DI_NORMAL, HICON, HMENU,
    MENUITEMINFOW, MF_BYPOSITION, MF_OWNERDRAW, MF_POPUP, MIIM_DATA, MIIM_FTYPE, MFT_OWNERDRAW,
    NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

use super::shellnew;

/// `IShellFolder` 里 `GetUIObjectOf` 的 vtable 槽位。
const ISF_GET_UI_OBJECT_OF: usize = 10;
/// `IShellFolder` 里 `CreateViewObject` 的槽位（拿背景菜单用）。
const ISF_CREATE_VIEW_OBJECT: usize = 8;
/// `IContextMenu` 里 `QueryContextMenu` 的槽位。
const ICM_QUERY_MENU: usize = 3;
/// `IContextMenu` 里 `InvokeCommand` 的槽位。
const ICM_INVOKE: usize = 4;
/// `IContextMenu2` 里 `HandleMenuMsg` 的槽位。
const ICM_HANDLE_MSG: usize = 5;
/// `IContextMenu3` 里 `HandleMenuMsg2` 的槽位。
const ICM_HANDLE_MSG2: usize = 6;
/// `CMINVOKECOMMANDINFOEX.fMask` 的宽字符标志。
const CMIC_MASK_UNICODE: u32 = 0x0000_4000;
/// 菜单项编号的起点。`InvokeCommand` 里的 verb 就是这个号减去它。
const ID_FIRST: u32 = 1;
/// 菜单项编号的上限。`QueryContextMenu` 要求 `ID_LAST - ID_FIRST < 0x7FFF`。
const ID_LAST: u32 = 0x7FFF;

const IID_ISHELLFOLDER: u128 = 0x0002_14E6_0000_0000_C000_0000_0000_0046;
const IID_ICONTEXTMENU: u128 = 0x0002_14E4_0000_0000_C000_0000_0000_0046;
/// `{000214F4-0000-0000-C000-000000000046}`
const IID_ICONTEXTMENU2: u128 = 0x0002_14F4_0000_0000_C000_0000_0000_0046;
/// `{BCFCE0A0-EC17-11D0-8D10-00A0C90F2719}`
const IID_ICONTEXTMENU3: u128 = 0xBCFC_E0A0_EC17_11D0_8D10_00A0_C90F_2719;

thread_local! {
    /// 正在弹的菜单：`(IContextMenu2 或 3 的指针, 是否支持 HandleMenuMsg2)`。
    ///
    /// `TrackPopupMenu` 的模态循环把 `WM_INITMENUPOPUP` 发到我们窗口，
    /// 而窗口过程没有别的路子拿到这个接口指针，只能在这里暂存一份。
    static ACTIVE_MENU: std::cell::Cell<Option<(*mut c_void, bool)>> =
        std::cell::Cell::new(None);
}

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

/// `IUnknown::QueryInterface`。不支持该接口时返回空指针。
unsafe fn query_interface(obj: *mut c_void, iid: u128) -> *mut c_void {
    let f: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 = vfn(obj, 0);
    let g = GUID::from_u128(iid);
    let mut out: *mut c_void = std::ptr::null_mut();
    let hr = f(obj, &g, &mut out);
    if hr == 0 {
        out
    } else {
        std::ptr::null_mut()
    }
}

/// 把菜单消息转发给 Shell 的 `IContextMenu2/3`。
///
/// 这一步不是可选的：`QueryContextMenu` 只铺第一层项，**子菜单要靠
/// `WM_INITMENUPOPUP` 通知才有内容**。「新建」的整棵树（文件夹 / 文本文档 /
/// 各软件注册的 ShellNew 类型）就是这样填出来的——不转发就是空菜单，
/// 用户点「新建」什么都看不到。`WM_DRAWITEM` / `WM_MEASUREITEM` 同理，
/// 自绘项（带图标或自定义高度的项）也走这条路。
///
/// 返回 `Some(结果)` 表示 Shell 已处理，调用方应直接返回该值；
/// `None` 表示没有菜单在弹、或这条消息 Shell 不管。
pub fn forward_menu_message(msg: u32, wparam: usize, lparam: isize) -> Option<isize> {
    // 我们自己画的那几项得先接住：Shell 不认识它们的编号，转过去也是白转
    if let Some(r) = own_menu_message(msg, wparam, lparam) {
        return Some(r);
    }
    // 我们自己的子菜单不用 Shell 操心：内容是自己填好的，转过去只会
    // 让它把它的条目又塞进来一份
    if msg == windows_sys::Win32::UI::WindowsAndMessaging::WM_INITMENUPOPUP
        && is_own_submenu(wparam)
    {
        return None;
    }
    let (pcm, has_msg2) = ACTIVE_MENU.with(|c| c.get())?;
    if pcm.is_null() {
        return None;
    }
    unsafe {
        if has_msg2 {
            let f: unsafe extern "system" fn(
                *mut c_void,
                u32,
                usize,
                isize,
                *mut isize,
            ) -> i32 = vfn(pcm, ICM_HANDLE_MSG2);
            let mut result: isize = 0;
            if f(pcm, msg, wparam, lparam, &mut result) == 0 {
                return Some(result);
            }
        } else {
            let f: unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> i32 =
                vfn(pcm, ICM_HANDLE_MSG);
            if f(pcm, msg, wparam, lparam) == 0 {
                return Some(0);
            }
        }
    }
    None
}

/// 当前光标的屏幕坐标。菜单要弹在鼠标处。
fn cursor_pos() -> POINT {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
    }
    pt
}

// ------------------------------------------------------------ 自绘的新建项

/// 一个我们自己的菜单项：文本 + 图标。
struct OwnItem {
    label: String,
    icon: HICON,
}

struct OwnMenu {
    /// 「新建」这一项本身（子菜单的父项），也要自绘才有图标
    parent: OwnItem,
    items: Vec<OwnItem>,
    /// 我们自己的子菜单句柄。收到它的 `WM_INITMENUPOPUP` 要拦住，
    /// 不能转给 Shell——那是 Shell 用来往「它的」子菜单里补内容的信号。
    submenu: HMENU,
    /// 被我们摘下来的 Shell 原「新建」子菜单。
    ///
    /// 用 `RemoveMenu` 摘下来时它是活的，Shell 那边可能还记着这个句柄；
    /// 一直留到整个菜单关闭（guard drop）才销毁，避免出现悬空句柄。
    shell_submenu: HMENU,
    /// 打开主题数据用的窗口（`OpenThemeData` 需要）
    owner: HWND,
    /// 菜单打开期间持有的字体
    font: HFONT,
}

/// 父项在被问「怎么画」时靠 `dwItemData` 认出自己：弹出项的 `itemID`
/// 是子菜单句柄，没法用编号区间判断。
const OWN_PARENT_DATA: usize = 0x00DF_FFFE;

thread_local! {
    /// 当前打开着的、我们自己那层子菜单。同一时刻只会有一个。
    static OWN_MENU: std::cell::RefCell<Option<OwnMenu>> =
        const { std::cell::RefCell::new(None) };
}

/// 装好自绘菜单的状态；返回的守卫在 `Drop` 时拆掉（连同图标一起销毁）。
struct OwnMenuGuard;

impl Drop for OwnMenuGuard {
    fn drop(&mut self) {
        OWN_MENU.with(|c| {
            if let Some(menu) = c.borrow_mut().take() {
                for item in menu.items {
                    if !item.icon.is_null() {
                        unsafe { DestroyIcon(item.icon) };
                    }
                }
                if !menu.parent.icon.is_null() {
                    unsafe { DestroyIcon(menu.parent.icon) };
                }
                if !menu.shell_submenu.is_null() {
                    // 摘下来的 Shell 子菜单：到这里才安全销毁
                    unsafe { DestroyMenu(menu.shell_submenu) };
                }
                if !menu.font.is_null() {
                    unsafe { DeleteObject(menu.font as *mut c_void) };
                }
            }
        });
    }
}

fn install_own_menu(
    title: &str,
    kinds: &[shellnew::NewKind],
    submenu: HMENU,
    shell_submenu: HMENU,
    owner: HWND,
) -> OwnMenuGuard {
    let items: Vec<OwnItem> = kinds
        .iter()
        .map(|k| OwnItem {
            label: k.label.clone(),
            icon: shellnew::icon(k),
        })
        .collect();
    let parent = OwnItem {
        label: title.to_string(),
        icon: shellnew::icon(&shellnew::folder_kind()),
    };
    let font = unsafe { menu_font() };
    OWN_MENU.with(|c| {
        *c.borrow_mut() = Some(OwnMenu {
            parent,
            items,
            submenu,
            shell_submenu,
            owner,
            font,
        });
    });
    OwnMenuGuard
}

/// 菜单字体：跟系统菜单一致，项的高度才对得上邻居。
unsafe fn menu_font() -> HFONT {
    let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
    ncm.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
    if SystemParametersInfoW(
        SPI_GETNONCLIENTMETRICS,
        ncm.cbSize,
        &mut ncm as *mut _ as *mut c_void,
        0,
    ) != 0
    {
        CreateFontIndirectW(&ncm.lfMenuFont)
    } else {
        std::ptr::null_mut()
    }
}

/// 接住我们自己的项的 `WM_MEASUREITEM` / `WM_DRAWITEM`。
fn own_menu_message(msg: u32, _wparam: usize, lparam: isize) -> Option<isize> {
    use windows_sys::Win32::UI::Controls::ODT_MENU;
    use windows_sys::Win32::UI::WindowsAndMessaging::{WM_DRAWITEM, WM_MEASUREITEM};

    if msg != WM_MEASUREITEM && msg != WM_DRAWITEM || lparam == 0 {
        return None;
    }
    // 两个结构体的 `itemID` / `itemData` 偏移不同（中间夹着 hwnd/hDC/rcItem），
    // 所以必须按各自的布局取，不能按公共前缀硬读。
    // `CtlType` 必须一起校验：`WM_DRAWITEM` 是所有自绘控件共用的消息，
    // 只按 itemID 认领，将来任何一个自绘控件的编号落进我们的区间都会被截胡。
    let (ctl_type, item_id, item_data) = unsafe {
        if msg == WM_MEASUREITEM {
            let mis = &*(lparam as *const MEASUREITEMSTRUCT);
            (mis.CtlType, mis.itemID, mis.itemData)
        } else {
            let dis = &*(lparam as *const DRAWITEMSTRUCT);
            (dis.CtlType, dis.itemID, dis.itemData)
        }
    };
    if ctl_type != ODT_MENU {
        return None;
    }
    let which = own_item_of(item_id, item_data)?;
    OWN_MENU.with(|c| {
        let menu = c.borrow();
        let menu = menu.as_ref()?;
        let item = match which {
            OwnWhich::Parent => &menu.parent,
            OwnWhich::Child(i) => menu.items.get(i)?,
        };
        unsafe {
            if msg == WM_MEASUREITEM {
                measure_own_item(lparam, item, menu);
            } else {
                draw_own_item(lparam, item, menu);
            }
        }
        Some(1)
    })
}

enum OwnWhich {
    Parent,
    Child(usize),
}

/// 这个菜单句柄是不是我们自己的子菜单。
fn is_own_submenu(handle: usize) -> bool {
    OWN_MENU.with(|c| {
        c.borrow()
            .as_ref()
            .map(|m| m.submenu as usize == handle)
            .unwrap_or(false)
    })
}

/// 判断这条绘制消息是不是在问我们自己的项。
///
/// 父项（「新建」那一行）是弹出项，`itemID` 是 0，只能靠 `dwItemData`
/// 认；子项则靠编号区间。
fn own_item_of(item_id: u32, item_data: usize) -> Option<OwnWhich> {
    if item_data == OWN_PARENT_DATA {
        return Some(OwnWhich::Parent);
    }
    if item_id >= shellnew::OWN_ID_BASE {
        return Some(OwnWhich::Child((item_id - shellnew::OWN_ID_BASE) as usize));
    }
    None
}

unsafe fn measure_own_item(lparam: isize, item: &OwnItem, menu: &OwnMenu) {
    let mis = &mut *(lparam as *mut MEASUREITEMSTRUCT);
    let dc = GetDC(std::ptr::null_mut());
    let old = SelectObject(dc, menu.font as *mut c_void);
    let width = text_width(dc, &item.label);
    SelectObject(dc, old);
    ReleaseDC(std::ptr::null_mut(), dc);

    let icon = icon_metric();
    let pad = menu_pad();
    // 左边距 + 图标 + 间隙 + 文本 + 右边距（右对齐是为了对齐快捷键那一列）
    mis.itemWidth = (icon + width + 3 * pad) as u32;
    mis.itemHeight = (icon + pad) as u32;
}

unsafe fn draw_own_item(lparam: isize, item: &OwnItem, menu: &OwnMenu) {
    let dis = &*(lparam as *const DRAWITEMSTRUCT);
    let dc = dis.hDC;
    let r = dis.rcItem;
    let hot = dis.itemState & ODS_SELECTED as u32 != 0;

    // 配色一律问当前菜单主题要，**不用应用自己的调色板**：
    // 系统菜单的外壳由 uxtheme 画（`apply_menu_theme` 只是把它掰到应用的
    // 明暗一侧），拿应用调色板去画这几行，赶上外壳实际是另一侧时就成了
    // 浅底浅字。主题取不到才退回 `GetSysColor` 兜底。
    let text_color = theme_menu_item(dc, menu.owner, &r, hot);
    SetTextColor(dc, text_color);
    SetBkMode(dc, TRANSPARENT as i32);

    let icon = icon_metric();
    let pad = menu_pad();
    if !item.icon.is_null() {
        DrawIconEx(
            dc,
            r.left + pad,
            r.top + (r.bottom - r.top - icon) / 2,
            item.icon,
            icon,
            icon,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        );
    }

    let old = SelectObject(dc, menu.font as *mut c_void);
    let text_x = r.left + icon + 2 * pad;
    let mut text: Vec<u16> = item.label.encode_utf16().collect();
    TextOutW(
        dc,
        text_x,
        r.top + (r.bottom - r.top - text_height(dc)) / 2,
        text.as_mut_ptr(),
        text.len() as i32,
    );
    SelectObject(dc, old);
}

/// 按当前菜单主题画一项的底与文字色，返回可用的文字颜色。
///
/// 主题里 `MENU_POPUPITEM` 的 `MPI_HOT` 就是「鼠标划过/键盘选中」那一行：
/// 底色由 `DrawThemeBackground` 画（深色主题下它是深色高亮），文字色从
/// `TMT_TEXTCOLOR` 取。主题不可用（经典主题 / 取不到）才退回系统色。
unsafe fn theme_menu_item(dc: HDC, owner: HWND, r: &windows_sys::Win32::Foundation::RECT, hot: bool) -> u32 {
    use windows_sys::Win32::UI::Controls::{
        CloseThemeData, DrawThemeBackground, GetThemeColor, OpenThemeData, HTHEME, MENU_POPUPITEM,
        MPI_HOT, MPI_NORMAL, TMT_TEXTCOLOR,
    };

    let class = crate::win::to_wide("Menu");
    let ht: HTHEME = OpenThemeData(owner, class.as_ptr());
    if ht != 0 {
        let state = if hot { MPI_HOT } else { MPI_NORMAL };
        // 高亮底交给主题画：深色主题下它是一层半透明的深色块，
        // 自己填纯色反而会跟邻居不一样
        let mut drawn = true;
        if hot {
            drawn = DrawThemeBackground(ht, dc, MENU_POPUPITEM, state, r, std::ptr::null()) >= 0;
        }
        let mut color: u32 = 0;
        let ok = GetThemeColor(ht, MENU_POPUPITEM, state, TMT_TEXTCOLOR as i32, &mut color) >= 0;
        CloseThemeData(ht);
        if ok && drawn {
            return color;
        }
        // 主题缺这一项（老系统/自定义主题）：底色没画成的话，下面兜底补上
        if hot {
            let brush = CreateSolidBrush(GetSysColor(COLOR_HIGHLIGHT));
            FillRect(dc, r, brush);
            DeleteObject(brush as *mut c_void);
        }
        if ok {
            return color;
        }
    }

    // 兜底：系统菜单色
    if hot {
        let brush = CreateSolidBrush(GetSysColor(COLOR_HIGHLIGHT));
        FillRect(dc, r, brush);
        DeleteObject(brush as *mut c_void);
        GetSysColor(COLOR_HIGHLIGHTTEXT)
    } else {
        GetSysColor(COLOR_MENUTEXT)
    }
}

/// 菜单项左右内边距：跟着图标尺寸走，高 DPI 下才不会挤成一团。
fn menu_pad() -> i32 {
    (icon_metric() / 2).max(4)
}

fn icon_metric() -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    unsafe { GetSystemMetrics(SM_CXSMICON) }
}

unsafe fn text_width(dc: HDC, text: &str) -> i32 {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut size: SIZE = std::mem::zeroed();
    GetTextExtentPoint32W(dc, wide.as_mut_ptr(), wide.len() as i32, &mut size);
    size.cx
}

unsafe fn text_height(dc: HDC) -> i32 {
    let mut tm: TEXTMETRICW = std::mem::zeroed();
    GetTextMetricsW(dc, &mut tm);
    tm.tmHeight
}

/// 对 `paths` 弹出系统右键菜单。
///
/// `paths` 为空表示在 `dir` 的空白处右键——此时弹的是「背景菜单」，
/// 里面有「新建」「粘贴」「属性」这些作用于文件夹本身的项。
pub fn show(hwnd: HWND, paths: &[PathBuf], dir: Option<&Path>) -> Invocation {
    unsafe {
        if paths.is_empty() {
            match dir {
                Some(d) => show_background(hwnd, d),
                None => Invocation::default(),
            }
        } else {
            show_for_items(hwnd, paths)
        }
    }
}

/// 拖动条目的右键菜单。
unsafe fn show_for_items(hwnd: HWND, paths: &[PathBuf]) -> Invocation {
    // 同一目录下的项才能放进一次菜单调用；取第一个的父目录，
    // 其余的若不在同一目录就只处理同目录那部分（资源管理器也这么做）
    let first = match paths.first() {
        Some(p) => p,
        None => return Invocation::default(),
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
        return Invocation::default();
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
        return Invocation::default();
    }

    let (inv, own) = popup_and_invoke(hwnd, pcm, None);
    release(pcm);
    // Shell 的菜单对象已经释放，被接管过来的子菜单到这里才安全销毁
    drop(own);
    inv
}

/// 目录空白处的背景菜单。
unsafe fn show_background(hwnd: HWND, dir: &Path) -> Invocation {
    let Some(pcm) = background_context_menu(hwnd, dir) else {
        return Invocation::default();
    };
    let (inv, own) = popup_and_invoke(hwnd, pcm, Some(dir));
    release(pcm);
    drop(own);
    inv
}

/// 取某个目录自己的 `IContextMenu`（也就是背景菜单）。调用方负责 `release`。
///
/// 单独拆出来是为了能被测试直接调到：新建的整条链路（Shell 建文件 →
/// 我们再补重命名）只有在真 Shell 上跑一遍才算验证过。
unsafe fn background_context_menu(hwnd: HWND, dir: &Path) -> Option<*mut c_void> {
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
        return None;
    }

    let iid_sf = GUID::from_u128(IID_ISHELLFOLDER);
    let mut parent: *mut c_void = std::ptr::null_mut();
    let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
    let hr = SHBindToParent(pidl, &iid_sf, &mut parent, &mut child);
    if hr < 0 || parent.is_null() {
        CoTaskMemFree(pidl as *mut c_void);
        return None;
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
        return None;
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
        return None;
    }
    Some(pcm)
}

/// 一次菜单交互的结果。
#[derive(Clone, Default)]
pub struct Invocation {
    /// 我们自己建出来的新条目文件名（没有新建就是 `None`）。
    ///
    /// 资源管理器新建完立刻进入重命名，是因为它的视图能收到 Shell 的通知；
    /// 我们不是 Shell 的 `IShellView`，收不到。这里直接把名字交回去，
    /// 调用方枚举落地后按名字选中并补上重命名。
    pub created_new: Option<String>,
    /// 点了「新建」但没建成时的错误。
    ///
    /// 必须单独一个字段：`created_new == None` 既可能是「没点新建」，
    /// 也可能是「点了但失败」（只读目录、无权限、磁盘满、模板缺失……），
    /// 混在一起的结果就是用户点了没反应也看不到原因。
    pub new_error: Option<String>,
}

/// 弹菜单并把选中项交回 Shell 执行。
///
/// `dir` 是背景菜单作用的目录：只有它存在时才有「新建」可用，也才由我们
/// 自己执行新建（原因见 [`crate::win::shellnew`]）。
///
/// 返回的守卫要由调用方 **在 `release(pcm)` 之后** 丢弃：它负责销毁从
/// Shell 手里接管过来的原「新建」子菜单，必须活到 `IContextMenu` 释放为止。
unsafe fn popup_and_invoke(
    hwnd: HWND,
    pcm: *mut c_void,
    dir: Option<&Path>,
) -> (Invocation, Option<OwnMenuGuard>) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return (Invocation::default(), None);
    }

    let query: unsafe extern "system" fn(*mut c_void, isize, u32, u32, u32, u32) -> i32 =
        vfn(pcm, ICM_QUERY_MENU);
    let hr = query(pcm, menu as isize, 0, ID_FIRST, ID_LAST, CMF_NORMAL);
    if hr < 0 {
        DestroyMenu(menu);
        return (Invocation::default(), None);
    }

    // 把 Shell 那个必然失败的「新建」换成我们自己的
    let (kinds, own) = match dir {
        Some(_) => replace_new_submenu(menu, hwnd),
        None => (Vec::new(), None),
    };

    let pt = cursor_pos();
    // 必须先把窗口弄到前台，否则点菜单外面菜单不会消失
    PostMessageW(hwnd, 0x0000_0000, 0, 0); // WM_NULL，让前台队列醒一下
    SetForegroundWindow(hwnd);

    // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
    crate::win::init::apply_menu_theme();

    // 子菜单要有人喂消息才有内容（见 `forward_menu_message`）：
    // 优先 IContextMenu3，退回 IContextMenu2，都没有就照旧弹。
    let ext = {
        let p = query_interface(pcm, IID_ICONTEXTMENU3);
        if !p.is_null() {
            Some((p, true))
        } else {
            let p = query_interface(pcm, IID_ICONTEXTMENU2);
            if !p.is_null() {
                Some((p, false))
            } else {
                None
            }
        }
    };
    if let Some(active) = ext {
        ACTIVE_MENU.with(|c| c.set(Some(active)));
    }

    let cmd = TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        std::ptr::null(),
    );

    // 菜单收起来了，指针不能留在这里
    ACTIVE_MENU.with(|c| c.set(None));
    if let Some((p, _)) = ext {
        release(p);
    }

    if cmd == 0 {
        DestroyMenu(menu);
        return (Invocation::default(), own);
    }

    DestroyMenu(menu);

    // 我们自己的「新建」项：编号在我们的私有区间里，自己建文件
    if cmd as u32 >= shellnew::OWN_ID_BASE {
        let index = (cmd as u32 - shellnew::OWN_ID_BASE) as usize;
        let inv = match (dir, kinds.get(index)) {
            (Some(dir), Some(kind)) => match shellnew::create(dir, kind) {
                Ok(name) => Invocation {
                    created_new: Some(name),
                    new_error: None,
                },
                Err(err) => Invocation {
                    created_new: None,
                    new_error: Some(format!("{}：{err}", kind.label)),
                },
            },
            // 编号对不上条目：只可能是内部状态错乱，同样要说出来
            _ => Invocation {
                created_new: None,
                new_error: Some("菜单项与新建类型失去对应".to_string()),
            },
        };
        return (inv, own);
    }

    let _ = invoke_command(pcm, cmd as u32, hwnd);
    (Invocation::default(), own)
}

/// 用我们自己的「新建」子菜单顶掉 Shell 那一个，返回子菜单里的条目。
///
/// Shell 的子菜单里其实已经有完整的类型列表，但那些命令依赖宿主的
/// `IShellView`，在我们这里一律 `E_FAIL`（见 [`shellnew`] 的说明）。
/// 这里保留它在菜单里的**位置和标题**（标题直接复用 Shell 的，本地化和
/// 访问键都跟着系统走），只把内容换成我们自己能执行的那几个。
unsafe fn replace_new_submenu(menu: HMENU, owner: HWND) -> (Vec<shellnew::NewKind>, Option<OwnMenuGuard>) {
    let Some((pos, shell_submenu)) = new_submenu(menu) else {
        return (Vec::new(), None);
    };
    let title = menu_item_text(menu, pos);

    let mut kinds = vec![shellnew::folder_kind()];
    kinds.extend(shellnew::collect());

    let own = CreatePopupMenu();
    if own.is_null() {
        return (Vec::new(), None);
    }
    for i in 0..kinds.len() {
        // MF_OWNERDRAW：系统会把尺寸和绘制都问回我们（`WM_MEASUREITEM` /
        // `WM_DRAWITEM`），图标和配色才能跟主题对上。`lpNewItem` 传条目下标。
        AppendMenuW(
            own,
            MF_OWNERDRAW,
            (shellnew::OWN_ID_BASE + i as u32) as usize,
            i as usize as *const u16,
        );
    }

    // 摘掉 Shell 那个子菜单。这里必须用 `RemoveMenu` 而不是 `DeleteMenu`：
    // DeleteMenu 会把子菜单句柄一并销毁，而那个子菜单是 shell32 在
    // `QueryContextMenu` 里建好、挂上来的，它内部可能还记着这个句柄
    // （每次 `WM_INITMENUPOPUP` 都要往里补内容）。提前销毁等于给 Shell
    // 留个悬空句柄，什么时候炸取决于它下次什么时候用。
    // 用 RemoveMenu 只摘不销毁，句柄交给下面的 guard，等整个菜单关掉再放。
    RemoveMenu(menu, pos as u32, MF_BYPOSITION);
    let title_w = crate::win::to_wide(&title);
    if InsertMenuW(
        menu,
        pos as u32,
        MF_BYPOSITION | MF_POPUP,
        own as usize,
        title_w.as_ptr(),
    ) == 0
    {
        // 插不回去就把自己的菜单丢掉，宁可没有「新建」也不要留个悬空句柄
        DestroyMenu(own);
        DestroyMenu(shell_submenu);
        return (Vec::new(), None);
    }

    // 父项也改自绘：要不然只有子项有图标，缩进一眼就不齐
    let mut info: MENUITEMINFOW = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
    info.fMask = MIIM_FTYPE | MIIM_DATA;
    info.fType = MFT_OWNERDRAW;
    info.dwItemData = OWN_PARENT_DATA;
    SetMenuItemInfoW(menu, pos as u32, 1, &info);

    let guard = install_own_menu(&title, &kinds, own, shell_submenu, owner);
    (kinds, Some(guard))
}

/// 把命令号交回 Shell 执行。`cmd` 是 `TrackPopupMenu` 返回的菜单项 ID。
unsafe fn invoke_command(pcm: *mut c_void, cmd: u32, hwnd: HWND) -> i32 {
    // 交给 Shell 执行。verb 用序号形式（低位字 = 命令号 - ID_FIRST）
    let verb = (cmd as usize).wrapping_sub(ID_FIRST as usize) & 0xFFFF;
    let mut info: CMINVOKECOMMANDINFOEX = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32;
    info.hwnd = hwnd;
    // MAKEINTRESOURCE：低位字是命令号，靠 `ptr < 0x10000` 这个约定和字符串区分开
    info.lpVerb = verb as *const u8;
    info.lpVerbW = verb as *const u16;
    info.nShow = SW_SHOWNORMAL as i32;
    // CMIC_MASK_UNICODE 会让 Shell 只认 lpVerbW。两个 verb 都填上，
    // 这样无论 Shell 走宽字符分支还是 ANSI 分支都能取到同一个命令号——
    // 只填 lpVerbW 而漏掉 lpVerb 时，部分实现（含第三方扩展）会当场返回错误。
    info.fMask = CMIC_MASK_UNICODE;

    let invoke: unsafe extern "system" fn(*mut c_void, *const CMINVOKECOMMANDINFOEX) -> i32 =
        vfn(pcm, ICM_INVOKE);
    invoke(pcm, &info)
}

/// 「新建」子菜单的句柄（连同它在顶层菜单里的位置）。
unsafe fn new_submenu(menu: HMENU) -> Option<(i32, HMENU)> {
    for i in 0..GetMenuItemCount(menu) {
        let sub = GetSubMenu(menu, i);
        if sub.is_null() || !is_new_menu_text(&menu_item_text(menu, i)) {
            continue;
        }
        return Some((i, sub));
    }
    None
}

/// 取菜单项文本（按位置）。
unsafe fn menu_item_text(menu: HMENU, pos: i32) -> String {
    let mut buf = [0u16; 128];
    let n = GetMenuStringW(
        menu,
        pos as u32,
        buf.as_mut_ptr(),
        buf.len() as i32,
        MF_BYPOSITION,
    );
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())])
}

/// 归一化后的菜单文本是不是「新建」。
fn is_new_menu_text(text: &str) -> bool {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '&' | '.' | '\u{2026}'))
        .collect();
    // 「新建(N)」→ 取括号之前；全角括号一并处理
    let head = cleaned
        .split(['(', '（'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    head.eq_ignore_ascii_case("new") || head == "新建"
}
#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Com::CoUninitialize;
    use windows_sys::Win32::System::Ole::OleInitialize;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetMenuItemID;

    /// 造一个隐藏窗口当菜单的 owner。
    unsafe fn make_owner_window() -> HWND {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, RegisterClassExW, WNDCLASSEXW, WS_OVERLAPPED,
        };
        let class = crate::win::to_wide("DreamFolderMenuTestWnd");
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(DefWindowProcW);
        wc.lpszClassName = class.as_ptr();
        RegisterClassExW(&wc);
        let title = crate::win::to_wide("t");
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dreamfolder-menu-{tag}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 链路验证：背景菜单里的「新建」必须被换成我们自己的子菜单，
    /// 而且列表里的每一项都能真的建出东西。
    ///
    /// 为什么要有这条测试：Shell 自带的「新建」命令在我们这种没有
    /// `IShellView` 的宿主里对**每一个**条目都只回 `E_FAIL`
    /// （0x80004005），点上去毫无反应——这正是用户报的 bug。
    #[test]
    fn new_submenu_is_replaced_with_a_working_one() {
        unsafe {
            let hr = OleInitialize(std::ptr::null_mut());
            if hr < 0 && hr != 1 {
                eprintln!("跳过：当前线程无法初始化 OLE（0x{hr:08X}）");
                return;
            }
            let dir = temp_dir("new");
            let owner = make_owner_window();

            let Some(pcm) = background_context_menu(owner, &dir) else {
                std::fs::remove_dir_all(&dir).ok();
                CoUninitialize();
                eprintln!("跳过：拿不到背景菜单");
                return;
            };
            let menu = CreatePopupMenu();
            let query: unsafe extern "system" fn(*mut c_void, isize, u32, u32, u32, u32) -> i32 =
                vfn(pcm, ICM_QUERY_MENU);
            let hr = query(pcm, menu as isize, 0, ID_FIRST, ID_LAST, CMF_NORMAL);
            assert!(hr >= 0, "QueryContextMenu 失败：0x{:08X}", hr as u32);

            let (kinds, _guard) = replace_new_submenu(menu, owner);
            assert!(!kinds.is_empty(), "「新建」子菜单应该被替换成我们的");
            assert_eq!(kinds[0].ext, "", "第一项应该永远是「文件夹」");

            // 替换后的子菜单里只剩我们自己的编号
            let (_, sub) = new_submenu(menu).expect("替换后仍然应该有一个「新建」子菜单");
            let mut ids = Vec::new();
            for j in 0..GetMenuItemCount(sub) {
                let id = GetMenuItemID(sub, j);
                if id != u32::MAX && id != 0 {
                    ids.push(id);
                }
            }
            assert_eq!(ids.len(), kinds.len(), "菜单项数应该和条目数一致");
            assert!(
                ids.iter().all(|id| *id >= shellnew::OWN_ID_BASE),
                "所有项都该落在我们自己的编号区间：{ids:?}"
            );

            // 逐项建一遍，确认真能落盘（菜单本身要等用户点，这里直接走
            // 菜单项编号 → 条目 → 建文件 的同一条映射）
            let mut built = 0;
            for id in &ids {
                let index = (*id - shellnew::OWN_ID_BASE) as usize;
                let kind = &kinds[index];
                match shellnew::create(&dir, kind) {
                    Ok(name) => {
                        assert!(dir.join(&name).exists(), "建出来的 {name} 不见了");
                        built += 1;
                    }
                    Err(err) => panic!("新建「{}」失败：{err}", kind.label),
                }
            }
            assert_eq!(built, kinds.len());

            DestroyMenu(menu);
            release(pcm);
            std::fs::remove_dir_all(&dir).ok();
            CoUninitialize();
        }
    }
}
