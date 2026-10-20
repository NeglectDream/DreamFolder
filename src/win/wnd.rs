//! 窗口类注册、创建与消息循环。
//!
//! 主窗口是**无边框**的（WS_POPUP + WS_THICKFRAME），标题栏、标签栏、
//! 工具栏、地址栏、文件视图全部由我们自己绘制——这正是原软件 DuiLib
//! 的做法，也是我们把冷启动压到极致的先决条件（少一层框架初始化）。

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{module_handle, to_wide};

/// 窗口过程签名。
pub type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// 注册窗口类。返回原子（0 表示失败）。
///
/// # Safety
/// `proc_` 必须是合法的窗口过程函数指针且在整个进程生命周期内有效。
pub unsafe fn register_class(
    class_name: &str,
    proc_: WndProc,
    background: *mut core::ffi::c_void,
    cursor: *mut core::ffi::c_void,
) -> u16 {
    let name = to_wide(class_name);

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS | CS_OWNDC,
        lpfnWndProc: Some(proc_),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module_handle(),
        hIcon: std::ptr::null_mut(),
        hCursor: cursor,
        hbrBackground: background,
        lpszMenuName: std::ptr::null(),
        lpszClassName: name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };

    RegisterClassExW(&wc)
}

/// 创建无边框主窗口。
///
/// # Safety
/// 调用前必须已注册 `class_name` 对应的窗口类。
pub unsafe fn create_popup_window(
    class_name: &str,
    title: &str,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    ex_style: u32,
    style: u32,
    param: *mut core::ffi::c_void,
) -> HWND {
    let class = to_wide(class_name);
    let text = to_wide(title);

    CreateWindowExW(
        ex_style,
        class.as_ptr(),
        text.as_ptr(),
        style,
        x,
        y,
        w,
        h,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        module_handle(),
        param,
    )
}

/// 标准消息循环。返回进程退出码。
///
/// 使用 `GetMessageW` 的返回值三态语义：>0 正常、==0 收到 WM_QUIT、<0 出错。
pub unsafe fn message_loop() -> i32 {
    let mut msg: MSG = std::mem::zeroed();
    loop {
        let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
        if r == 0 {
            return msg.wParam as i32; // WM_QUIT
        }
        if r < 0 {
            return -1;
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// 取窗口客户区尺寸。
pub fn client_rect(hwnd: HWND) -> RECT {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut r) };
    r
}

/// 把窗口矩形限制在最近的显示器工作区内（原软件的初始定位行为）。
pub unsafe fn center_on_monitor(hwnd: HWND, w: i32, h: i32) -> (i32, i32) {
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;

    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if GetMonitorInfoW(mon, &mut mi) != 0 {
        let work = mi.rcWork;
        let x = work.left + ((work.right - work.left) - w) / 2;
        let y = work.top + ((work.bottom - work.top) - h) / 2;
        (x.max(work.left), y.max(work.top))
    } else {
        (200, 120)
    }
}

/// 把 `GetWindowLongPtrW` 的 USERDATA 槽当作 `*mut T` 读写。
pub unsafe fn set_user_data<T>(hwnd: HWND, p: *mut T) {
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, p as isize);
}

/// # Safety
/// 返回的指针只有在窗口存活期间有效。
pub unsafe fn user_data<T>(hwnd: HWND) -> *mut T {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut T
}
