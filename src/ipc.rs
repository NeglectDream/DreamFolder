//! 单实例与进程间通信。
//!
//! 与原软件一致使用命名管道：`\\.\pipe\dreamfolderpipe`（见 PE 字符串）。
//! 第二个实例把「打开哪个路径 / 要不要新窗口」投递给已在运行的实例，然后退出。
//!
//! ## 为什么服务端必须用重叠 I/O
//!
//! 这里曾经用阻塞管道（`PIPE_WAIT`、不带 `FILE_FLAG_OVERLAPPED`），然后在
//! `WM_TIMER` 里调 `ConnectNamedPipe`「试探」有没有客户端接入。那是错的：
//! 阻塞模式下 `ConnectNamedPipe` 在没有客户端时**会一直等下去**，
//! 而它跑在主线程的消息处理里——窗口刚建好就被冻住，表现为启动后无响应。
//!
//! 现在管道以 `FILE_FLAG_OVERLAPPED` 创建，`ConnectNamedPipe` 挂起后立即
//! 返回 `ERROR_IO_PENDING`，主线程只做非阻塞的状态查询，永不等待。
//!
//! 注意 `PIPE_WAIT` 仍然保留：它描述的是**缓冲区满了之后**的写行为，
//! 与「调用是否会阻塞」是两回事。真正决定阻塞与否的是 `FILE_FLAG_OVERLAPPED`。

use crate::app::CommandLine;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PeekNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::win::to_wide;

/// 与原软件相同的管道名。
pub const PIPE_NAME: &str = r"\\.\pipe\dreamfolderpipe";

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
            bytes.as_ptr(),
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
/// 由主线程用 `PeekNamedPipe` 轮询而非阻塞重载，因此所有调用都必须是非阻塞的。
pub struct PipeServer {
    handle: HANDLE,
    /// `ConnectNamedPipe` 挂起的异步等待在内核侧持有这个结构的地址，
    /// 直到操作完成前都不能失效——所以它必须 Box 在堆上：`PipeServer`
    /// 被赋值进 `Application::pipe` 时结构体会整体移动，
    /// 若 OVERLAPPED 直接内联在结构体里，地址一变这个等待就悬空了。
    connect_ov: Box<OVERLAPPED>,
    /// 连接等待已挂起（避免重复发起）。
    connect_pending: bool,
    /// 客户端已接入。
    connected: bool,
}

impl PipeServer {
    /// 创建管道实例。失败返回 None（不影响程序启动）。
    pub fn create() -> Option<PipeServer> {
        let name = to_wide(PIPE_NAME);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                8192,
                8192,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return None;
        }

        let mut server = PipeServer {
            handle,
            // OVERLAPPED 是纯 POD，清零即合法初值；hEvent 留空表示
            // 完成时以文件句柄本身作为同步对象。
            connect_ov: Box::new(unsafe { std::mem::zeroed() }),
            connect_pending: false,
            connected: false,
        };

        // 创建后立刻挂起等待：客户端 `CreateFileW` 只有在服务端已经挂起
        // 等待时才会立即返回，否则第二个实例要一直等到第一次定时器触发。
        server.start_connect();
        Some(server)
    }

    /// 发起一次异步连接等待。立即返回，绝不阻塞。
    fn start_connect(&mut self) {
        if self.connected || self.connect_pending {
            return;
        }
        let ov = &mut *self.connect_ov as *mut OVERLAPPED;
        let ok = unsafe { ConnectNamedPipe(self.handle, ov) };
        if ok != 0 {
            self.connected = true;
            return;
        }
        match unsafe { GetLastError() } {
            // 客户端在 ConnectNamedPipe 之前就已经打开管道
            ERROR_PIPE_CONNECTED => self.connected = true,
            // 正常路径：等待挂起，稍后由轮询查询
            ERROR_IO_PENDING => self.connect_pending = true,
            _ => {}
        }
    }

    /// 非阻塞查询连接状态。返回 true 表示客户端已接入。
    pub fn try_accept(&mut self) -> bool {
        if self.connected {
            return true;
        }
        if !self.connect_pending {
            self.start_connect();
            return self.connected;
        }

        let ov = &*self.connect_ov as *const OVERLAPPED;
        let mut transferred = 0u32;
        // bWait = FALSE：只查询，不等待
        let ok = unsafe { GetOverlappedResult(self.handle, ov, &mut transferred, 0) };
        if ok != 0 {
            self.connected = true;
            self.connect_pending = false;
            return true;
        }
        // ERROR_IO_INCOMPLETE：仍在等客户端，这是常态而非错误
        false
    }

    /// 读取一条报文。返回 None 表示当前没有数据。
    ///
    /// 重叠句柄上的 `ReadFile` 必须带 OVERLAPPED，因此这里用栈上的结构。
    /// 调用前已由 `PeekNamedPipe` 确认管道里有数据，所以正常情况下
    /// `ReadFile` 会立即完成，只有极少数竞态才会走到等待分支。
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
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                &mut ov,
            )
        };
        if ok == 0 {
            if unsafe { GetLastError() } != ERROR_IO_PENDING {
                return None;
            }
            // 数据已在管道缓冲区里，这里顶多等一次内存拷贝
            let ovp = &ov as *const OVERLAPPED;
            let mut transferred = 0u32;
            if unsafe { GetOverlappedResult(self.handle, ovp, &mut transferred, 1) } == 0 {
                return None;
            }
            read = transferred;
        }
        if read == 0 {
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
