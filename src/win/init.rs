//! 进程级初始化。冷启动路径上只做必须做的事，其余全部延迟。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows_sys::Win32::System::Ole::OleInitialize;
use windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext;
use windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;

/// 原始程序 manifest 声明的是 PerMonitorV2（见 PE 资源 RT_MANIFEST），
/// 这里再显式调用一次，保证不带 manifest 运行时（如 `cargo run`）行为一致。
pub fn init_process() {
    unsafe {
        // PerMonitorV2 优先；老系统上该导出不存在，退回系统级 DPI 感知。
        // 不检查返回值：失败本身不影响功能，只是缩放精度差一点。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetProcessDPIAware();

        // 用 OleInitialize 而不是 CoInitializeEx：它除了建 STA，还会装上
        // OLE 拖放需要的消息过滤器。没有这一层，DoDragDrop 期间鼠标和键盘
        // 消息不会被泵送，拖动会僵在半路。重复初始化返回 S_FALSE，无害。
        let _ = OleInitialize(std::ptr::null_mut());
    }
}

/// `SetPreferredAppMode` 的取值（uxtheme 内部枚举，按序号导出，没有头文件）。
const APP_MODE_FORCE_DARK: i32 = 2;
const APP_MODE_FORCE_LIGHT: i32 = 3;

type SetPreferredAppModeFn = unsafe extern "system" fn(i32) -> i32;
type FlushMenuThemesFn = unsafe extern "system" fn();

/// 当前应用期望的菜单配色。
static MENU_DARK: AtomicBool = AtomicBool::new(false);
/// uxtheme 的两个导出按序号取一次即可，避免每次弹菜单都去解析符号。
static SET_PREFERRED_APP_MODE: OnceLock<Option<SetPreferredAppModeFn>> = OnceLock::new();
static FLUSH_MENU_THEMES: OnceLock<Option<FlushMenuThemesFn>> = OnceLock::new();

/// 记录应用主题（深色 = true）并立刻同步给系统菜单。
pub fn set_menu_dark(dark: bool) {
    MENU_DARK.store(dark, Ordering::Relaxed);
    apply_menu_theme();
}

/// 按记录的主题强制系统菜单的外壳配色。
///
/// 右键菜单来自 `IContextMenu` + `TrackPopupMenu`，是系统画的外壳菜单，
/// 它不认我们的自绘调色板。默认（`AllowDark`）它跟随 **Windows 自身**
/// 的深浅设置：系统是深色、而应用选了浅色时，菜单就会突兀地变黑。
/// 这里改调 `ForceLight` / `ForceDark`，让菜单跟随应用内的主题。
///
/// 这两个函数是 uxtheme.dll 里按序号导出的未公开接口（135、136），
/// 没有头文件也没有文档，只能按序号取；取不到就放弃（老系统）。
pub fn apply_menu_theme() {
    let dark = MENU_DARK.load(Ordering::Relaxed);

    let set_mode = *SET_PREFERRED_APP_MODE.get_or_init(|| unsafe {
        ux_export::<SetPreferredAppModeFn>(135)
    });
    let flush = *FLUSH_MENU_THEMES.get_or_init(|| unsafe { ux_export::<FlushMenuThemesFn>(136) });

    unsafe {
        if let Some(set_mode) = set_mode {
            set_mode(if dark {
                APP_MODE_FORCE_DARK
            } else {
                APP_MODE_FORCE_LIGHT
            });
        }
        if let Some(flush) = flush {
            flush();
        }
    }
}

/// 从 uxtheme.dll 按导出序号取函数指针。
///
/// # Safety
/// 调用方必须保证 `F` 与该序号的真实签名一致——序号写错会直接崩在栈上。
unsafe fn ux_export<F: Copy>(ordinal: u32) -> Option<F> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    let name = crate::win::to_wide("uxtheme.dll");
    let module = GetModuleHandleW(name.as_ptr());
    if module.is_null() {
        return None;
    }
    let proc = GetProcAddress(module, ordinal as usize as *const u8)?;
    Some(std::mem::transmute_copy(&proc))
}

/// PerMonitorV2 句柄常量（windows-sys 未导出该常量时在此补齐）。
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: *mut core::ffi::c_void = -4isize as _;

/// 退出前释放 COM。
pub fn shutdown_process() {
    unsafe {
        windows_sys::Win32::System::Ole::OleUninitialize();
    }
}
