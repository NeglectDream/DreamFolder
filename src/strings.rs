//! 界面文本。
//!
//! **全部取自原软件的 .ui 主题包 XML**，逐条对应，方便与原版本对照。
//! 集中定义的好处：将来做多语言只需替换这一张表。

#![allow(dead_code)]

// ---------------------------------------------------------------- 应用
pub const APP_NAME: &str = "360文件夹";

// ---------------------------------------------------------------- 主菜单
// 对应 main_menu.xml
pub const MENU_ADD_DESKTOP_SHORTCUT: &str = "添加桌面图标";
pub const MENU_TAKE_OVER_EXPLORER: &str = "接管系统文件管理";
pub const MENU_FEEDBACK: &str = "反馈";
pub const MENU_CHECK_UPDATE: &str = "检测更新";
pub const MENU_SETTINGS: &str = "设置";
pub const MENU_EXIT: &str = "退出";

// ---------------------------------------------------------------- 标签菜单
// 对应 tab_menu.xml
pub const TAB_NEW: &str = "新建标签";
pub const TAB_DUPLICATE: &str = "复制当前标签";
pub const TAB_OPEN_NEW_WINDOW: &str = "在新窗口打开";
pub const TAB_ADD_TO_BOOKMARK: &str = "添加到书签栏";
pub const TAB_CLOSE: &str = "关闭当前标签";
pub const TAB_CLOSE_OTHERS: &str = "关闭其它标签";

// ---------------------------------------------------------------- 显示菜单
// 对应 option_menu.xml
pub const OPT_NAV_PANE: &str = "导航窗格";
pub const OPT_PREVIEW_PANE: &str = "预览窗格";
pub const OPT_DETAIL_PANE: &str = "详细信息窗格";
pub const OPT_FILE_EXTENSION: &str = "文件扩展名";
pub const OPT_HIDDEN_ITEMS: &str = "隐藏的项目";

// ---------------------------------------------------------------- 视图菜单
// 对应 view_menu.xml
pub const VIEW_GROUP_DISPLAY: &str = "显示";

// ---------------------------------------------------------------- 收藏栏
// 对应 explorer_frame.xml 与 favoriate_menu.xml
pub const FAVORITES_PLACEHOLDER: &str = "可拖动文档、软件、文件夹到此，点击快速打开";
pub const FAV_OPEN: &str = "打开";
pub const FAV_OPEN_LOCATION: &str = "打开所在位置";
pub const FAV_PIN: &str = "设为置顶";
pub const FAV_RENAME: &str = "重命名";
pub const FAV_DELETE: &str = "删除书签";
pub const FAV_ADD_FOLDER: &str = "添加书签目录";

// ---------------------------------------------------------------- 工具栏
// 对应 explorer_frame.xml 的 ToolContainer 与 tool_*.svg 图标名
pub const TOOL_NEW_TAB: &str = "新建标签页";
pub const TOOL_QUICK_SEARCH: &str = "快速搜索";
pub const TOOL_SYSTEM_SEARCH: &str = "系统搜索";
pub const TOOL_WEB_SEARCH: &str = "网络搜索";
pub const TOOL_ARROW: &str = "更多";

// ---------------------------------------------------------------- 设置对话框
// 对应 DlgSetting.xml
pub const SET_TITLE: &str = "设置";
pub const SET_CATEGORY_COMMON: &str = "基础设置";
pub const SET_CATEGORY_HOTKEY: &str = "快捷键";

pub const SET_GROUP_STARTUP: &str = "启动设置";
pub const SET_REPLACE_EXPLORER: &str = "接管系统文件夹操作（双面板多标签，操作更快捷）";
pub const SET_QUICK_PREVIEW: &str = "开启快速预览（通过敲击空格键来快速预览文件内容）";

pub const SET_GROUP_START_WHEN: &str = "启动时";
pub const SET_START_NEWTAB: &str = "打开新标签页";
pub const SET_START_RESTORE: &str = "恢复上次打开的标签页（启动加载速度可能变慢）";
pub const SET_START_CUSTOM: &str = "打开一个或多个指定标签页";
pub const SET_CUSTOM_FOLDER: &str = "指定文件夹";
pub const SET_ADD_FOLDER: &str = "新增文件夹";

pub const SET_GROUP_ON_CLOSE: &str = "关闭窗口时";
pub const SET_CLOSE_ALERT: &str = "关闭窗口时，如果有多个标签则提示";

pub const SET_GROUP_TOOLBAR: &str = "工具栏设置";
pub const SET_TOOLBAR_VISIBLE: &str = "显示工具栏";
pub const SET_BOOKMARK_VISIBLE: &str = "显示收藏书签栏";
pub const SET_BOOKMARK_NEWTAB: &str = "在新标签页打开收藏书签栏的文件";

pub const SET_GROUP_SHORTCUTS: &str = "快捷操作";
pub const SET_DBCLICK_PARENT: &str = "双击文件夹空区域跳转上层目录";
pub const SET_CTRL_DBCLICK_RIGHT: &str = "Ctrl+双击文件夹，右侧创建新标签页打开";
pub const SET_DBCLICK_CLOSE_TAB: &str = "双击标签页关闭当前标签页面";
pub const SET_WHEEL_SWITCHTAB: &str = "标签栏上滚动鼠标切换标签页";

pub const SET_GROUP_HOTKEY: &str = "快捷键";
pub const SET_DISABLE_ALL_HOTKEYS: &str = "禁用所有快捷键";
pub const SET_RESTORE_DEFAULT_HOTKEYS: &str = "恢复默认快捷键";
pub const SET_HOTKEY_COL_ACTION: &str = "动作";
pub const SET_HOTKEY_COL_KEY: &str = "快捷键";

// ---------------------------------------------------------------- 通用对话框
// 对应 AlertWnd.xml / CloseAlert.xml / RenameAlert.xml / AddFavoriteFolderWnd.xml
pub const BTN_OK: &str = "确定";
pub const BTN_CANCEL: &str = "取消";
pub const BTN_SAVE: &str = "保存";
pub const BTN_KNOW: &str = "我知道了";
pub const BTN_CLOSE_ALL: &str = "全部关闭";
pub const BTN_NO_MORE_PROMPT: &str = "不再提示";
pub const BTN_RETRY_LATER: &str = "稍后重试";

pub const ALERT_TITLE: &str = "360文件夹";
pub const ALERT_CLOSE_TITLE: &str = "关闭提示";
pub const ALERT_CLOSE_BODY: &str = "当前窗口已打开多个标签页面，您确定要全部关闭吗？";
pub const ALERT_EMPTY_NAME: &str = "名字不能为空";

pub const RENAME_BOOKMARK_TITLE: &str = "修改书签";
pub const LABEL_NAME: &str = "名称";
pub const LABEL_LOCATION: &str = "位置";
pub const ADD_BOOKMARK_FOLDER_TITLE: &str = "新建书签目录";

// ---------------------------------------------------------------- 异常修复
// 对应 RepairBhoWnd.xml
pub const REPAIR_TITLE: &str = "异常提示";
pub const REPAIR_NOW: &str = "立即修复";
pub const REPAIR_LATER: &str = "暂不修复";

// ---------------------------------------------------------------- 更新
// 对应 CheckUpateWnd.xml / ExistUpateWnd.xml
pub const UPDATE_CHECKING: &str = "版本检测中...";
pub const UPDATE_LATEST: &str = "您使用的已经是最新版";
pub const UPDATE_NEW_VERSION: &str = "新版本特性";
pub const UPDATE_UPGRADE: &str = "升级新版";
pub const UPDATE_DOWNLOADING: &str = "新版本下载中";
pub const UPDATE_DOWNLOAD_FAILED: &str = "下载失败，请检查网络";

// ---------------------------------------------------------------- 引导页
// 对应 guide_wnd.xml
pub const GUIDE_PAGES: [&str; 4] = [
    "已为您升级为多标签的文件管理器“360文件夹”，更快速的切换文件夹，告别凌乱的窗口；若不需要可在菜单中恢复。",
    "点击“+”号新建标签页，还能点击右侧管理直接预览标签哦～",
    "可拖动文档、文件夹、软件到此，添加书签快速直达",
    "极速搜索、文件/网页一键直达",
];

// ---------------------------------------------------------------- 状态栏与提示
pub const STATUS_ITEMS: &str = "个项目";
pub const STATUS_ITEMS_SELECTED: &str = "个项目被选中";
pub const STATUS_LOADING: &str = "正在加载...";
pub const STATUS_NO_ACCESS: &str = "无法访问该位置";
pub const STATUS_EMPTY_FOLDER: &str = "此文件夹为空";
pub const STATUS_DRIVES: &str = "此电脑";

// ---------------------------------------------------------------- 托盘
// 对应 tray_menu.xml
pub const TRAY_SHOW: &str = "显示主界面";
pub const TRAY_SETTINGS: &str = "设置";
pub const TRAY_EXIT: &str = "退出";
