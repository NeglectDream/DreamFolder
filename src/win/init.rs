//! 进程级初始化。冷启动路径上只做必须做的事，其余全部延迟。

use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::UI::HiDpi::{SetProcessDPIAware, SetProcessDpiAwarenessContext};

/// 原始程序 manifest 声明的是 PerMonitorV2（见 PE 资源 RT_MANIFEST），
/// 这里再显式调用一次，保证不带 manifest 运行时（如 `cargo run`）行为一致。
pub fn init_process() {
    unsafe {
        // PerMonitorV2 优先；老系统上该导出不存在，退回系统级 DPI 感知。
        // 不检查返回值：失败本身不影响功能，只是缩放精度差一点。
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = SetProcessDPIAware();

        // COM 单线程套间：Shell 图标查询、上下文菜单、拖放都需要。
        // RPC_E_CHANGED_MODE 表示已被初始化过，忽略即可。
        let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
    }
}

/// PerMonitorV2 句柄常量（windows-sys 未导出该常量时在此补齐）。
const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: *mut core::ffi::c_void = -4isize as _;

/// 退出前释放 COM。
pub fn shutdown_process() {
    unsafe {
        windows_sys::Win32::System::Com::CoUninitialize();
    }
}
