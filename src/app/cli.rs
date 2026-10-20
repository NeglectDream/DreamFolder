//! 命令行解析（开关取自原软件 PE 字符串里的实际参数）。
//!
//! 单独成文件：这一段是纯字符串处理，与窗口状态机无关，
//! 留在 `app.rs` 里只会让那个文件更难读。

use std::path::PathBuf;

use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

use crate::fs;

// ------------------------------------------------------------------ 命令行

/// 命令行开关。全部来自原软件 PE 字符串中的实际参数。
#[derive(Debug, Clone, Default)]
pub struct CommandLine {
    /// 要打开的路径（裸参数）。
    pub path: Option<PathBuf>,
    /// `/OpenInNewWindow` —— 强制新窗口，不做单实例投递。
    pub new_window: bool,
    /// `/lowrun` —— 低优先级启动（原软件用于开机自启场景）。
    pub low_run: bool,
    /// `/autorun` —— 随系统启动。
    pub autorun: bool,
    /// `/replace_explorer_all` —— 接管资源管理器后退出。
    pub replace_explorer_all: bool,
    /// `/restore_explorer_all` —— 恢复资源管理器后退出。
    pub restore_explorer_all: bool,
    /// `/method=search /searchkey="..."` —— 直接发起搜索。
    pub search_key: Option<String>,
    /// `/curdir="..."` —— 启动目录。
    pub cur_dir: Option<PathBuf>,
    /// `/from=...` —— 来源标记（其余产品唤起）。
    pub from: Option<String>,
}

impl CommandLine {
    /// 解析真实命令行。使用 `GetCommandLineW` 以避免 ANSI 代码页损失。
    pub fn parse() -> CommandLine {
        let argv = raw_argv();
        let mut cl = CommandLine::default();

        let mut i = 1usize; // 跳过 exe 自身
        while i < argv.len() {
            let a = &argv[i];
            let lower = a.to_ascii_lowercase();

            if lower == "/openinnewwindow" {
                cl.new_window = true;
            } else if lower == "/lowrun" {
                cl.low_run = true;
            } else if lower == "/autorun" {
                cl.autorun = true;
            } else if lower == "/replace_explorer_all" {
                cl.replace_explorer_all = true;
            } else if lower == "/restore_explorer_all" {
                cl.restore_explorer_all = true;
            } else if let Some(v) = strip_prefix_ci(a, "/method=") {
                // 仅识别 search；其它方法暂未使用
                let _ = v;
            } else if let Some(v) = strip_prefix_ci(a, "/searchkey=") {
                cl.search_key = Some(unquote(v));
            } else if let Some(v) = strip_prefix_ci(a, "/curdir=") {
                cl.cur_dir = Some(PathBuf::from(unquote(v)));
            } else if let Some(v) = strip_prefix_ci(a, "/from=") {
                cl.from = Some(v.to_string());
            } else if a.starts_with('/') {
                // 未知开关：忽略（保持向前兼容）
            } else {
                // 裸路径参数
                let p = unquote(a);
                if !p.is_empty() {
                    cl.path = fs::resolve_path(&p);
                }
            }
            i += 1;
        }

        cl
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// 取原始命令行参数（UTF-16 → String）。
fn raw_argv() -> Vec<String> {
    unsafe {
        let cmd = GetCommandLineW();
        let mut argc = 0i32;
        let argv_ptr = CommandLineToArgvW(cmd, &mut argc);
        if argv_ptr.is_null() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(argc as usize);
        for i in 0..argc {
            let p = *argv_ptr.add(i as usize);
            out.push(from_wide_ptr_safe(p));
        }
        LocalFree(argv_ptr as *mut core::ffi::c_void);
        out
    }
}

unsafe fn from_wide_ptr_safe(p: *const u16) -> String {
    crate::win::from_wide_ptr(p)
}

#[allow(non_snake_case)] // 沿用 Win32 原名，便于与 MSDN 对照
unsafe fn LocalFree(p: *mut core::ffi::c_void) {
    let _ = windows_sys::Win32::Foundation::LocalFree(p);
}
