//! 后台工作线程：把会阻塞的操作挪出 UI 线程。
//!
//! 以前所有文件系统调用都直接跑在窗口线程上：枚举目录、查盘符容量、
//! 读预览、系统索引搜索、复制删除。只要其中一个慢——离线的映射网络盘、
//! 几百兆的日志、索引服务抽风——整个窗口就冻在那里，连重绘都做不了。
//!
//! 这里的约定很简单：
//!   UI 线程 `Io::send(Job)` → 工作线程执行 → 结果塞回通道 →
//!   `PostMessage(WM_APP_IO)` 把 UI 唤醒 → `Application::drain_io` 逐个应用。
//!
//! 每个 job 起一条线程而不是排队执行：一次卡住的索引查询不该把后面的
//! 目录枚举堵死。请求带自增 `id`，UI 侧只认最新那个，过期结果直接丢。

use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::fs::enumerate::ListOptions;
use crate::fs::entry::{Entry, Volume};
use crate::fs::ops::OpResult;
use crate::ui::preview::PreviewData;

/// 「后台干完了」的自定义消息，UI 线程收到后去取结果。
pub const WM_APP_IO: u32 = WM_APP + 1;

/// 目录列表的种类。虚拟视图没有真实目录，取数方式不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Dir,
    Computer,
    Recycle,
}

/// 交给后台的活。
pub enum Job {
    /// 枚举目录（含排序）。
    List {
        id: u64,
        tab: usize,
        path: PathBuf,
        opts: ListOptions,
        kind: ListKind,
    },
    /// 查驱动器的卷标与容量。离线网络盘会让这类调用卡上几十秒，
    /// 所以必须离开 UI 线程，而且要和「先把盘符列出来」分开。
    Volumes {
        id: u64,
        tab: usize,
        drives: Vec<PathBuf>,
    },
    /// 读预览内容（文本解码 / 图片原始字节）。解码仍在 UI 线程做，
    /// 见 `ui::preview`——GDI+ 对象不跨线程。
    Preview {
        id: u64,
        path: PathBuf,
        ext: String,
        size: u64,
    },
    /// 图片缩略图。与 `Preview` 不同：解码放后台——网格视图一屏可能
    /// 有几十张图片，主线程逐张解码就是秒级卡顿。GDI+ 对象在后台
    /// 线程内自产自销，跨线程只传 `HICON` 句柄（用户对象无线程亲和）。
    Thumb { path: PathBuf },
    /// 搜索：系统索引 + 子树扫描 + 已加载条目兜底。
    Search {
        id: u64,
        tab: usize,
        scope: PathBuf,
        key: String,
        limit: usize,
        budget_ms: u32,
        /// 最新请求号。后台在长循环里拿它判断自己是否已经过期。
        latest: Arc<AtomicU64>,
    },
    /// 文件操作。Shell 会弹自己的进度/冲突 UI，窗口本身不该被占住。
    Op { what: OpWhat },
}

/// 可以后台执行的文件操作。只收「耗时不可控」的那几种；
/// 重命名、新建目录是一个系统调用的事，留在前台更简单。
pub enum OpWhat {
    Copy { items: Vec<PathBuf>, target: PathBuf },
    Move { items: Vec<PathBuf>, target: PathBuf },
    Delete { items: Vec<PathBuf>, permanent: bool },
}

/// 一个驱动器的卷标与容量。
pub struct DriveInfo {
    pub root: PathBuf,
    pub label: String,
    pub volume: Option<Volume>,
}

/// 后台执行完的结果。
pub enum Done {
    Listed {
        id: u64,
        tab: usize,
        path: PathBuf,
        kind: ListKind,
        entries: Vec<Entry>,
        error: Option<String>,
    },
    Volumes {
        id: u64,
        tab: usize,
        infos: Vec<DriveInfo>,
    },
    Previewed {
        id: u64,
        path: PathBuf,
        data: PreviewData,
    },
    /// 缩略图解码结果。`icon == 0` 表示失败（缓存层据此防重试）。
    Thumbed { path: PathBuf, icon: isize },
    Searched {
        id: u64,
        tab: usize,
        key: String,
        hits: Vec<Entry>,
    },
    OpDone {
        result: OpResult,
    },
}

/// 后台任务的发送端与结果接收端。
pub struct Io {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
    /// 结果就绪时用来 `PostMessage` 唤醒 UI。窗口创建前是 0。
    hwnd: Arc<AtomicIsize>,
}

impl Default for Io {
    fn default() -> Self {
        Self::new()
    }
}

impl Io {
    pub fn new() -> Io {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let hwnd = Arc::new(AtomicIsize::new(0));

        let notifier = Arc::clone(&hwnd);
        std::thread::Builder::new()
            .name("dreamfolder-io".to_string())
            .spawn(move || {
                // 只做分发：每个 job 一条线程，避免一个卡死的请求挡住后面的活
                for job in job_rx {
                    let tx = done_tx.clone();
                    let notify = Arc::clone(&notifier);
                    let spawned = std::thread::Builder::new()
                        .name("dreamfolder-job".to_string())
                        .spawn(move || run(job, tx, notify));
                    // 线程起不来（极端资源耗尽）时不能把结果通道弄丢：
                    // 直接把 Done 丢掉即可，UI 侧的状态由 id 过期兜底。
                    if spawned.is_err() {
                        continue;
                    }
                }
            })
            .expect("无法启动后台 IO 线程");

        Io {
            jobs: job_tx,
            done: done_rx,
            hwnd,
        }
    }

    /// 窗口创建后绑定句柄，后台才有地方投递「结果就绪」。
    pub fn bind(&mut self, hwnd: HWND) {
        self.hwnd.store(hwnd as isize, Ordering::SeqCst);
    }

    pub fn send(&self, job: Job) {
        // 发送失败说明后台线程没了（只可能是进程退出中），忽略即可
        let _ = self.jobs.send(job);
    }

    /// 取一条结果（非阻塞）。UI 线程在 `WM_APP_IO` 里循环调用。
    pub fn try_recv(&self) -> Option<Done> {
        self.done.try_recv().ok()
    }
}

fn run(job: Job, tx: mpsc::Sender<Done>, hwnd: Arc<AtomicIsize>) {
    let done = execute(job);
    if tx.send(done).is_err() {
        return;
    }
    let h = hwnd.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            PostMessageW(h as HWND, WM_APP_IO, 0, 0);
        }
    }
}

fn execute(job: Job) -> Done {
    use crate::fs::enumerate;

    match job {
        Job::List {
            id,
            tab,
            path,
            opts,
            kind,
        } => {
            let (entries, error) = match kind {
                ListKind::Recycle => (crate::fs::recycle::list(), None),
                ListKind::Computer => (enumerate::list_computer(&opts).entries, None),
                ListKind::Dir => match enumerate::list_dir(&path, &opts) {
                    Ok(listing) => (listing.entries, None),
                    Err(e) => (Vec::new(), Some(e)),
                },
            };
            Done::Listed {
                id,
                tab,
                path,
                kind,
                entries,
                error,
            }
        }

        Job::Volumes { id, tab, drives } => {
            let infos = drives
                .into_iter()
                .map(|root| DriveInfo {
                    label: enumerate::drive_label(&root),
                    volume: enumerate::volume_of(&root),
                    root,
                })
                .collect();
            Done::Volumes { id, tab, infos }
        }

        Job::Preview {
            id,
            path,
            ext,
            size,
        } => {
            let data = crate::ui::preview::load_data(&path, &ext, size);
            Done::Previewed { id, path, data }
        }

        Job::Thumb { path } => {
            let icon = crate::win::gdiplus::Image::load(&path)
                .map(|img| img.to_hicon_at(crate::fs::thumbs::THUMB_SIDE) as isize)
                .unwrap_or(0);
            Done::Thumbed { path, icon }
        }

        Job::Search {
            id,
            tab,
            scope,
            key,
            limit,
            budget_ms,
            latest,
        } => {
            let hits = crate::fs::search::search(
                &scope,
                &key,
                limit,
                budget_ms,
                // 过期就不再往下扫：用户已经改了搜索词或换了目录
                || latest.load(Ordering::SeqCst) != id,
            );
            Done::Searched {
                id,
                tab,
                key,
                hits,
            }
        }

        Job::Op { what } => {
            // 用 0 当 owner：操作在后台线程上，Shell 会在自己的线程里弹进度框
            let result = match what {
                OpWhat::Copy { items, target } => {
                    crate::fs::ops::copy_to(std::ptr::null_mut(), &items, &target)
                }
                OpWhat::Move { items, target } => {
                    crate::fs::ops::move_to(std::ptr::null_mut(), &items, &target)
                }
                OpWhat::Delete { items, permanent } => {
                    if permanent {
                        crate::fs::ops::delete_permanent(std::ptr::null_mut(), &items)
                    } else {
                        crate::fs::ops::delete_to_recycle(std::ptr::null_mut(), &items)
                    }
                }
            };
            Done::OpDone { result }
        }
    }
}
