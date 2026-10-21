//! 窗口过程：把 Win32 消息翻译成 `Application` 上的动作。
//!
//! 自绘 UI 没有子窗口，所有交互都从这里进：命中测试、键盘、拖放、
//! DPI 变化，以及三个定时器（插入符闪烁 / 悬停提示 / 搜索超时）。
//! 这里只做「消息 → 方法调用」的翻译，状态改动都在 `Application` 上。

use super::*;

// ------------------------------------------------------------------ 窗口过程

/// 窗口过程。所有消息都转发到 `Application` 上的方法。
pub(crate) unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // WM_NCCREATE 时把 Application 指针写入窗口
    if msg == WM_NCCREATE {
        let cs = lparam as *const CREATESTRUCTW;
        if !cs.is_null() {
            let p = (*cs).lpCreateParams as *mut Application;
            winwnd::set_user_data(hwnd, p);
            if !p.is_null() {
                (*p).hwnd = hwnd;
            }
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let app = winwnd::user_data::<Application>(hwnd);
    if app.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *app;

    match msg {
        WM_CREATE => {
            app.recompute_layout();
            0
        }
        WM_SIZE => {
            use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, RDW_INVALIDATE, RDW_UPDATENOW};
            app.recompute_layout();
            // 立即同步重绘。只置 invalid 的话，新露出的那条区域在下一帧
            // WM_PAINT 之前是未初始化的表面——拖动边缘缩放、最大化/还原时
            // 边缘会一闪白。同步画一帧的代价和必然后发生的那一帧相同。
            RedrawWindow(
                hwnd,
                std::ptr::null(),
                std::ptr::null_mut(),
                RDW_INVALIDATE | RDW_UPDATENOW,
            );
            0
        }
        // 无边框窗口：非客户区（那条系统画的激活边框）不需要，
        // 也不该在焦点切换那一刻按主题色重画一遍——浅色主题下就是
        // 一闪而过的一条白边。返回 TRUE 表示「激活状态我已经处理」。
        WM_NCACTIVATE => 1,
        // 同上：DefWindowProc 会按 WS_THICKFRAME 画默认 sizing frame，
        // 我们整窗自绘，轮不到它。
        WM_NCPAINT => 0,
        WM_DPICHANGED => {
            app.update_dpi(hwnd);
            // 使用系统建议的窗口矩形
            let sug = lparam as *const RECT;
            if !sug.is_null() {
                let r = *sug;
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !hdc.is_null() {
                let client = winwnd::client_rect(hwnd);
                let w = client.right - client.left;
                let h = client.bottom - client.top;
                if w > 0 && h > 0 {
                    let (mem, recreated) = app.back_buffer.acquire(hdc, w, h);
                    if !mem.is_null() {
                        // 位图刚重建时内存 DC 里什么都没有，必须整窗重画；
                        // 否则只重画系统标的脏区——离屏位图里上一帧的
                        // 其余内容还在，没变的地方不用重画。
                        let dirty = if recreated { client } else { ps.rcPaint };
                        if dirty.right > dirty.left && dirty.bottom > dirty.top {
                            draw::paint(app, mem, dirty);
                            app.back_buffer.blit_to(
                                hdc,
                                dirty.left,
                                dirty.top,
                                dirty.right - dirty.left,
                                dirty.bottom - dirty.top,
                            );
                        }
                    }
                }
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1, // 背景由双缓冲覆盖，禁止系统擦除
        WM_MOUSEMOVE => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            app.ensure_mouse_leave_tracking();
            interact::on_mouse_move(app, x, y);
            0
        }
        WM_MOUSELEAVE => {
            app.tracking_mouse = false;
            interact::on_mouse_leave(app);
            0
        }
        WM_LBUTTONDOWN => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            SetCapture(hwnd);
            interact::on_lbutton_down(app, x, y);
            0
        }
        WM_LBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            ReleaseCapture();
            interact::on_lbutton_up(app, x, y);
            0
        }
        // 自己处理右键抬起并返回 0，DefWindowProc 就不会再合成 WM_CONTEXTMENU，
        // 从而避免菜单被弹出两次。
        WM_RBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_rbutton_up(app, x, y);
            0
        }
        WM_LBUTTONDBLCLK => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_lbutton_dblclk(app, x, y);
            0
        }
        WM_MBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_mbutton_up(app, x, y);
            0
        }
        // 鼠标侧键：XBUTTON1 后退、XBUTTON2 前进。键号在 wParam 高字里。
        WM_XBUTTONUP => {
            match ((wparam >> 16) & 0xFFFF) as u16 {
                1 => app.go_back(),
                2 => app.go_forward(),
                _ => {}
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            interact::on_mousewheel(app, delta);
            0
        }
        WM_MOUSEHWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            interact::on_hwheel(app, delta);
            0
        }
        WM_KEYDOWN => {
            // Alt 状态来自 GetKeyState
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_MENU};
            app.modifiers.alt = (GetKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
            interact::update_modifiers(app);
            if interact::on_key_down(app, wparam) {
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CHAR => {
            let ch = wparam as u32;
            if let Some(c) = char::from_u32(ch) {
                interact::on_char(app, c);
            }
            0
        }
        // IME 开始组合（用户按下输入法第一个键）与激活上下文时，把
        // 组合窗/候选窗挪到当前编辑框插入符旁。不接这两条，自绘窗口
        // 里中文候选窗会飘到屏幕左上角
        WM_IME_STARTCOMPOSITION => {
            app.place_ime_window(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_IME_SETCONTEXT => {
            app.place_ime_window(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_SETFOCUS => {
            app.active_tab_focused = true;
            app.request_repaint();
            0
        }
        WM_KILLFOCUS => {
            app.active_tab_focused = false;
            app.request_repaint();
            0
        }
        WM_DROPFILES => {
            app.on_drop_files(wparam as *mut core::ffi::c_void);
            0
        }
        crate::win::droptarget::WM_DREAMFOLDER_DROP => {
            app.handle_external_drop();
            0
        }
        // 后台线程干完活投过来的唤醒：把队里的结果一次取干净。
        // 同一个消息号可能被一次批量投递多次，多出来的空转可忽略。
        crate::io::WM_APP_IO => {
            app.drain_io();
            0
        }
        // 被监听的目录有变化。复制几百个文件会连投上百条，这里只负责起
        // 一个防抖定时器，真正刷新放到 TIMER_FS 到点之后。
        crate::app::WM_APP_FS => {
            unsafe {
                SetTimer(hwnd, TIMER_FS, crate::app::FS_DEBOUNCE_MS, None);
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_CARET {
                // 还有编辑态就催一帧重绘让插入符翻面；都结束了就自己停。
                // 只失效编辑框那一条：插入符闪烁是高频周期事件，整窗重绘
                // 等于每半秒把整个界面重画一遍。
                let dialog_caret = app
                    .dialog
                    .as_ref()
                    .map(|d| d.has_input())
                    .unwrap_or(false);
                if app.search_focused {
                    let r = app.layout.search;
                    app.request_repaint_rect(r);
                } else if app.address_editing {
                    let r = app.layout.address;
                    app.request_repaint_rect(r);
                } else if app.rename_target.is_some() || dialog_caret {
                    // 重命名的那一格和对话框输入框没有现成的矩形，
                    // 这两处闪烁频率本来就低，整窗重绘可以接受
                    app.request_repaint();
                } else {
                    KillTimer(hwnd, TIMER_CARET);
                }
                return 0;
            }
            if wparam == TIMER_HOVER {
                // 一次性：到点就停，只在指针仍停在该按钮上时才亮出提示条
                KillTimer(hwnd, TIMER_HOVER);
                if matches!(app.hover, ui::Hit::ToolbarButton(_)) {
                    app.tooltip_ready = true;
                    app.request_repaint();
                }
                return 0;
            }
            if wparam == TIMER_SEARCH {
                // 一次性：等太久就别再转圈了，界面回到可交互状态。
                KillTimer(hwnd, TIMER_SEARCH);
                app.on_search_timeout();
                return 0;
            }
            if wparam == TIMER_NOTE {
                // 一次性：状态栏提示到点，回落到常规统计
                KillTimer(hwnd, TIMER_NOTE);
                app.status_note = None;
                app.request_repaint();
                return 0;
            }
            if wparam == TIMER_FS {
                // 一次性：目录变化的防抖窗口结束，这时候刷才不会一次刷十几遍
                KillTimer(hwnd, TIMER_FS);
                app.apply_fs_change();
                return 0;
            }
            if wparam == TIMER_PIPE {
                // 轮询单实例投递。先把报文取出来再操作 app：
                // 管道借用活在 &mut app.pipe 上，直接在里面调 app 的方法会冲突。
                let incoming = match &mut app.pipe {
                    Some(pipe) => {
                        if pipe.try_accept() {
                            pipe.read_message()
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                if let Some(msg) = incoming {
                    // 外部唤起（接管后双击文件夹、拖文件到 exe 图标上）一律
                    // 合并成新标签——像浏览器那样，绝不覆盖用户正在看的标签。
                    if let Some(p) = crate::ipc::parse_message(&msg) {
                        app.new_tab(Some(p));
                    }
                    // 把窗口带到前台。最小化时必须先还原：只调
                    // SetForegroundWindow 对一个最小化窗口没有可见效果，
                    // 二次启动就会像「双击了但什么都没发生」。
                    unsafe {
                        if IsIconic(hwnd) != 0 {
                            ShowWindow(hwnd, SW_RESTORE);
                        }
                        SetForegroundWindow(hwnd);
                    }
                }
            }
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut MINMAXINFO;
            if !mmi.is_null() {
                let s = app.dpi_scale;
                (*mmi).ptMinTrackSize.x =
                    (ui::metrics::MIN_WINDOW_W as f32 * s).round() as i32;
                (*mmi).ptMinTrackSize.y =
                    (ui::metrics::MIN_WINDOW_H as f32 * s).round() as i32;
            }
            0
        }
        WM_NCCALCSIZE => {
            // 无边框窗口：客户区必须等于整个窗口矩形。
            //
            // 返回值是关键——返回 1 才表示「lparam 里的 rgrc[0] 就是客户区」；
            // 返回 0 会让系统按默认规则**再扣一次** WS_THICKFRAME 的调整边框，
            // 于是客户区比窗口尺寸小一圈（左右各 8px 左右），布局在这个错误的
            // 尺寸上计算，整页内容随之偏移、挤压。
            if wparam == 0 {
                // lParam 是 RECT*：无边框窗口基本不走这条，交给系统默认
                return 0;
            }
            let params = lparam as *mut NCCALCSIZE_PARAMS;
            if params.is_null() {
                return 0;
            }
            if IsZoomed(hwnd) != 0 {
                // 最大化时客户区取显示器**工作区**，否则会盖住任务栏
                (*params).rgrc[0] = monitor_info(hwnd);
            }
            // 非最大化：保持系统传入的窗口矩形，即客户区铺满整个窗口
            1
        }
        WM_NCHITTEST => {
            // 允许拖动调整大小
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut r);
            let border = (6.0 * app.dpi_scale).round() as i32;

            let left = x < r.left + border;
            let right = x >= r.right - border;
            let top = y < r.top + border;
            let bottom = y >= r.bottom - border;

            let hit = if top && left {
                HTTOPLEFT
            } else if top && right {
                HTTOPRIGHT
            } else if bottom && left {
                HTBOTTOMLEFT
            } else if bottom && right {
                HTBOTTOMRIGHT
            } else if left {
                HTLEFT
            } else if right {
                HTRIGHT
            } else if top {
                HTTOP
            } else if bottom {
                HTBOTTOM
            } else {
                HTCLIENT
            };
            return hit as LRESULT;
        }
        WM_SETCURSOR => {
            // lParam 低字是 WM_NCHITTEST 的结果。
            //
            // 这里必须把每个 HT 都显式映射成光标：窗口类注册时用的是
            // IDC_ARROW，而 DefWindowProc 只在「系统非客户区」里替我们挑
            // 缩放光标；我们是无边框窗口、边框命中全由 WM_NCHITTEST 自己
            // 决定，返回 0 让系统兜底的结果就是一直显示箭头。
            let code = lo_word(lparam);
            let cursor = match code {
                x if x == HTLEFT as i32 || x == HTRIGHT as i32 => IDC_SIZEWE,
                x if x == HTTOP as i32 || x == HTBOTTOM as i32 => IDC_SIZENS,
                x if x == HTTOPLEFT as i32 || x == HTBOTTOMRIGHT as i32 => IDC_SIZENWSE,
                x if x == HTTOPRIGHT as i32 || x == HTBOTTOMLEFT as i32 => IDC_SIZENESW,
                x if x == HTCLIENT as i32 => {
                    if app.resizing_nav
                        || app.resizing_detail
                        || app.drag_col.is_some()
                        || app.hover == crate::ui::Hit::NavSplitter
                        || app.hover == crate::ui::Hit::DetailSplitter
                        || matches!(app.hover, crate::ui::Hit::DetailCol(_))
                    {
                        IDC_SIZEWE
                    } else if app.address_editing || app.search_focused {
                        IDC_IBEAM
                    } else {
                        IDC_ARROW
                    }
                }
                _ => IDC_ARROW,
            };
            SetCursor(LoadCursorW(std::ptr::null_mut(), cursor));
            1
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            app.recompute_layout();
            app.request_repaint();
            0
        }
        NAV_MSG => {
            // 只加载最终目标，中间被覆盖掉的目标直接丢弃
            if let Some(target) = app.pending_nav.take() {
                app.load_into_active(target, false);
            }
            0
        }
        WM_CLOSE => {
            app.request_close();
            0
        }
        WM_DESTROY => {
            app.save_state();
            // 监听线程手里握着这个窗口的句柄。留着等 Application 析构才收
            // 也可以，但窗口句柄一旦被系统回收复用，迟到的消息会投错地方。
            app.watcher = None;
            if app.pipe.is_some() {
                KillTimer(hwnd, TIMER_PIPE);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 首次启动的窗口尺寸：按主显示器工作区推导。
///
/// 旧版本把 1000×640 硬编码在设置默认值里——那在 1080p 及以上的屏幕上偏小，
/// 且完全不随屏幕变化。这里改成按工作区算，并夹在最小尺寸与工作区之间。
///
/// 注意：本函数在**窗口创建前**调用，此时没有 hwnd 可用，因此走
/// `SystemParametersInfoW(SPI_GETWORKAREA)` 而不是 `MonitorFromWindow`。
/// 它返回主显示器工作区，多屏下可能与鼠标所在屏不同，但只会影响首次启动的
/// 一个初始位置，用户拖一次窗口后就被保存的几何取代。
pub(crate) fn default_window_size() -> (i32, i32) {
    /// 取不到工作区时的兜底（与旧默认值一致，至少不会崩）。
    const FALLBACK: (i32, i32) = (1000, 640);

    let mut work: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut core::ffi::c_void,
            0,
        )
    };
    if ok == 0 {
        return FALLBACK;
    }

    let ww = work.right - work.left;
    let wh = work.bottom - work.top;
    if ww <= 0 || wh <= 0 {
        return FALLBACK;
    }

    // 宽取工作区的 5/8、高取 3/4：在 1920×1040 的工作区上得到 1200×780，
    // 与资源管理器在 1080p 下打开时的观感接近。
    let w = (ww * 5 / 8).clamp(ui::metrics::MIN_WINDOW_W, ww);
    let h = (wh * 3 / 4).clamp(ui::metrics::MIN_WINDOW_H, wh);
    (w, h)
}

unsafe fn monitor_info(hwnd: HWND) -> RECT {
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if GetMonitorInfoW(mon, &mut mi) != 0 {
        mi.rcWork
    } else {
        std::mem::zeroed()
    }
}

/// 取 lParam 低 16 位，**按有符号**解释（等价于 `GET_X_LPARAM`）。
///
/// 鼠标消息里的客户区坐标可以是负数：按下时 `SetCapture` 会持续把消息送进
/// 窗口，用户把光标拖到客户区左上角外面，x/y 就是负值。按无符号读会变成
/// 65xxx，框选的终点于是被夹到视图的**右下角**，整块选择框直接翻到对面
/// （拖动翻页、拖动滚动条也一样会被这一下带偏）。
pub(crate) fn lo_word(lparam: LPARAM) -> i32 {
    (lparam & 0xFFFF) as i16 as i32
}

/// 取 lParam 高 16 位，按有符号解释（等价于 `GET_Y_LPARAM`）。理由同 [`lo_word`]。
pub(crate) fn hi_word(lparam: LPARAM) -> i32 {
    ((lparam >> 16) & 0xFFFF) as i16 as i32
}
