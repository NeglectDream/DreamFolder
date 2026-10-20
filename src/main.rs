//! DreamFolder — DreamFolder 的 Rust 原生重写
//!
//! 设计目标（按优先级）：
//!   1. 冷启动：直接 Win32 + 自绘，无框架、无 GC、无 CRT 依赖，入口即建窗
//!   2. 功能等价：多标签、收藏栏、导航、8 种视图、搜索、接管资源管理器
//!   3. 可维护：模块边界与原始 DuiLib XML 布局一一对应
//!
//! 与源软件的对应关系见 README.md 的「功能映射」表。

#![windows_subsystem = "windows"] // 发布版不弹控制台；调试期见下方 cfg

// 本项目是「行为对齐」式迁移：一批系统调用包装（自定义文件夹图标、
// 缩略图缩放绘制、接管资源管理器的分用户重载、地址栏历史补全、
// 书签编辑等）已经按原软件语义写好，只是尚未全部接进 UI 入口。
// 它们不是遗留垃圾——删掉之后重新接线还要把 FFI 细节再写一遍，
// 因此在这里集中声明一次，避免几十条 dead_code 噪音淹没真正的告警。
#![allow(dead_code)]

mod app;
mod cfg;
mod fs;
mod ipc;
mod shell_integration;
mod strings;
mod ui;
mod win;

use std::panic;

fn main() {
    // 安装 panic 钩子：release 下 panic=abort，这里确保任何异常都能写日志而非静默死掉
    panic::set_hook(Box::new(|info| {
        let msg = format!("[DreamFolder] panic: {info}\n");
        // 直接写文件，避免依赖任何未初始化状态
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(cfg::paths::crash_log())
        {
            use std::io::Write;
            let _ = f.write_all(msg.as_bytes());
        }
    }));

    // 全局初始化：COM（用于 Shell 图标/上下文菜单）、DPI、GDI+
    win::init::init_process();

    let args = app::CommandLine::parse();

    // ---- 单实例：已存在实例则把本次请求通过命名管道投递过去 ----
    match ipc::route_to_existing(&args) {
        ipc::RouteResult::Delivered => return,
        ipc::RouteResult::NoServer => {}
        ipc::RouteResult::Failed => {}
    }

    // ---- 建立主程序状态并进入消息循环 ----
    let mut application = app::Application::new(args);
    application.run();
}
