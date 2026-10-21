//! 目录变化监听（`ReadDirectoryChangesW`）。
//!
//! 资源管理器在别的程序往当前目录写文件时会自己刷新列表。我们以前只有 F5，
//! 外部程序新建、删除、改名之后界面一直停在旧状态——这是最容易被当成
//! 「这软件坏了」的一处。
//!
//! 每个工作线程守一个目录。这里全部用 overlapped 形态：
//!
//! - 文件句柄以 `FILE_FLAG_OVERLAPPED` 打开；
//! - `ReadDirectoryChangesW` 带 `OVERLAPPED` 结构，调用立即返回；
//! - 线程用 `WaitForMultipleObjects([变化事件, 停止事件])` 等结果。
//!
//! 三条缺一不可。最开始只给 `ReadDirectoryChangesW` 传了 `OVERLAPPED`，
//! 却忘了句柄上的 `FILE_FLAG_OVERLAPPED`，于是它在**调用线程里同步等待**：
//! 切到别的目录时 `drop` 去 join 这个线程，就一直等到那个目录里有文件
//! 变动为止——界面直接未响应，而且只在「安静的目录」上复现，看起来像
//! 某些文件夹有毒。`src/ipc.rs` 开头记过同一个坑。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::win::to_wide_path;

/// 变化之后先歇一会儿再听下一轮：复制整个目录会连出上百条通知，
/// 这段时间攒下的一律算作同一次刷新（上层还有 300ms 防抖）。
const COALESCE_MS: u32 = 200;

/// 一次监听的句柄包。`Drop` 时通知线程退出。
///
/// 线程自己关文件句柄与变化事件，这里只保留停止事件与 join 句柄。
pub struct DirWatcher {
    /// 正在守的目录。上层用它在刷新时判断要不要重挂——同目录刷新
    /// 重挂一次要等线程收尾，等于白等。
    pub path: PathBuf,
    /// 手动重置（manual-reset）事件：置位后线程从等待中立刻醒来。
    stop: HANDLE,
    join: Option<JoinHandle<()>>,
}

// 停止事件是裸句柄，只在 UI 线程创建、置位、关闭；线程侧只读它。
// 这里不需要 Send，`DirWatcher` 也就没有跨线程的需求。

impl DirWatcher {
    /// 开始监听 `dir`：任何子项的新建/删除/改名/写盘都会向 `hwnd` 投递 `msg`。
    ///
    /// 目录打不开（无权限、已删除）时返回 `None`——监听失败不该影响浏览。
    pub fn spawn(dir: &Path, hwnd: isize, msg: u32) -> Option<DirWatcher> {
        // 停止事件在 UI 线程建：线程可能刚起来就卡在打开慢目录上，
        // 得让上层手里先有东西可以置位。
        let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if stop.is_null() {
            return None;
        }

        let path = dir.to_path_buf();
        let path_thread = path.clone();
        let stop_raw = stop as usize;
        let join = std::thread::spawn(move || {
            watcher_loop(&path_thread, stop_raw, hwnd, msg);
        });

        Some(DirWatcher {
            path,
            stop,
            join: Some(join),
        })
    }
}

impl Drop for DirWatcher {
    fn drop(&mut self) {
        unsafe { SetEvent(self.stop) };

        let Some(join) = self.join.take() else {
            unsafe { CloseHandle(self.stop) };
            return;
        };
        if join.is_finished() {
            let _ = join.join();
            unsafe { CloseHandle(self.stop) };
            return;
        }
        // 线程可能正卡在打开一个离线网络目录上。join 会让界面陪着等，
        // 所以先交给回收队列，下次切目录时顺手收尾。
        retire(self.stop as usize, join);
    }
}

/// 等待收尾的监听线程。句柄要等线程退出后才能关。
static RETIRED: Mutex<Vec<(usize, JoinHandle<()>)>> = Mutex::new(Vec::new());

fn retire(stop: usize, join: JoinHandle<()>) {
    let mut q = RETIRED.lock().unwrap_or_else(|e| e.into_inner());
    q.push((stop, join));
    reap(&mut q);
}

/// 把已经退出的线程 join 掉并关掉它的停止事件。
fn reap(q: &mut Vec<(usize, JoinHandle<()>)>) {
    let mut i = 0;
    while i < q.len() {
        if q[i].1.is_finished() {
            let (stop, join) = q.remove(i);
            let _ = join.join();
            unsafe { CloseHandle(stop as HANDLE) };
        } else {
            i += 1;
        }
    }
}

/// 清掉之前攒下的监听线程。上层每次重挂监听时调一下，不必另开回收线程。
pub fn reap_retired() {
    let mut q = RETIRED.lock().unwrap_or_else(|e| e.into_inner());
    reap(&mut q);
}

/// 线程主体：发一次请求 → 等「有变化」或「该停了」→ 投递消息 → 再来一次。
fn watcher_loop(dir: &Path, stop_raw: usize, hwnd: isize, msg: u32) {
    let stop = stop_raw as HANDLE;

    // 打开目录本身也可能很慢（离线网络盘），所以它在线程里做，
    // 不占 UI 线程。等待期间上层若已经要求停止，直接收工。
    if unsafe { WaitForSingleObject(stop, 0) } == WAIT_OBJECT_0 {
        return;
    }

    let wide = to_wide_path(dir);
    // SAFETY: wide 是 NUL 结尾宽串；文件名参数传目录时必须带
    // FILE_FLAG_BACKUP_SEMANTICS，否则一律 ACCESS_DENIED。
    // FILE_FLAG_OVERLAPPED 是下面 `ReadDirectoryChangesW` 不阻塞的前提。
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return;
    }

    // 手动重置：一次置位，后面每一轮等待都会立刻返回，退出不会漏看。
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() {
        unsafe { CloseHandle(handle) };
        return;
    }

    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_SIZE
        | FILE_NOTIFY_CHANGE_ATTRIBUTES;

    let mut buf = [0u8; 8192];
    loop {
        if unsafe { WaitForSingleObject(stop, 0) } == WAIT_OBJECT_0 {
            break;
        }

        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event;
        unsafe { ResetEvent(event) };

        let mut returned = 0u32;
        // SAFETY: 句柄以 FILE_FLAG_OVERLAPPED 打开，ov 用本轮的栈变量，
        // 且下面等到完成（或取消）之后才会进入下一轮——缓冲区不会提前失效。
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                0, // 不递归：只关心这一层
                filter,
                &mut returned,
                &mut ov,
                None,
            )
        };
        if ok == 0 {
            break;
        }

        let waits = [event, stop];
        let r = unsafe { WaitForMultipleObjects(2, waits.as_ptr(), 0, u32::MAX) };
        if r == WAIT_OBJECT_0 {
            if hwnd != 0 {
                unsafe { PostMessageW(hwnd as *mut std::ffi::c_void, msg, 0, 0) };
            }
            // 突发变化合并：歇一会儿再听下一轮。合并期间**必须重新挂起
            // 一次 ReadDirectoryChangesW**——空窗里没有挂起请求的话，
            // 内核不记这期间的变化，落进去的文件就再也不触发刷新了。
            let mut coalesce_ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            coalesce_ov.hEvent = event;
            unsafe { ResetEvent(event) };
            let mut coalesce_returned = 0u32;
            let requeued = unsafe {
                ReadDirectoryChangesW(
                    handle,
                    buf.as_mut_ptr().cast(),
                    buf.len() as u32,
                    0,
                    filter,
                    &mut coalesce_returned,
                    &mut coalesce_ov,
                    None,
                ) != 0
            };
            unsafe { WaitForSingleObject(stop, COALESCE_MS) };
            if requeued {
                // 等待窗口里若真的又来了变化：事件已置位，直接投递。
                // 下一轮循环会先复位再重挂，不会重复消费这次完成。
                if unsafe { WaitForSingleObject(event, 0) } == WAIT_OBJECT_0 && hwnd != 0 {
                    unsafe { PostMessageW(hwnd as *mut std::ffi::c_void, msg, 0, 0) };
                }
            }
        } else {
            // 停止（或等待失败）：取消挂起的请求，别把线程留在内核调用里
            unsafe {
                CancelIoEx(handle, &ov);
                break;
            }
        }
    }

    unsafe {
        CloseHandle(event);
        CloseHandle(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 挂上监听再立刻丢掉必须马上返回。
    ///
    /// 这是那次「某些文件夹一点就未响应」的回归测试：句柄少了
    /// `FILE_FLAG_OVERLAPPED` 时，`ReadDirectoryChangesW` 会在调用线程里
    /// 同步等，`drop` 的 join 就一直等到该目录里有文件变动为止。
    /// 用一个安静的目录反复挂/拆，阻塞版会直接挂死。
    #[test]
    fn drop_does_not_wait_for_a_change() {
        let dir = std::env::temp_dir();
        let t0 = Instant::now();
        for _ in 0..3 {
            let w = DirWatcher::spawn(&dir, 0, 0).expect("临时目录应能挂上监听");
            drop(w);
        }
        let used = t0.elapsed();
        assert!(used < Duration::from_secs(2), "监听收尾太慢：{used:?}");
    }
}
