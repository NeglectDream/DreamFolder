//! 界面文本。
//!
//! **全部取自原软件的 .ui 主题包 XML**，逐条对应，方便与原版本对照。
//! 集中定义的好处：将来做多语言只需替换这一张表。

#![allow(dead_code)]

// ---------------------------------------------------------------- 应用
pub const APP_NAME: &str = "DreamFolder";

// ---------------------------------------------------------------- 主菜单
// 对应 main_menu.xml
pub const MENU_ADD_DESKTOP_SHORTCUT: &str = "添加桌面图标";
pub const MENU_TAKE_OVER_EXPLORER: &str = "接管系统文件管理";
/// 已接管时菜单项的文案：点它会进入「恢复」流程。
pub const MENU_RESTORE_EXPLORER: &str = "已接管（点击可恢复）";
pub const MENU_FEEDBACK: &str = "反馈";
pub const MENU_ABOUT: &str = "关于";
pub const MENU_CHECK_UPDATE: &str = "检测更新";
pub const MENU_SETTINGS: &str = "设置";
pub const MENU_EXIT: &str = "退出";

/// 代码仓库：菜单「反馈」与「关于」都指向这里。
pub const FEEDBACK_URL: &str = "https://github.com/NeglectDream/DreamFolder";

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
pub const FAVORITES_PLACEHOLDER: &str = "拖动文件、文件夹到此收藏";
pub const FAV_OPEN: &str = "打开";
pub const FAV_OPEN_LOCATION: &str = "打开所在位置";
pub const FAV_PIN: &str = "设为置顶";
pub const FAV_RENAME: &str = "重命名";
pub const FAV_DELETE: &str = "删除书签";
pub const FAV_ADD_FOLDER: &str = "添加书签目录";
/// 书签目录里一条都没有时，展开出来的菜单里那句提示。
pub const FAV_EMPTY_FOLDER: &str = "（暂无书签）";

// ---------------------------------------------------------------- 工具栏
// 对应 explorer_frame.xml 的 ToolContainer 与 tool_*.svg 图标名
pub const TOOL_NEW_TAB: &str = "新建标签";
pub const TOOL_QUICK_SEARCH: &str = "快速搜索";
pub const TOOL_SYSTEM_SEARCH: &str = "系统搜索";
pub const TOOL_WEB_SEARCH: &str = "网络搜索";
// 搜索框左侧的模式标签只有 40px 宽，四字放不下，用两字。
pub const SEARCH_MODE_QUICK: &str = "快速";
pub const SEARCH_MODE_SYSTEM: &str = "系统";
pub const SEARCH_MODE_WEB: &str = "网络";
/// 搜索框空着时的占位提示。资源管理器只写「搜索」。
pub const SEARCH_HINT: &str = "搜索";
pub const TOOL_ARROW: &str = "更多";

// ---------------------------------------------------------------- 设置对话框
// 对应 DlgSetting.xml
pub const SET_TITLE: &str = "设置";
pub const SET_CATEGORY_COMMON: &str = "基础设置";
pub const SET_CATEGORY_HOTKEY: &str = "快捷键";

pub const SET_GROUP_STARTUP: &str = "启动设置";
pub const SET_REPLACE_EXPLORER: &str = "接管系统文件夹操作";
pub const SET_QUICK_PREVIEW: &str = "快速预览（空格键）";

pub const SET_GROUP_START_WHEN: &str = "启动时";
pub const SET_START_NEWTAB: &str = "打开新标签";
pub const SET_START_RESTORE: &str = "恢复上次打开的标签";
pub const SET_START_CUSTOM: &str = "打开指定文件夹";
pub const SET_CUSTOM_FOLDER: &str = "指定文件夹";
pub const SET_ADD_FOLDER: &str = "新增文件夹";

pub const SET_GROUP_ON_CLOSE: &str = "关闭窗口时";
pub const SET_CLOSE_ALERT: &str = "有多个标签则提示";

pub const SET_GROUP_TOOLBAR: &str = "工具栏设置";
pub const SET_TOOLBAR_VISIBLE: &str = "显示工具栏";
pub const SET_BOOKMARK_VISIBLE: &str = "显示收藏栏";
pub const SET_BOOKMARK_NEWTAB: &str = "书签在新标签打开";

pub const SET_GROUP_SHORTCUTS: &str = "快捷操作";
pub const SET_DBCLICK_PARENT: &str = "双击空白处跳转上层目录";
pub const SET_CTRL_DBCLICK_RIGHT: &str = "Ctrl+双击文件夹在新标签打开";
pub const SET_DBCLICK_CLOSE_TAB: &str = "双击关闭标签";
pub const SET_WHEEL_SWITCHTAB: &str = "标签栏上滚动切换标签";

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

pub const ALERT_TITLE: &str = "DreamFolder";
pub const ALERT_CLOSE_TITLE: &str = "关闭提示";
pub const ALERT_CLOSE_BODY: &str = "有多个标签，确定全部关闭？";
pub const ALERT_EMPTY_NAME: &str = "名字不能为空";
pub const RENAME_BOOKMARK_TITLE: &str = "修改书签";
pub const LABEL_NAME: &str = "名称";
pub const LABEL_LOCATION: &str = "位置";
pub const ADD_BOOKMARK_FOLDER_TITLE: &str = "新建书签目录";

// ---------------------------------------------------------------- 类型名
// 显示在详情窗格、详细信息窗格与列表视图的「类型」列上，
// 同时是「按类型排序」的比较键——因此必须是用户眼里的那串文字。
pub const TYPE_FOLDER: &str = "文件夹";
pub const TYPE_FILE: &str = "文件";

// ---------------------------------------------------------------- 状态栏与提示
pub const STATUS_ITEMS: &str = "个项目";
pub const STATUS_ITEMS_SELECTED: &str = "个项目被选中";
pub const STATUS_LOADING: &str = "正在加载...";
pub const STATUS_NO_ACCESS: &str = "无法访问该位置";
pub const STATUS_EMPTY_FOLDER: &str = "此文件夹为空";
pub const STATUS_DRIVES: &str = "此电脑";
/// 搜索正在跑。和「搜完没结果」必须分开：只有后者才说明文件真的不存在。
pub const SEARCH_RUNNING: &str = "正在搜索…";
/// 搜完了，但一条都没命中。
pub const SEARCH_NO_RESULT: &str = "没有找到匹配项";
/// 搜索命中的条目数后缀（和普通列表的「N 个项目」区分开）。
pub const SEARCH_HITS: &str = "个匹配项";
/// 有搜索词但还没按回车。
pub const SEARCH_PRESS_ENTER: &str = "按回车搜索";
/// 等太久了。索引服务多半没响应，后台结果真回来时仍会替换成结果。
pub const SEARCH_TIMEOUT: &str = "搜索超时，可缩小范围重试";

// ---------------------------------------------------------------- 导航窗格
// 与资源管理器的树结构对齐：主文件夹 → 快速访问区 → 此电脑 → 网络
pub const NAV_HOME: &str = "主文件夹";
pub const NAV_DESKTOP: &str = "桌面";
pub const NAV_DOWNLOADS: &str = "下载";
pub const NAV_DOCUMENTS: &str = "文档";
pub const NAV_PICTURES: &str = "图片";
pub const NAV_MUSIC: &str = "音乐";
pub const NAV_VIDEOS: &str = "视频";
pub const NAV_NETWORK: &str = "网络";
/// 回收站：跟在「主文件夹」下面，和资源管理器一致。
pub const NAV_RECYCLE: &str = "回收站";
/// 回收站条目右键菜单：还原到删除前的位置。
pub const RECYCLE_RESTORE: &str = "还原";
/// 回收站条目右键菜单：彻底删除（不弹确认，与资源管理器一致）。
pub const RECYCLE_PURGE: &str = "彻底删除";
/// 快捷访问区空着时的提示（那一块是用户自己拖出来的）。
pub const NAV_QUICK_EMPTY: &str = "拖动到此收藏";
/// 快捷访问项的右键菜单：只做移除，不动磁盘上的东西。
pub const NAV_QUICK_REMOVE: &str = "从快捷访问移除";
/// 快捷访问项的右键菜单：在当前标签打开。
pub const NAV_QUICK_OPEN: &str = "打开";
/// 快捷访问项的右键菜单：新标签打开。
pub const NAV_QUICK_OPEN_NEWTAB: &str = "在新标签页打开";
/// 快捷访问项的右键菜单：置顶。
pub const NAV_QUICK_PIN: &str = "设为置顶";
/// 快捷访问项的右键菜单：重命名。
pub const NAV_QUICK_RENAME: &str = "重命名";
/// 「此电脑」下的分组标题，与资源管理器一致。
pub const COMPUTER_GROUP_DRIVES: &str = "设备和驱动器";

// ---------------------------------------------------------------- 标签
/// 导航窗格里标签分组的空提示。
pub const NAV_TAGS_EMPTY: &str = "暂无标签";
/// 新建标签对话框标题。
pub const TAG_NEW_TITLE: &str = "新建标签";
pub const TAG_RENAME_TITLE: &str = "重命名标签";
/// 标签名输入框的说明。
pub const TAG_NAME_LABEL: &str = "标签名称";
/// 新建标签时名字空着的提示。
pub const TAG_EMPTY_NAME: &str = "标签名称不能为空";
/// 同名标签已存在。
pub const TAG_DUPLICATE: &str = "已有同名标签";
/// 详情窗格的标签模块标题。
pub const DETAIL_TAGS: &str = "标签";
/// 详情窗格那一行没有归属任何标签时的提示。
pub const TAG_NONE: &str = "暂无标签";
/// 标签标题行的右键菜单项。
pub const TAG_MENU_REMOVE: &str = "删除标签";
pub const TAG_MENU_RENAME: &str = "重命名标签";
/// 标签条目的右键菜单项。
pub const TAG_ITEM_REMOVE: &str = "从标签移除";
/// 删除标签的确认文案。
pub const TAG_REMOVE_CONFIRM: &str = "确定删除？标签内条目不受影响";
