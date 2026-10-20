//! 单实例与进程间通信。
//!
//! 与原软件一致使用命名管道：`\\.\pipe\360filepipe`（见 PE 字符串）。
//! 第二个实例把「打开哪个路径 / 要不要新窗口」投递给已在运行的实例，然后退出。
//!
//! 冷启动考量：管道连接是**非阻塞**的，服务端不存在时立刻返回，
//! 正常启动路径上只多一次 `CreateFileW` 失败判断。

use crate::app::CommandLine;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GENERIC_READ, GENERIC_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PeekNamedPipe, PIPE_ACCESS_DUPLEX,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::win::to_wide;

/// 与原软件相同的管道名。
pub const PIPE_NAME: &str = r"\\.\pipe\360filepipe";

/// 投递结果。
pub enum RouteResult {
    /// 已成功交给现有实例。
    Delivered,
    /// 没有实例在监听——正常启动。
    NoServer,
    /// 有实例但通信失败——仍然继续启动，避免用户打不开程序。
    Failed,
}

/// 把本次请求投递给已运行的实例。
pub fn route_to_existing(args: &CommandLine) -> RouteResult {
    // 需要新窗口时不做投递（用户明确要新窗口）
    if args.new_window {
        return RouteResult::NoServer;
    }

    let name = to_wide(PIPE_NAME);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return RouteResult::NoServer;
    }

    // 报文格式：PATH\nNEWTAB\n  以 UTF-8 传输
    let payload = format!(
        "{}\n{}\n",
        args.path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        if args.new_window { "1" } else { "0" }
    );
    let bytes = payload.as_bytes();

    let mut written = 0u32;
    let ok = unsafe {
        WriteFile(
            handle,
            bytes.as_ptr() as *const core::ffi::c_void,
            bytes.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };

    if ok != 0 {
        RouteResult::Delivered
    } else {
        RouteResult::Failed
    }
}

/// 服务端：创建管道实例，等待客户端投递。
///
/// 设计为**由主线程用 PeekNamedPipe 轮询**而非阻塞重载，
/// 因此这里用非重叠管道 + 每次只服务一个实例。
pub struct PipeServer {
    handle: HANDLE,
}

impl PipeServer {
    /// 创建管道实例。失败返回 None（不影响程序启动）。
    pub fn create() -> Option<PipeServer> {
        let name = to_wide(PIPE_NAME);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                8192,
                8192,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            None
        } else {
            Some(PipeServer { handle })
        }
    }

    /// 非阻塞尝试接受连接。返回 true 表示已连接。
    pub fn try_accept(&self) -> bool {
        let ok = unsafe { ConnectNamedPipe(self.handle, std::ptr::null_mut()) };
        if ok != 0 {
            return true;
        }
        // 已经连接的情况会返回 0 且 GetLastError == ERROR_PIPE_CONNECTED
        let err = unsafe { GetLastError() };
        const ERROR_PIPE_CONNECTED: u32 = 535;
        err == ERROR_PIPE_CONNECTED
    }

    /// 读取一条报文。返回 None 表示当前没有数据。
    pub fn read_message(&self) -> Option<String> {
        let mut avail = 0u32;
        let ok = unsafe {
            PeekNamedPipe(
                self.handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut avail,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || avail == 0 {
            return None;
        }

        let mut buf = vec![0u8; avail.min(8192) as usize];
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return None;
        }
        buf.truncate(read as usize);
        String::from_utf8(buf).ok()
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        unsafe {
            DisconnectNamedPipe(self.handle);
            CloseHandle(self.handle);
        }
    }
}

/// 解析投递来的报文。
pub fn parse_message(msg: &str) -> (Option<std::path::PathBuf>, bool) {
    let mut lines = msg.lines();
    let path = lines
        .next()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let new_tab = lines.next().map(|s| s.trim() == "1").unwrap_or(false);
    (path, new_tab)
}
