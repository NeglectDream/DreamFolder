//! 配置层：设置、书签、地址历史、会话。
//!
//! 存储布局对齐原软件（见 PE 字符串）：
//! ```text
//! <程序目录>/Profile/
//!   favoriates.json        书签（原为 favoriates.json / .ini / .dat）
//!   addr_history.ini       地址栏历史（保持 ini，便于用户手改）
//!   lasttabs.dat           上次会话标签（保持最紧凑的二进制）
//!   ignores.ini            路径忽略规则
//! ```
//! 程序目录不可写时整体回退到 `%APPDATA%\DreamFolder\Profile\`。

pub mod bookmarks;
pub mod history;
pub mod navplaces;
pub mod paths;
pub mod session;
pub mod settings;
pub mod tags;

pub use bookmarks::Bookmarks;
pub use history::AddrHistory;
pub use navplaces::NavPlaces;
pub use session::Session;
pub use settings::Settings;
pub use tags::Tags;

/// 配置根：一次加载，全局共享。
pub struct Config {
    pub settings: Settings,
    pub bookmarks: Bookmarks,
    /// 导航窗格「快捷访问」区（用户可增删排序）。
    pub nav_places: NavPlaces,
    pub history: AddrHistory,
    pub session: Session,
    /// 用户自定义的颜色标签（跨目录组织）。
    pub tags: Tags,
    /// Profile 目录（已确保存在）
    pub root: std::path::PathBuf,
}

impl Config {
    /// 冷启动路径：这里只做**读取**，不写盘、不创建目录（除非需要）。
    pub fn load() -> Config {
        let root = paths::profile_dir();

        let settings = Settings::load(&root);
        let bookmarks = Bookmarks::load(&root);
        // 没有配置文件＝首次启动，播种默认的六个位置；有配置文件就照它来，
        // 哪怕里面是空列表（用户自己清空的）
        let nav_places = NavPlaces::load(&root).unwrap_or_else(NavPlaces::seeded);
        let history = AddrHistory::load(&root);
        let session = Session::load(&root);
        let tags = Tags::load(&root);

        Config {
            settings,
            bookmarks,
            nav_places,
            history,
            session,
            tags,
            root,
        }
    }

    /// 退出时一次性落盘。分开写是为了让失败互不影响。
    pub fn save_all(&self) {
        self.settings.save(&self.root);
        self.bookmarks.save(&self.root);
        self.nav_places.save(&self.root);
        self.history.save(&self.root);
        self.session.save(&self.root);
        self.tags.save(&self.root);
    }
}
