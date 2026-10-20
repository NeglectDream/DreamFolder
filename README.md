# 360Folder — 360文件夹 的 Rust 原生重写

用 Rust + 纯 Win32 重写的多标签文件管理器，目标是**极致冷启动速度**与**可维护的代码结构**。

本项目通过静态逆向原软件 `360FileBrowser.exe`（PE 分析、资源解析、字符串提取）还原其完整功能矩阵，然后用 Rust 重新实现。

---

## 一、原软件分析结论（逆向证据）

### 1.1 基本信息

| 项目 | 值 |
|---|---|
| 产品名 | 360文件夹 |
| 可执行文件 | `360FileBrowser.exe`（2,500,352 字节） |
| SHA-256 | `95fed599fddc6e76adb28b1d0c638e63c28677df269af279be8fbf4c3d818e07` |
| 架构 | PE32+ / x86-64 |
| 子系统 | Windows GUI |
| 保护 | HIGH_ENTROPY_VA、DYNAMIC_BASE、NX_COMPAT、TERMINAL_SERVICE_AWARE |
| 链接器 | MSVC 14.16（Visual Studio 2017） |
| 编译时间戳 | 2026-07-14 20:11:11 |
| UI 框架 | **DuiLib**（由 `.ui` 包内的 XML 布局语法判定） |
| 代码签名 | GlobalSign 代码签名证书 + 时间戳 |

### 1.2 依赖

```
KERNEL32  USER32  GDI32  ADVAPI32  SHELL32  ole32  OLEAUT32
SHLWAPI   WS2_32  VERSION  PSAPI  SETUPAPI  MSIMG32  gdiplus  COMCTL32
```

无 MFC/ATL 依赖，是裸 Win32 + DuiLib 结构。

### 1.3 主题包格式

`360FileBrowser_Theme.ui` / `360FileBrowser_BlackTheme.ui` 实为 **ZIP 压缩包**，内含：

- `*.xml` — DuiLib 布局（窗口、菜单、列表项模板）
- `*.svg` / `*.png` — 图标与位图资源

布局文件清单（共 24 个）：

```
explorer_frame.xml     主窗口框架（标题栏/收藏栏/工具栏/导航栏）
main_menu.xml          主菜单
tab_menu.xml           标签右键菜单
option_menu.xml        显示选项子菜单
view_menu.xml          视图模式菜单
nav_records_menu.xml   历史记录菜单
favoriate_menu.xml            书签右键菜单
favoriate_folder_menu.xml     书签目录右键菜单
favoriate_container_menu.xml  书签容器菜单
DlgSetting.xml         设置对话框
AlertWnd.xml           通用提示框
CloseAlert.xml         关闭确认框
RenameAlert.xml        重命名框
AddFavoriteFolderWnd.xml  新建书签目录
RepairBhoWnd.xml       异常修复
CheckUpateWnd.xml      更新检测
ExistUpateWnd.xml      发现新版本
guide_wnd.xml          引导页
GuidePreviewWnd.xml    引导预览
bookmark_list.xml      书签下拉列表模板
subdir_list.xml        子目录列表模板
tabitem_list.xml       标签管理列表模板
searchswitchlist.xml   搜索模式列表模板
```

### 1.4 从 PE 字符串还原的接口

**命令行开关**（全部实测存在于 UTF-16 字符串表中）：

```
/OpenInNewWindow        在新窗口打开（右键菜单注册用的就是这个）
/lowrun                 低优先级启动
/autorun                随系统启动
/replace_explorer_all   接管资源管理器后退出
/restore_explorer_all   恢复资源管理器后退出
/restore_win_e          恢复 Win+E
/replace_win_e          替换 Win+E
/install_upgrade        安装升级
/install_win_e          安装 Win+E 接管
/from=360FileBrowser    来源标记
/NotifyWnd=%d           通知窗口句柄（IPC）
/method=search /searchkey="%s"   直接搜索
/curdir="%s"            启动目录
```

**进程间通信**：

```
\\.\pipe\360filepipe    命名管道（单实例唤醒）
\\.\360SelfProtection   自我保护设备
```

**配置文件**：

```
360FileBrowser\Profile\favoriates.json   书签（另有 .ini / .dat 变体）
360FileBrowser\Profile\addr_history.ini  地址栏历史
360FileBrowser\Profile\lasttabs.dat      上次会话标签
\ignores.ini                             忽略规则
```

**注册表**：

```
SOFTWARE\360FileBrowser\HotKeys          快捷键配置
HKU\.DEFAULT\Software\Classes\Folder\shell           接管点（见 接管资源管理器.bat）
```

**关联的 360 家族组件**（字符串中出现，用于功能判定）：

```
360Base64.dll  360base.dll  360FolderExt64.dll  Shell 扩展
360QuickPreview.exe  快速预览  360ScreenCapture.exe  截图
360searchlite.exe    搜索      360FileUpdate.exe     更新
360leakfixer.exe     BHO 修复  kittip.dll            工具栏宿主
```

### 1.5 接管资源管理器的方式

`接管资源管理器.bat` 揭示了完整做法（需管理员权限，写 `HKU\.DEFAULT`）：

```bat
reg add "HKU\.DEFAULT\Software\Classes\Folder\shell" /ve /d "360FileBrowser" /f
reg add "HKU\.DEFAULT\Software\Classes\Folder\shell\360FileBrowser" /ve /d "360文件夹" /f
reg add "HKU\.DEFAULT\Software\Classes\Folder\shell\360FileBrowser\command" /ve /d "\"…\360FileBrowser.exe\" \"%%1\"" /f
reg add "HKU\.DEFAULT\Software\Classes\Folder\shell\newwindow" /ve /d "在新窗口打开(&E)" /f
reg add "HKU\.DEFAULT\Software\Classes\Folder\shell\newwindow\command" /ve /d "\"…\360FileBrowser.exe\" /OpenInNewWindow \"%%1\"" /f
```

本项目在 `src/shell_integration.rs` 中**等价实现**了这套注册表操作，并提供 HKCU 版本（无需提权，便于测试）。

---

## 二、功能映射表

| 原软件功能（来源） | Rust 实现位置 | 状态 |
|---|---|---|
| 多标签页浏览 | [src/app.rs](src/app.rs) `Tab` / `new_tab` / `close_tab` | 已实现 |
| 标签右键菜单（新建/复制/新窗口/加书签/关闭/关闭其它） | [src/ui/draw.rs](src/ui/draw.rs) + [src/strings.rs](src/strings.rs) | 文案就绪，菜单待接线 |
| 收藏书签栏（拖拽添加、置顶、重命名、删除、目录分组） | [src/cfg/bookmarks.rs](src/cfg/bookmarks.rs) | 数据结构完成，UI 部分完成 |
| 后退 / 前进 / 向上 | [src/app.rs](src/app.rs) `go_back` / `go_forward` / `go_up` | 已实现 |
| 历史记录下拉 | [src/cfg/history.rs](src/cfg/history.rs) | 数据完成，下拉 UI 待做 |
| 地址栏（面包屑 ⇄ 编辑框） | [src/ui/draw.rs](src/ui/draw.rs) `paint_breadcrumb` | 已实现 |
| 搜索（快速 / 系统 / 网络三模式） | [src/app.rs](src/app.rs) `SearchMode` | 快速搜索已实现 |
| 8 种视图（超大/大/中/小图标、列表、详细信息、平铺、内容） | [src/ui/draw.rs](src/ui/draw.rs) `paint_grid_view` / `paint_rows_view` | 已实现 |
| 显示选项（导航/预览/详情窗格、扩展名、隐藏项） | [src/cfg/settings.rs](src/cfg/settings.rs) | 配置就绪，窗格绘制待做 |
| 工具栏操作（新建/剪切/复制/粘贴/删除/重命名/视图…） | [src/app.rs](src/app.rs) `toolbar_action` | 已实现 |
| 文件操作（回收站删除、复制、移动、重命名、新建） | [src/fs/ops.rs](src/fs/ops.rs) | 已实现（走 SHFileOperation） |
| 空格键快速预览 | [src/app.rs](src/app.rs) `quick_preview_selection` | 文本文件已实现 |
| Ctrl+双击右侧新标签 | [src/ui/interact.rs](src/ui/interact.rs) `on_lbutton_dblclk` | 已实现 |
| 双击标签关闭 | 同上 | 已实现 |
| 标签栏滚轮切换标签 | [src/ui/interact.rs](src/ui/interact.rs) `on_mousewheel` | 已实现 |
| 双击空白返回上级 | 同上 | 已实现 |
| 快捷键系统 | [src/cfg/settings.rs](src/cfg/settings.rs) `default_hotkeys` | 默认表已实现，自定义 UI 待做 |
| 设置持久化 | [src/cfg/](src/cfg/) | 已实现 |
| 单实例 + 命名管道唤醒 | [src/ipc.rs](src/ipc.rs) | 已实现 |
| 接管系统文件管理 | [src/shell_integration.rs](src/shell_integration.rs) | 已实现 |
| 拖放文件/文件夹 | [src/app.rs](src/app.rs) `on_drop_files` | 已实现 |
| 自动更新 | — | 有意不做（见下） |
| 引导页 | [src/strings.rs](src/strings.rs) `GUIDE_PAGES` | 文案就绪，界面待做 |
| BHO 修复 | — | 不适用（本项目不装 BHO） |

---

## 三、冷启动优化

原软件冷启动慢的主要来源与我们的对策：

| 原实现 | 问题 | Rust 实现 |
|---|---|---|
| DuiLib 框架初始化 | 解析 XML、构建控件树、注册窗口类 | **无框架**。布局是编译期常量，[src/ui/mod.rs](src/ui/mod.rs) `Layout::compute` 只做整数运算 |
| 逐文件 `SHGetFileInfo` | 每个文件都问一次 Shell 拿图标 | **按扩展名缓存**，只查一次（[src/fs/icons.rs](src/fs/icons.rs)） |
| 启动即加载全部主题资源 | 解压 .ui 包、解析全部 SVG | **懒加载**，且首版直接用程序化绘制（无 IO） |
| `FindNextFile` 逐条 | 内核反复往返 | **`FIND_FIRST_EX_LARGE_FETCH`** 批量读取（[src/fs/enumerate.rs](src/fs/enumerate.rs)） |
| CRT / C++ 运行时初始化 | 静态构造、locale、异常表 | **`panic=abort` + `lto=fat` + `strip`**，无 C++ 运行时（[Cargo.toml](Cargo.toml)） |
| GDI+ / 图像解码 | 启动时初始化 | 只用 GDI，按需创建 |
| 字体逐个创建 | 每次绘制 `CreateFont` | **`FontCache`** 按 (字号, 粗体) 缓存（[src/win/paint.rs](src/win/paint.rs)） |
| 全部控件独立 HWND | 每个控件一个窗口句柄，消息分发开销 | **零子窗口**，整窗自绘 + 单次 BitBlt |

启动路径被压缩为：`main` → DPI/COM 初始化 → 读配置 → `CreateWindowExW` → 首帧绘制。没有框架层。

---

## 四、目录结构

```
360folder-rs/
├── Cargo.toml              构建配置（release 为冷启动全面优化）
├── build.bat / build.ps1   一键构建脚本
├── src/
│   ├── main.rs             入口
│   ├── app.rs              应用状态机、命令行解析、消息循环
│   ├── strings.rs          全部 UI 文本（取自原 .ui 包）
│   ├── ipc.rs              单实例 + 命名管道
│   ├── shell_integration.rs 注册表接管
│   ├── win/                Win32 原语
│   │   ├── mod.rs          字符串转换、模块句柄
│   │   ├── init.rs         进程初始化（DPI、COM）
│   │   ├── wnd.rs          窗口类、消息循环
│   │   └── paint.rs        双缓冲、Canvas、字体/调色板缓存
│   ├── ui/                 界面层
│   │   ├── mod.rs          布局计算、命中类型
│   │   ├── draw.rs         全部绘制
│   │   └── interact.rs     鼠标/键盘/命中测试
│   ├── fs/                 文件系统
│   │   ├── mod.rs          路径工具、驱动器枚举
│   │   ├── entry.rs        目录项模型
│   │   ├── enumerate.rs    枚举与排序
│   │   ├── icons.rs        图标/类型名缓存
│   │   ├── ops.rs          复制/移动/删除/重命名/新建
│   │   └── shell.rs        ShellExecute、剪贴板
│   └── cfg/                配置持久化
│       ├── mod.rs
│       ├── paths.rs        配置目录解析
│       ├── settings.rs     设置
│       ├── bookmarks.rs    书签
│       ├── history.rs      地址历史
│       └── session.rs      会话标签
```

---

## 五、构建

### 前置条件

- Rust stable ≥ 1.75（`rustup` 安装，或使用 RustRover 自带工具链）
- Windows 10/11 x64
- **一个可用的链接器**（见下）

无需任何额外系统库——只用 `windows-sys` 的原始 FFI 声明。

### 链接器的选择（重要）

Rust 在 Windows 上默认目标是 `x86_64-pc-windows-msvc`，它需要 MSVC 的
`link.exe`（随「Visual Studio 生成工具」的 *C++ 生成工具* 工作负载安装）。
只装了 Windows SDK 而没有 MSVC 编译器时，`cargo build` 会失败在
`linking with link.exe failed`，而且**第一个卡住的往往不是自己的代码**——
是 `serde_derive`/`quote` 这类构建脚本（它们用 host 工具链编译并链接）。

两条出路，任选其一：

**A. 装 MSVC 生成工具**（推荐；产物最小、无需附带运行时 DLL）

```
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64"
```

**B. 走 MinGW-GNU**（不装 VS 时用；本项目实测走的就是这条）

```powershell
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
```

注意 GNU 路线必须装 **toolchain** 而不是只加 target：只执行
`rustup target add x86_64-pc-windows-gnu` 时，构建脚本仍然由 msvc 的 host
工具链编译，一样会卡在 `link.exe`。

### 项目路径不能含非 ASCII 字符（重要）

MinGW 的 `ld.exe` 解析不了含中文的路径，会报一串：

```
ld.exe: cannot find D:\...\target\debug\deps\xxx.o: No such file or directory
```

而且它打印出来的路径本身也会变成乱码，很容易被误判成别的问题。

**因此本项目必须放在纯 ASCII 路径下**——当前位于
`D:\MineCraft\PluginsSrc\360folder-rs`。

> 历史注记：项目早期位于 `E:\下载\360FileBrowser\360folder-rs`，当时靠
> `CARGO_TARGET_DIR` 把构建目录重定向到 `C:\cargo-target\360folder` 绕过。
> 迁移到 ASCII 路径后该 workaround 已移除，构建产物现在正常落在项目内的 `target\`。
> 若以后要把项目搬回含中文的目录，需要重新加回这层重定向。

### 构建命令

只有一件事写在项目配置里：

- [rust-toolchain.toml](rust-toolchain.toml) —— 固定使用 GNU 工具链（命令行与 RustRover 都会读）

```powershell
# 方式一：脚本（只负责找 cargo 并调用）
.\build.ps1              # release 构建
.\build.ps1 -Debug       # debug 构建
.\build.ps1 -Run         # 构建并运行
.\build.ps1 -Clean       # 清理后构建

# 方式二：直接 cargo
cargo build --release
```

产物：`target\release\360Folder.exe`（就在项目内）。

在 RustRover 里直接 Build / Run 即可——它会读取 `rust-toolchain.toml`，不会再去找 `link.exe`。
（验证方式：在项目目录下执行 `cargo -vV`，`host:` 一行应显示 `x86_64-pc-windows-gnu`。）

### 实测结果

在本机（无 MSVC、MinGW-GNU 工具链）实际编译通过：

| 配置 | 产物大小 |
|---|---|
| `--release` | **661 KB**（676,864 字节） |
| 原版 `360FileBrowser.exe` | 2.4 MB（2,500,352 字节） |

即体积约为原版的 **27%**。

---

## 六、运行行为

首次运行会在 **exe 同目录**下创建 `Profile\`：

```
Profile\
  settings.json      设置
  favoriates.json    书签
  addr_history.ini   地址栏历史
  lasttabs.dat       会话标签
```

若程序目录不可写（例如装在 `Program Files`），自动回退到 `%APPDATA%\360Folder\Profile\`。

### 接管系统文件管理

主菜单 → 「接管系统文件管理」。需要**管理员权限**（写 `HKU\.DEFAULT`）。

无管理员权限时可用 HKCU 版本测试：调用 `shell_integration::take_over_current_user()`。

---

## 七、与原软件的差异（有意为之）

1. **不做自动更新、不做 BHO** —— 这两项是原软件商业分发的组成部分，在自维护项目中没有意义。
2. **不加载原 .ui 主题包** —— 首版直接程序化绘制，避免 ZIP/SVG 解析的启动开销。主题包解析器可作为可选功能加入（接口已按 `Palette` 抽象好）。
3. **不使用命名管道做跨进程状态同步** —— 只保留单实例唤醒这一必要用途。
4. **配置用 JSON 而非私有二进制** —— 便于用户手改与版本控制。

---

## 八、开发状态（诚实声明）

### 已完整实现
- 纯 Win32 无边框窗口 + 全自绘 UI + 双缓冲
- 多标签（新建/关闭/切换/滚轮切换/双击关闭）
- 目录导航（后退/前进/向上/历史）
- 地址栏（面包屑显示 + 编辑模式 + 路径解析含 `~` 和 `%VAR%`）
- 8 种视图模式（真实虚拟化绘制，只画可见项）
- 文件操作（回收站删除、复制、移动、重命名、新建文件夹/文件）
- 图标缓存（按扩展名，不逐文件查询）
- 键盘快捷键（导航、选择、剪切板、视图切换）
- 配置持久化、会话恢复
- 单实例 + 命名管道
- 资源管理器接管（注册表读写）
- 拖放接收

### 已实现但较简化
- **设置对话框**：当前用系统 `MessageBox` 承载摘要；原软件的 `DlgSetting.xml` 多页面板需要自绘控件树，属于后续迭代
- **快速预览**：文本文件（<1 MiB）在对话框中显示前 12 行；原软件的富预览需要图像/文档解码
- **主菜单**：用系统 `TrackPopupMenu` 实现，保证键盘可达性；原软件是自绘菜单
- **搜索**：快速搜索（当前目录名称过滤）已实现；系统搜索需索引 API，网络搜索需 URL 模板配置

### 尚未实现
- 历史记录下拉列表 UI
- 导航窗格 / 预览窗格 / 详细信息窗格的实际绘制
- 引导页
- 更新检测
- 缩略图（需缩略图缓存 API）
- 快捷键自定义 UI（数据层已就绪）
- 排序表头点击切换

### 编译状态

本项目已在编写环境实际编译并链接通过：

- `cargo check` —— 0 错误、0 警告
- `cargo build` / `cargo build --release` —— 产出可执行文件（见「五、构建 → 实测结果」）

初版撰写时曾有两类系统性错误，均已修正，记录在此供后续排查参考：

1. **符号所在模块猜错**。`windows-sys` 的模块划分和 Win32 头文件的直觉不一致，例如
   `InvalidateRect` / `UpdateWindow` / `BeginPaint` 都在 `Win32::Graphics::Gdi`
   （不是 `WindowsAndMessaging`），`ReleaseCapture` 在 `Win32::UI::Input::KeyboardAndMouse`，
   `PIPE_ACCESS_DUPLEX` 在 `Win32::Storage::FileSystem`，`HKEY` 在 `Win32::System::Registry`，
   而 `WM_MOUSELEAVE` 竟然在 `Win32::UI::Controls`——漏掉它会让 `match msg` 里该分支
   退化成变量绑定并吞掉后面所有分支（表现为一连串 `unreachable pattern` 警告）。
2. **同名类型重复定义**。排序键一度在设置层和枚举层各有一份，两处不通用。
   现已统一为 [src/fs/enumerate.rs](src/fs/enumerate.rs) 的单一 `SortKey`，
   由 [src/cfg/settings.rs](src/cfg/settings.rs) 直接 re-export。

若日后升级 `windows-sys` 后再次报导入错误，按编译器提示重新定位即可，
都是机械性修正，不涉及设计问题。

---

## 九、许可

MIT。本项目为独立实现的干净重写，不包含原软件的任何代码或资源。
