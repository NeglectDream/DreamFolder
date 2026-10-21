//! 拖放源：把选中的文件拖给资源管理器、桌面或其他程序。
//!
//! 数据对象直接借系统的 `SHCreateDataObject`——它会把 CF_HDROP 等格式
//! 都填好，目标程序收到的与「从资源管理器拖出来」完全一样。
//! 我们只需实现 `IDropSource`，它只回答两件事：什么时候算放下、按 ESC 算不算取消。
//!
//! 手写 COM 而非引入 `windows` crate：这个项目刻意只依赖纯 FFI 绑定，
//! 为了两个接口把整层包装拉进来，链接产物和冷启动都会变差。

use core::ffi::c_void;
use std::path::PathBuf;

use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{
    BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, POINT,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Ole::{DoDragDrop, DROPEFFECT, DROPEFFECT_COPY,
                                       DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_ESCAPE, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows_sys::Win32::UI::Shell::{Common::ITEMIDLIST, SHCreateDataObject, SHParseDisplayName};

const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as _;
const S_OK: HRESULT = 0;

/// GUID 比较。windows-sys 的 GUID 没实现 PartialEq，只能逐字段比。
fn same_guid(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// `IDropSource` 的 vtable。
///
/// 顺序必须与 oleidl.h 一致：前三个是所有 COM 接口共有的，
/// 后两个才是拖放自己的。
#[repr(C)]
struct IDropSourceVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    query_continue_drag: unsafe extern "system" fn(*mut c_void, BOOL, u32) -> HRESULT,
    give_feedback: unsafe extern "system" fn(*mut c_void, DROPEFFECT) -> HRESULT,
}

/// 拖放源对象。生命周期就是 `start` 这一次调用，所以放栈上。
#[repr(C)]
struct DropSource {
    vtbl: *const IDropSourceVtbl,
    refs: u32,
}

static DROP_SOURCE_VTBL: IDropSourceVtbl = IDropSourceVtbl {
    query_interface: ds_query_interface,
    add_ref: ds_add_ref,
    release: ds_release,
    query_continue_drag: ds_query_continue_drag,
    give_feedback: ds_give_feedback,
};

unsafe extern "system" fn ds_query_interface(
    this: *mut c_void,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_NOINTERFACE;
    }
    // 只承认 IUnknown 和 IDropSource——别的接口这里一概没有
    let iid = *riid;
    let unknown = GUID::from_u128(0x0000_0000_0000_0000_C000_0000_0000_0046);
    let drop_source = GUID::from_u128(0x0000_0121_0000_0000_C000_0000_0000_0046);
    if same_guid(&iid, &unknown) || same_guid(&iid, &drop_source) {
        *out = this;
        ds_add_ref(this);
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn ds_add_ref(this: *mut c_void) -> u32 {
    let me = &mut *(this as *mut DropSource);
    me.refs += 1;
    me.refs
}

unsafe extern "system" fn ds_release(this: *mut c_void) -> u32 {
    let me = &mut *(this as *mut DropSource);
    me.refs = me.refs.saturating_sub(1);
    // 对象在栈上，不能在这里 free；计数只用于让 COM 方知道我们还活着
    me.refs
}

unsafe extern "system" fn ds_query_continue_drag(
    _this: *mut c_void,
    escape_pressed: BOOL,
    key_state: u32,
) -> HRESULT {
    if escape_pressed != 0 {
        return DRAGDROP_S_CANCEL;
    }
    // 左键松开就是放下；用 GetKeyState 而不是 key_state 参数，
    // 因为有些宿主传进来的 key_state 不含鼠标键位
    let lbutton = (GetKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0;
    if !lbutton {
        return DRAGDROP_S_DROP;
    }
    let _ = key_state;
    let _ = GetKeyState(VK_ESCAPE as i32);
    S_OK
}

unsafe extern "system" fn ds_give_feedback(
    _this: *mut c_void,
    _effect: DROPEFFECT,
) -> HRESULT {
    // 用系统默认的光标反馈（复制/移动/快捷方式符号由系统按目标窗口决定）
    DRAGDROP_S_USEDEFAULTCURSORS
}

/// 调用系统释放一个 COM 接口指针。
unsafe fn release_com(obj: *mut c_void) {
    if obj.is_null() {
        return;
    }
    let vtbl = *(obj as *mut *const usize);
    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
        std::mem::transmute(*vtbl.add(2));
    release(obj);
}

unsafe fn free_pidls(pidls: &[*mut ITEMIDLIST]) {
    for &p in pidls {
        if !p.is_null() {
            CoTaskMemFree(p as *mut c_void);
        }
    }
}

/// 开始一次拖放。返回放下时的光标屏幕坐标（而不是中途取消）。
///
/// 调用方通常在同线程处理 `WM_LBUTTONDOWN` 之后、移动超过拖拽阈值时调用它。
/// `allow_move` 为 false 时只允许复制和建快捷方式，用于只读位置。
///
/// 返回坐标是为了让调用方判断「是否落在自己的收藏栏上」。窗口虽然已经
/// 注册了 `IDropTarget`，但落点判定有两套：OLE 回调给的客户端坐标
/// （`WM_DREAMFOLDER_DROP`）和这里事后取的屏幕坐标，两条路都要能走通。
pub fn start(paths: &[PathBuf], allow_move: bool) -> Option<(i32, i32)> {
    if paths.is_empty() {
        return None;
    }
    unsafe {
        // 1) 路径 → 绝对 PIDL
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
        for p in paths {
            let wide = crate::win::to_wide(&p.to_string_lossy());
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            let hr = SHParseDisplayName(
                wide.as_ptr(),
                std::ptr::null_mut(),
                &mut pidl,
                0,
                std::ptr::null_mut(),
            );
            if hr < 0 || pidl.is_null() {
                // 有一个解析不出来就整体放弃：拖出一个缺项的集合比不拖更糟
                free_pidls(&pidls);
                return None;
            }
            pidls.push(pidl);
        }

        // 2) 系统数据对象（CF_HDROP 由它填）
        let iid_data_object = GUID::from_u128(0x0000_010E_0000_0000_C000_0000_0000_0046);
        let mut data_obj: *mut c_void = std::ptr::null_mut();
        let hr = SHCreateDataObject(
            std::ptr::null(),
            pidls.len() as u32,
            // SHParseDisplayName 填的是 *mut，而 SHCreateDataObject 要 *const 数组
            pidls.as_ptr() as *const *const ITEMIDLIST,
            std::ptr::null_mut(),
            &iid_data_object,
            &mut data_obj,
        );
        free_pidls(&pidls);
        if hr < 0 || data_obj.is_null() {
            return None;
        }

        // 3) 拖放源 + 拖动
        let mut src = DropSource {
            vtbl: &DROP_SOURCE_VTBL,
            refs: 1,
        };
        let allowed: DROPEFFECT = if allow_move {
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK
        } else {
            DROPEFFECT_COPY | DROPEFFECT_LINK
        };
        let mut effect: DROPEFFECT = 0;
        // 登记成「自己的数据对象」：放置目标据此区分「自己拖自己」（路径已知）
        // 与「从别处拖进来」（路径只能在 Drop 回调里读 CF_HDROP）
        super::droptarget::set_our_data(data_obj);
        let hr = DoDragDrop(
            data_obj,
            &mut src as *mut DropSource as *mut c_void,
            allowed,
            &mut effect,
        );
        super::droptarget::clear_our_data();

        release_com(data_obj);
        let _ = hr;

        // 放下时的光标位置（屏幕坐标）：调用方用它判断是不是落在自己的收藏栏上。
        //
        // 注意这里**不按 `hr` 提前返回**：落回自己的窗口时，`Drop` 回调已经
        // 通过 `WM_DREAMFOLDER_DROP` 把落点送过去了，但别的目标（含
        // `DRAGDROP_S_CANCEL` 的中途取消）仍需要靠这个坐标兜底判定。
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt) == 0 {
            return None;
        }
        Some((pt.x, pt.y))
    }
}
