//! 本窗口作为 OLE 放置目标。
//!
//! 只为解决一件事：从别处（包括自己的文件列表）拖东西过来时，鼠标光标
//! 会一路顶着「禁止」叉号。原因是窗口从未注册 `IDropTarget`，OLE 认为
//! 整块客户区都不接受拖放。收藏栏本来就是合法的落点，只要把它如实报给
//! OLE，光标就会变成系统的复制光标，松手也能真的收到 `Drop`。
//!
//! 判定用的收藏栏矩形由 UI 每帧写进来，且是**客户区坐标**：窗口移动时
//! 不需要重新登记，回调里用 `ScreenToClient` 现算即可。

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::System::Com::{FORMATETC, STGMEDIUM};
use windows_sys::Win32::System::Ole::{
    RegisterDragDrop, ReleaseStgMedium, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragQueryFileW};
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

// 收藏栏在客户区里的矩形。左/上/右/下，0 表示「没有收藏栏」。
static FAVBAR_L: AtomicI32 = AtomicI32::new(0);
static FAVBAR_T: AtomicI32 = AtomicI32::new(0);
static FAVBAR_R: AtomicI32 = AtomicI32::new(0);
static FAVBAR_B: AtomicI32 = AtomicI32::new(0);

// 导航窗格在客户区里的矩形，用来接受「拖进快捷访问区」。
static NAV_L: AtomicI32 = AtomicI32::new(0);
static NAV_T: AtomicI32 = AtomicI32::new(0);
static NAV_R: AtomicI32 = AtomicI32::new(0);
static NAV_B: AtomicI32 = AtomicI32::new(0);

// 文件视图在客户区里的矩形：拖到条目上／空白处都算落在这里。
static VIEW_L: AtomicI32 = AtomicI32::new(0);
static VIEW_T: AtomicI32 = AtomicI32::new(0);
static VIEW_R: AtomicI32 = AtomicI32::new(0);
static VIEW_B: AtomicI32 = AtomicI32::new(0);

/// 上一次 `Drop` 是否落在收藏栏 / 导航窗格上。松手后由调用方取走并清零。
static DROPPED_ON_FAVBAR: AtomicBool = AtomicBool::new(false);
static DROPPED_ON_NAV: AtomicBool = AtomicBool::new(false);

/// 上一次 `Drop` 落在客户区的哪个点（客户区坐标）。
///
/// 松手之后才知道「落在哪个条目上」——文件视图里的落点要经过一次命中测试
/// 才能变成「某个文件夹」或「空白处」，而 OLE 不会把那个点留给我们，
/// 所以在 `Drop` 里先记下来，由 UI 线程取走。
static DROP_PT: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// 本进程自己正在拖的数据对象（`dragdrop::start` 里登记）。
///
/// 自己拖自己的情况只关心「落在哪个区」，路径由调用方直接拿着；
/// 从别处（资源管理器、桌面）拖进来时路径只有这里能读到。
static OUR_DATA: std::sync::atomic::AtomicPtr<c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// 当前这次拖动的数据对象是不是本进程自己造的（`DragEnter` 时判定）。
///
/// 判定结果影响「松手时报告 MOVE 还是 COPY」：文件视图里的移动是**我们**
/// 用 `SHFileOperation` 做的，所以对外部来源只能报 COPY——报 MOVE 的话
/// Explorer 会以为移动由它负责，在我们已经搬完之后再去删源文件。
static OUR_DRAG: AtomicBool = AtomicBool::new(false);

/// 外部拖入的待处理内容：`(落在哪个区, 路径列表)`。
static PENDING: Mutex<Option<(DropZone, Vec<PathBuf>)>> = Mutex::new(None);

/// 拖放内容已就绪的自定义消息（由 `drop_impl` 投递，UI 线程取出后处理）。
pub const WM_DREAMFOLDER_DROP: u32 = WM_APP + 17;

/// 拖动经过窗口时投递的消息：UI 线程据此更新「会落在哪一行」的高亮。
pub const WM_DREAMFOLDER_DRAGOVER: u32 = WM_APP + 18;

/// 拖动中最近一次的客户区落点。
///
/// 只有 UI 线程读它，但 OLE 回调也可能在别的线程进来，所以仍然用互斥量。
/// 写进来只是为了「让用户看清松开会发生什么」——拖到标签行上要能看出
/// 这一行是目标，否则打标签和加入快捷访问在外观上完全没有区别。
static DRAG_PT: Mutex<Option<(i32, i32)>> = Mutex::new(None);

/// 取走拖动中的落点。`None` 表示这一轮拖动已经离开窗口（或已松手）。
pub fn take_drag_point() -> Option<(i32, i32)> {
    DRAG_PT.lock().ok().and_then(|mut slot| slot.take())
}

/// 落点所在的区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    /// 什么都没落在上面。
    None,
    /// 收藏书签栏。
    Favbar,
    /// 导航窗格（拖进来就是加入「快捷访问」）。
    NavPane,
    /// 文件视图：拖到文件夹上＝移进去，拖到可执行文件上＝运行它，
    /// 拖到空白处＝移进当前目录。
    View,
}

/// 更新收藏栏的客户区矩形。布局每次重算后调用。
pub fn set_favbar_rect(r: RECT) {
    if r.right <= r.left || r.bottom <= r.top {
        FAVBAR_R.store(0, Ordering::Relaxed);
        return;
    }
    FAVBAR_L.store(r.left, Ordering::Relaxed);
    FAVBAR_T.store(r.top, Ordering::Relaxed);
    FAVBAR_R.store(r.right, Ordering::Relaxed);
    FAVBAR_B.store(r.bottom, Ordering::Relaxed);
}

/// 更新导航窗格的客户区矩形。布局每次重算后调用。
pub fn set_nav_rect(r: RECT) {
    if r.right <= r.left || r.bottom <= r.top {
        NAV_R.store(0, Ordering::Relaxed);
        return;
    }
    NAV_L.store(r.left, Ordering::Relaxed);
    NAV_T.store(r.top, Ordering::Relaxed);
    NAV_R.store(r.right, Ordering::Relaxed);
    NAV_B.store(r.bottom, Ordering::Relaxed);
}

/// 更新文件视图的客户区矩形。布局每次重算后调用。
pub fn set_view_rect(r: RECT) {
    if r.right <= r.left || r.bottom <= r.top {
        VIEW_R.store(0, Ordering::Relaxed);
        return;
    }
    VIEW_L.store(r.left, Ordering::Relaxed);
    VIEW_T.store(r.top, Ordering::Relaxed);
    VIEW_R.store(r.right, Ordering::Relaxed);
    VIEW_B.store(r.bottom, Ordering::Relaxed);
}

fn view_visible() -> bool {
    VIEW_R.load(Ordering::Relaxed) > VIEW_L.load(Ordering::Relaxed)
}

/// 登记「本进程正在拖的数据对象」，拖动结束后用 `clear_our_data` 清掉。
pub fn set_our_data(p: *mut c_void) {
    OUR_DATA.store(p, Ordering::Relaxed);
}

pub fn clear_our_data() {
    OUR_DATA.store(std::ptr::null_mut(), Ordering::Relaxed);
}

fn favbar_visible() -> bool {
    FAVBAR_R.load(Ordering::Relaxed) > FAVBAR_L.load(Ordering::Relaxed)
}

fn nav_visible() -> bool {
    NAV_R.load(Ordering::Relaxed) > NAV_L.load(Ordering::Relaxed)
}

fn client_zone(x: i32, y: i32) -> DropZone {
    if favbar_visible()
        && x >= FAVBAR_L.load(Ordering::Relaxed)
        && x < FAVBAR_R.load(Ordering::Relaxed)
        && y >= FAVBAR_T.load(Ordering::Relaxed)
        && y < FAVBAR_B.load(Ordering::Relaxed)
    {
        return DropZone::Favbar;
    }
    if nav_visible()
        && x >= NAV_L.load(Ordering::Relaxed)
        && x < NAV_R.load(Ordering::Relaxed)
        && y >= NAV_T.load(Ordering::Relaxed)
        && y < NAV_B.load(Ordering::Relaxed)
    {
        return DropZone::NavPane;
    }
    if view_visible()
        && x >= VIEW_L.load(Ordering::Relaxed)
        && x < VIEW_R.load(Ordering::Relaxed)
        && y >= VIEW_T.load(Ordering::Relaxed)
        && y < VIEW_B.load(Ordering::Relaxed)
    {
        return DropZone::View;
    }
    DropZone::None
}

/// 取走「上次拖放落在收藏栏」的标记。
pub fn take_drop_flag() -> bool {
    DROPPED_ON_FAVBAR.swap(false, Ordering::Relaxed)
}

/// 取走「上次拖放落在导航窗格」的标记。
pub fn take_nav_drop_flag() -> bool {
    DROPPED_ON_NAV.swap(false, Ordering::Relaxed)
}

/// 取走外部拖入的内容（只有真正读到路径时才有值）。
pub fn take_external_drop() -> Option<(DropZone, Vec<PathBuf>)> {
    PENDING.lock().ok().and_then(|mut slot| slot.take())
}

/// 取走「上次拖放落在客户区的哪个点」。
pub fn take_drop_point() -> Option<(i32, i32)> {
    DROP_PT.lock().ok().and_then(|mut slot| slot.take())
}

// ---------------------------------------------------------------- COM 部分

const S_OK: i32 = 0;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

/// `POINTL`：OLE 拖动回调里用的是屏幕坐标的长整型点。
///
/// 注意：它在 `IDropTarget` 的方法里是**按值**传参的（x64 ABI 下装进一个
/// 寄存器），绝不能用 `*const PointL` 声明——那样拿到的是坐标数值本身，
/// 当成指针去解引用就会立刻访问越界，把整个进程带走。
/// 这是「在滚动条上按下拖动就崩溃」的真凶：拖动越界后转成了文件拖放，
/// 在本窗口内松手时走 `Drop`，崩溃就发生在这一次解引用上。
#[repr(C)]
#[derive(Clone, Copy)]
struct PointL {
    x: i32,
    y: i32,
}

type QiFn = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
type RefFn = unsafe extern "system" fn(*mut c_void) -> u32;
/// `IDropTarget::DragEnter`：`(this, data, keys, pt, effect)`。
type DragEnterFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, u32, PointL, *mut u32) -> i32;
/// `IDropTarget::DragOver`：**比 `DragEnter` 少一个 `data` 参数**。
///
/// 这一点极容易踩坑：三个拖动回调长得几乎一样，但 `DragOver` 的参数表是
/// `(this, keys, pt, effect)`。要是按 `DragEnter` 的形状声明它，`effect`
/// 就会从栈上读成一个野指针，写进去直接访问越界——「一拖动就崩」的真凶
/// 之一就在这里。
type DragOverFn = unsafe extern "system" fn(*mut c_void, u32, PointL, *mut u32) -> i32;
/// `IDropTarget::Drop`：与 `DragEnter` 同形。
type DropFn =
    unsafe extern "system" fn(*mut c_void, *mut c_void, u32, PointL, *mut u32) -> i32;
type LeaveFn = unsafe extern "system" fn(*mut c_void) -> i32;

/// `IDropTarget` 的 vtable。windows-sys 只给函数导出、不给 COM 结构，
/// 所以这份布局得手写。顺序必须和 `IDropTarget` 的头文件定义一致。
#[repr(C)]
struct DropTargetVtbl {
    query_interface: QiFn,
    add_ref: RefFn,
    release: RefFn,
    drag_enter: DragEnterFn,
    drag_over: DragOverFn,
    drag_leave: LeaveFn,
    drop: DropFn,
}

#[repr(C)]
struct DropTarget {
    vtbl: *const DropTargetVtbl,
    refs: u32,
    hwnd: HWND,
}

static VTBL: DropTargetVtbl = DropTargetVtbl {
    query_interface: query_interface,
    add_ref: add_ref,
    release: release,
    drag_enter: drag_enter,
    drag_over: drag_over,
    drag_leave: drag_leave,
    drop: drop_impl,
};

/// 注册放置目标。进程内只需要一次，句柄常驻不释放
/// （窗口活多久它就活多久，泄漏是刻意的）。
///
/// # Safety
/// `hwnd` 必须是有效窗口句柄，且已经 `OleInitialize`。
pub unsafe fn register(hwnd: HWND) {
    let target = Box::into_raw(Box::new(DropTarget {
        vtbl: &VTBL,
        refs: 1,
        hwnd,
    }));
    // 顺手让窗口接受 WM_DROPFILES 之外的传统拖放，避免旧程序走老路径被拒
    DragAcceptFiles(hwnd, 1);
    if RegisterDragDrop(hwnd, target as *mut c_void) != S_OK {
        // 注册失败就退化成原来的行为（光标是叉号），不至于崩
        drop(Box::from_raw(target));
    }
}

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    riid: *const GUID,
    out: *mut *mut c_void,
) -> i32 {
    // 只有 IUnknown 和 IDropTarget 能被问出来
    const IID_IUNKNOWN: GUID = GUID {
        data1: 0x0000_0000,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };
    const IID_IDROPTARGET: GUID = GUID {
        data1: 0x0000_0122,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    if out.is_null() {
        return E_NOINTERFACE;
    }
    let matched = !riid.is_null()
        && (guid_eq(riid, &IID_IUNKNOWN) || guid_eq(riid, &IID_IDROPTARGET));
    if matched {
        *out = this;
        add_ref(this);
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

/// 16 字节比较。`windows-sys` 的 `GUID` 没有实现 `PartialEq`，只能自己来。
fn guid_eq(a: *const GUID, b: &GUID) -> bool {
    let x = unsafe { std::slice::from_raw_parts(a as *const u8, 16) };
    let y = unsafe { std::slice::from_raw_parts(b as *const GUID as *const u8, 16) };
    x == y
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let t = &mut *(this as *mut DropTarget);
    t.refs += 1;
    t.refs
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let t = &mut *(this as *mut DropTarget);
    t.refs -= 1;
    t.refs
}

/// 屏幕坐标 → 落点区域。
/// 把 OLE 给的**屏幕**坐标换算成客户区坐标，并判出落在哪个区域。
unsafe fn zone_at(hwnd: HWND, pt: PointL) -> (DropZone, Option<(i32, i32)>) {
    let mut p = POINT { x: pt.x, y: pt.y };
    if ScreenToClient(hwnd, &mut p) == 0 {
        return (DropZone::None, None);
    }
    (client_zone(p.x, p.y), Some((p.x, p.y)))
}

/// 从 OLE 数据对象里取出 `CF_HDROP` 的文件列表。
///
/// 外部拖入时这是唯一能拿到路径的时机：`Drop` 一返回数据对象就没了。
/// 取不到就返回空表，调用方按「没东西可加」处理。
unsafe fn read_hdrop(data: *mut c_void) -> Vec<PathBuf> {
    const CF_HDROP: u16 = 15;
    const DVASPECT_CONTENT: u32 = 1;
    const TYMED_HGLOBAL: u32 = 1;
    /// `IDataObject::GetData` 在 vtable 里的槽位（前三个是 IUnknown 的）。
    const GET_DATA_SLOT: usize = 3;

    let mut out = Vec::new();
    if data.is_null() {
        return out;
    }
    let vtbl = *(data as *mut *const usize);
    if vtbl.is_null() {
        return out;
    }
    let get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> i32 =
        std::mem::transmute(*vtbl.add(GET_DATA_SLOT));

    let fmt = FORMATETC {
        cfFormat: CF_HDROP,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL,
    };
    let mut medium: STGMEDIUM = std::mem::zeroed();
    if get_data(data, &fmt, &mut medium) < 0 {
        return out;
    }

    // CF_HDROP 的 hGlobal 就是 HDROP 本身，可以直接喂给 DragQueryFileW
    let hdrop = medium.u.hGlobal as *mut c_void;
    if !hdrop.is_null() {
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; (len + 1) as usize];
            DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
            out.push(PathBuf::from(crate::win::from_wide(&buf)));
        }
    }
    ReleaseStgMedium(&mut medium);
    out
}

/// 拖动经过时该显示什么效果。
///
/// 收藏栏 / 导航窗格上是「复制」（书签/快捷项只是引用，原件不动）；
/// 文件视图里只有**本进程**的拖动报「移动」——因为移动是我们在落点里做的，
/// 外部来源报 MOVE 会让 Explorer 也去删一遍源文件。
unsafe fn set_effect(this: *mut c_void, pt: PointL, effect: *mut u32) {
    // `effect` 由 OLE 自己传进来，正常不会是空；空指针就不写，免得多一种崩法
    if effect.is_null() {
        return;
    }
    let hwnd = (*(this as *mut DropTarget)).hwnd;
    *effect = match zone_at(hwnd, pt).0 {
        DropZone::None => DROPEFFECT_NONE,
        DropZone::Favbar | DropZone::NavPane => DROPEFFECT_COPY,
        DropZone::View => {
            if OUR_DRAG.load(Ordering::Relaxed) {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_COPY
            }
        }
    };
}

unsafe extern "system" fn drag_enter(
    this: *mut c_void,
    data: *mut c_void,
    _keys: u32,
    pt: PointL,
    effect: *mut u32,
) -> i32 {
    // 只在进入时判一次：这一次拖动的数据对象不会中途换人
    OUR_DRAG.store(data == OUR_DATA.load(Ordering::Relaxed), Ordering::Relaxed);
    set_effect(this, pt, effect);
    note_drag_point(this, pt);
    S_OK
}

unsafe extern "system" fn drag_over(
    this: *mut c_void,
    _keys: u32,
    pt: PointL,
    effect: *mut u32,
) -> i32 {
    set_effect(this, pt, effect);
    note_drag_point(this, pt);
    S_OK
}

/// 记下落点，供 UI 线程画「会落在哪一行」的高亮。
///
/// 窗口内拖拽和从资源管理器拖进来走同一条路：两种来源落到标签行上都会打
/// 标签，高亮就该一样。只有状态真的变了才投消息——拖动时这条回调跟着指针
/// 走，每次都投会把消息队列灌满，重绘反而是白干。
unsafe fn note_drag_point(this: *mut c_void, pt: PointL) {
    let hwnd = (*(this as *mut DropTarget)).hwnd;
    let next = match zone_at(hwnd, pt) {
        (DropZone::NavPane, Some(point)) => Some(point),
        _ => None,
    };
    let changed = if let Ok(mut slot) = DRAG_PT.lock() {
        let changed = *slot != next;
        *slot = next;
        changed
    } else {
        false
    };
    if changed {
        PostMessageW(hwnd, WM_DREAMFOLDER_DRAGOVER, 0, 0);
    }
}

unsafe extern "system" fn drag_leave(this: *mut c_void) -> i32 {
    // 离开窗口：清掉高亮，否则那一行会一直亮着像是还在拖
    if let Ok(mut slot) = DRAG_PT.lock() {
        *slot = None;
    }
    let hwnd = (*(this as *mut DropTarget)).hwnd;
    PostMessageW(hwnd, WM_DREAMFOLDER_DRAGOVER, 0, 0);
    S_OK
}

unsafe extern "system" fn drop_impl(
    this: *mut c_void,
    data: *mut c_void,
    _keys: u32,
    pt: PointL,
    effect: *mut u32,
) -> i32 {
    let hwnd = (*(this as *mut DropTarget)).hwnd;
    let (zone, point) = zone_at(hwnd, pt);
    // 松手：拖动高亮到此为止
    if let Ok(mut slot) = DRAG_PT.lock() {
        *slot = None;
    }
    PostMessageW(hwnd, WM_DREAMFOLDER_DRAGOVER, 0, 0);
    match zone {
        DropZone::Favbar => DROPPED_ON_FAVBAR.store(true, Ordering::Relaxed),
        DropZone::NavPane => DROPPED_ON_NAV.store(true, Ordering::Relaxed),
        DropZone::View | DropZone::None => {}
    }
    // 落点先记下来：文件视图里要靠它命中「落在哪个文件夹/可执行文件上」
    if let Ok(mut slot) = DROP_PT.lock() {
        *slot = point;
    }

    // 外部拖入（数据对象不是本进程自己造的）：路径得在这里取，
    // 取到后投个消息让 UI 线程去加书签 / 加强捷访问 / 移进文件夹。
    // 自己拖自己的情况路径就在调用方手里，这里只留「落在哪」的标记。
    if zone != DropZone::None && data != OUR_DATA.load(Ordering::Relaxed) {
        let paths = read_hdrop(data);
        if !paths.is_empty() {
            if let Ok(mut slot) = PENDING.lock() {
                *slot = Some((zone, paths));
            }
            PostMessageW(hwnd, WM_DREAMFOLDER_DROP, 0, 0);
        }
    }

    if !effect.is_null() {
        *effect = match zone {
            DropZone::None => DROPEFFECT_NONE,
            DropZone::Favbar | DropZone::NavPane => DROPEFFECT_COPY,
            // 移动由我们自己做（见 `Application::drop_paths_onto_view`），
            // 所以对外部来源只报 COPY，别让源端再删一次
            DropZone::View => {
                if OUR_DRAG.load(Ordering::Relaxed) {
                    DROPEFFECT_MOVE
                } else {
                    DROPEFFECT_COPY
                }
            }
        };
    }
    S_OK
}
