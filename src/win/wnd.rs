//! 窗口类注册、创建与消息循环。
//!
//! 主窗口是**无边框**的（WS_POPUP + WS_THICKFRAME），标题栏、标签栏、
//! 工具栏、地址栏、文件视图全部由我们自己绘制——这正是原软件 DuiLib
//! 的做法，也是我们把冷启动压到极致的先决条件（少一层框架初始化）。

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
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

    // 类图标也要给：任务栏分组时用的是类/进程级别的图标，
    // 只在创建后发 WM_SETICON 的时候，任务栏那块常常还是空白。
    // 图标来自内嵌的 logo（见 `appicon`），取不到时它会退化成系统图标。
    let icon_large = super::appicon::large();
    let icon_small = super::appicon::small();

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS | CS_OWNDC,
        lpfnWndProc: Some(proc_),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module_handle(),
        hIcon: icon_large,
        hCursor: cursor,
        hbrBackground: background,
        lpszMenuName: std::ptr::null(),
        lpszClassName: name.as_ptr(),
        hIconSm: icon_small,
    };

    RegisterClassExW(&wc)
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

/// 从 `explorer.exe` 里取资源管理器自己的大/小图标。
///
/// 返回的是进程共享的图标句柄，不需要（也不应该）DestroyIcon。
/// 取不到时返回空句柄，调用方按「没有图标」处理。
pub unsafe fn explorer_icons() -> (HICON, HICON) {
    use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
    use windows_sys::Win32::UI::Shell::ExtractIconExW;

    let mut dir = [0u16; 260];
    let n = GetWindowsDirectoryW(dir.as_mut_ptr(), dir.len() as u32);
    if n == 0 || n as usize >= dir.len() {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }

    let mut path: Vec<u16> = dir[..n as usize].to_vec();
    path.extend(r"\explorer.exe".encode_utf16());
    path.push(0);

    let mut large: HICON = std::ptr::null_mut();
    let mut small: HICON = std::ptr::null_mut();
    if ExtractIconExW(path.as_ptr(), 0, &mut large, &mut small, 1) == 0 {
        return (std::ptr::null_mut(), std::ptr::null_mut());
    }
    (large, small)
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

/// 给无边框窗口加 Win11 的小圆角。
///
/// 资源管理器那圈弧度是 DWM 画的，靠 `DWMWA_WINDOW_CORNER_PREFERENCE`
/// 请求即可；Win10 上没这个属性，调用失败也无害，窗口维持直角。
/// 只有最外层这一圈跟着系统走，内部控件一律保持方正。
///
/// # Safety
/// `hwnd` 必须是有效窗口句柄。
pub unsafe fn apply_round_corners(hwnd: HWND) {
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &DWMWCP_ROUND as *const i32 as *const core::ffi::c_void,
        std::mem::size_of::<i32>() as u32,
    );
}

/// # Safety
/// 返回的指针只有在窗口存活期间有效。
pub unsafe fn user_data<T>(hwnd: HWND) -> *mut T {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut T
}
