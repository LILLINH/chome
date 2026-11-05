//! Engine dùng chung cho mọi cửa sổ/tab: một WebContext, một NetworkSession,
//! một bộ Settings và một UserContentManager (chứa bộ chặn quảng cáo).
//!
//! Dùng chung thay vì tạo riêng cho từng tab giúp tiết kiệm bộ nhớ đáng kể.

use crate::{adblock, pages, storage::Storage};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use webkit6::{gio, glib, gtk, prelude::*};
use webkit6::{
    CacheModel, HardwareAccelerationPolicy, MemoryPressureSettings, NetworkSession, Settings,
    UserContentFilter, UserContentManager, WebContext,
};

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

const YOUTUBE_URLS: &[&str] = &[
    "https://*.youtube.com/*",
    "https://youtube.com/*",
    "https://*.youtube-nocookie.com/*",
];

/// Công cụ tìm kiếm: (mã, tên hiển thị, URL tìm kiếm — từ khóa nối vào cuối).
pub const SEARCH_ENGINES: &[(&str, &str, &str)] = &[
    ("google", "Google", "https://www.google.com/search?q="),
    ("bing", "Bing", "https://www.bing.com/search?q="),
    ("duckduckgo", "DuckDuckGo", "https://duckduckgo.com/?q="),
    ("coccoc", "Cốc Cốc", "https://coccoc.com/search#query="),
];

/// Cấu hình chỉ đọc từ biến môi trường (dùng khi kiểm thử / tinh chỉnh nâng cao).
pub struct Config {
    pub search_url: Option<String>,
    pub sleep_after: Option<Duration>,
    pub gpu: Option<bool>,
    pub adblock: Option<bool>,
    /// Giới hạn RAM cho mỗi web process (MB).
    pub memory_limit_mb: u32,
    /// Khi RAM trống của máy xuống dưới mức này (MB), cho tab nền ngủ ngay.
    pub low_memory_mb: u64,
    pub cache_model: CacheModel,
}

impl Config {
    fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        let env_u64 = |k: &str, d: u64| var(k).and_then(|v| v.parse().ok()).unwrap_or(d);
        Self {
            search_url: var("CHOME_SEARCH_URL"),
            sleep_after: var("CHOME_SLEEP_SECONDS")
                .and_then(|v| v.parse().ok())
                .map(Duration::from_secs)
                .or_else(|| var("CHOME_SLEEP_MINUTES").and_then(|v| v.parse::<u64>().ok()).map(|m| Duration::from_secs(m * 60))),
            gpu: var("CHOME_GPU").map(|v| v == "1"),
            adblock: var("CHOME_ADBLOCK").map(|v| v != "0"),
            memory_limit_mb: env_u64("CHOME_MEMORY_LIMIT_MB", 4096) as u32,
            low_memory_mb: env_u64("CHOME_LOW_MEMORY_MB", 1500),
            // Cache đầy đủ như Chrome. DocumentViewer (tối thiểu) tắt cả cache trên đĩa,
            // làm YouTube tải lại ~40MB JS mỗi lần và kẹt ở khung xương — chỉ tiết kiệm ~5MB RAM.
            cache_model: match var("CHOME_CACHE").as_deref() {
                Some("viewer") => CacheModel::DocumentViewer,
                Some("document") => CacheModel::DocumentBrowser,
                _ => CacheModel::WebBrowser,
            },
        }
    }
}

/// Cài đặt người dùng (trang chome://settings), lưu trong SQLite.
#[derive(Clone)]
pub struct Prefs {
    pub restore_session: bool,
    pub search: String,
    pub theme: String,
    pub zoom: f64,
    pub adblock: bool,
    pub itp: bool,
    pub sleep_minutes: u64,
    pub gpu: bool,
    /// Cho công cụ AI (MCP) điều khiển trình duyệt — mặc định tắt.
    pub mcp: bool,
    /// "ai" = chỉ tab do AI mở (mặc định), "all" = mọi tab.
    pub mcp_scope: String,
    /// Kiểm tra chính tả khi gõ (gạch đỏ chữ sai).
    pub spell: bool,
    /// Luôn hiện thanh bookmark (Ctrl+Shift+B); tắt thì chỉ hiện ở trang tab mới.
    pub bookmarks_bar: bool,
    /// Đề nghị lưu mật khẩu sau khi đăng nhập.
    pub passwords: bool,
    /// YouTube không dừng: tự xử lý hộp "Tiếp tục xem?".
    pub yt_nonstop: bool,
    /// Tự kiểm tra bản mới trên GitHub mỗi ngày.
    pub update_check: bool,
}

impl Prefs {
    fn load(db: &Storage) -> Self {
        let get = |k: &str| db.get_setting(k);
        Self {
            restore_session: get("startup").is_none_or(|v| v == "restore"),
            search: get("search")
                .filter(|v| SEARCH_ENGINES.iter().any(|(id, _, _)| id == v))
                .unwrap_or_else(|| "google".into()),
            theme: get("theme").filter(|v| ["system", "light", "dark"].contains(&v.as_str())).unwrap_or_else(|| "system".into()),
            zoom: get("zoom").and_then(|v| v.parse().ok()).filter(|z| (0.25..=5.0).contains(z)).unwrap_or(1.0),
            adblock: get("adblock").is_none_or(|v| v == "1"),
            itp: get("itp").is_none_or(|v| v == "1"),
            sleep_minutes: get("sleep").and_then(|v| v.parse().ok()).unwrap_or(5),
            // Mặc định bật GPU: cuộn mượt như Chrome (tắt chỉ tiết kiệm ~20-35MB/trang).
            gpu: get("gpu").is_none_or(|v| v == "1"),
            mcp: get("mcp").is_some_and(|v| v == "1"),
            mcp_scope: get("mcp_scope").filter(|v| v == "all").unwrap_or_else(|| "ai".into()),
            spell: get("spell").is_none_or(|v| v == "1"),
            bookmarks_bar: get("bookmarks_bar").is_some_and(|v| v == "1"),
            passwords: get("passwords").is_none_or(|v| v == "1"),
            yt_nonstop: get("yt_nonstop").is_none_or(|v| v == "1"),
            update_check: get("update_check").is_none_or(|v| v == "1"),
        }
    }
}

pub struct Engine {
    pub context: WebContext,
    pub session: NetworkSession,
    pub settings: Settings,
    pub content: UserContentManager,
    /// Bộ quản lý riêng cho cửa sổ ẩn danh: không có script mật khẩu.
    pub private_content: UserContentManager,
    pub passwords: crate::passwords::PasswordStore,
    pub db: Rc<Storage>,
    pub config: Config,
    pub prefs: RefCell<Prefs>,
    pub data_dir: PathBuf,
    /// Tải xuống (bong bóng trên thanh công cụ + chome://downloads).
    pub downloads: Rc<crate::downloads::Manager>,
    /// Chế độ tối của hệ thống lúc khởi động (để trả lại khi chọn "Theo hệ thống").
    system_dark: Cell<bool>,
    adblock_filter: RefCell<Option<UserContentFilter>>,
    /// Chặn quảng cáo YouTube: script xóa dữ liệu quảng cáo + CSS ẩn khung quảng cáo.
    yt_script: webkit6::UserScript,
    yt_style: webkit6::UserStyleSheet,
    /// YouTube không dừng (tham khảo extension "YouTube NonStop", MIT).
    yt_nonstop: webkit6::UserScript,
    /// Trình xem JSON (cây thu gọn được, tô màu, tìm kiếm) khi mở một địa chỉ trả về JSON.
    json_viewer: webkit6::UserScript,
    json_style: webkit6::UserStyleSheet,
    adblock_on: Cell<bool>,
}

impl Engine {
    pub fn new() -> Rc<Self> {
        let config = Config::from_env();
        let data_dir = glib::user_data_dir().join("chome");
        let cache_dir = glib::user_cache_dir().join("chome");
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);

        // Web process tự dọn cache khi gần chạm giới hạn và bị khởi động lại nếu vượt.
        let mut mem = MemoryPressureSettings::new();
        mem.set_memory_limit(config.memory_limit_mb);
        // Dọn nhẹ (bất đồng bộ) từ ~600MB; dọn mạnh (đồng bộ — trang phải dừng chờ) chỉ từ
        // ~1.4GB: ngưỡng cũ ~820MB làm YouTube nghe lâu bị đứng từng lúc vì bị dọn mạnh liên tục.
        // Thứ tự quan trọng: WebKit yêu cầu conservative < strict ngay lúc đặt.
        mem.set_strict_threshold(env_f64("CHOME_MEM_STRICT", 0.35));
        mem.set_conservative_threshold(env_f64("CHOME_MEM_CONSERVATIVE", 0.15));
        let context = WebContext::builder().memory_pressure_settings(&mem).build();
        context.set_cache_model(config.cache_model);

        let session = NetworkSession::new(
            Some(&data_dir.join("web").to_string_lossy()),
            Some(&cache_dir.join("web").to_string_lossy()),
        );
        session.set_persistent_credential_storage_enabled(true);
        // Lưu cookie (đăng nhập) xuống đĩa. Không đặt dòng này thì WebKitGTK chỉ giữ cookie
        // trong RAM → đóng trình duyệt là mất hết đăng nhập.
        if let Some(cookies) = session.cookie_manager() {
            cookies.set_persistent_storage(
                &data_dir.join("web").join("cookies.sqlite").to_string_lossy(),
                webkit6::CookiePersistentStorage::Sqlite,
            );
        }
        if let Some(wdm) = session.website_data_manager() {
            wdm.set_favicons_enabled(true);
        }

        let settings = Settings::new();
        settings.set_enable_page_cache(false); // không giữ trang cũ trong RAM khi back/forward
        settings.set_enable_smooth_scrolling(true);
        settings.set_enable_developer_extras(true);
        settings.set_javascript_can_open_windows_automatically(false); // chặn popup tự bật
        // Không tự phát video khi chưa bấm (CHOME_AUTOPLAY=1 chỉ dùng khi đo đạc).
        settings.set_media_playback_requires_user_gesture(std::env::var_os("CHOME_AUTOPLAY").is_none());
        settings.set_user_agent_with_application_details(Some("Chrome"), Some("130.0.0.0"));

        let db = Rc::new(Storage::open(&data_dir.join("browser.sqlite")).unwrap_or_else(|e| {
            eprintln!("[storage] không mở được CSDL ({e}), dùng bộ nhớ tạm");
            Storage::in_memory()
        }));

        let engine = Rc::new(Self {
            context,
            session,
            settings,
            content: UserContentManager::new(),
            private_content: UserContentManager::new(),
            passwords: crate::passwords::PasswordStore::new(db.clone()),
            downloads: crate::downloads::Manager::new(db.clone()),
            prefs: RefCell::new(Prefs::load(&db)),
            db,
            config,
            data_dir,
            adblock_filter: RefCell::new(None),
            yt_script: webkit6::UserScript::new(
                include_str!("youtube_adblock.js"),
                webkit6::UserContentInjectedFrames::AllFrames,
                webkit6::UserScriptInjectionTime::Start,
                YOUTUBE_URLS,
                &[],
            ),
            yt_style: webkit6::UserStyleSheet::new(
                include_str!("youtube_adblock.css"),
                webkit6::UserContentInjectedFrames::AllFrames,
                webkit6::UserStyleLevel::User,
                YOUTUBE_URLS,
                &[],
            ),
            yt_nonstop: webkit6::UserScript::new(
                include_str!("youtube_nonstop.js"),
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::End,
                &["https://www.youtube.com/*", "https://music.youtube.com/*"],
                &[],
            ),
            json_viewer: webkit6::UserScript::new(
                include_str!("json_viewer.js"),
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserScriptInjectionTime::End,
                &[],
                &[],
            ),
            json_style: webkit6::UserStyleSheet::new(
                include_str!("json_viewer.css"),
                webkit6::UserContentInjectedFrames::TopFrame,
                webkit6::UserStyleLevel::Author,
                &[],
                &[],
            ),
            adblock_on: Cell::new(true),
            system_dark: Cell::new(
                gtk::Settings::default().is_some_and(|s| s.is_gtk_application_prefer_dark_theme()),
            ),
        });
        {
            let p = engine.prefs.borrow().clone();
            engine.content.add_script(&engine.json_viewer);
            engine.content.add_style_sheet(&engine.json_style);
            engine.private_content.add_script(&engine.json_viewer);
            engine.private_content.add_style_sheet(&engine.json_style);
            let on = engine.config.adblock.unwrap_or(p.adblock);
            engine.adblock_on.set(on);
            if on {
                engine.content.add_script(&engine.yt_script);
                engine.content.add_style_sheet(&engine.yt_style);
                engine.private_content.add_script(&engine.yt_script);
                engine.private_content.add_style_sheet(&engine.yt_style);
            }
            engine.session.set_itp_enabled(p.itp); // chống tracking xuyên trang
            engine.apply_gpu(engine.config.gpu.unwrap_or(p.gpu));
            engine.apply_theme(&p.theme);
            engine.apply_spell(p.spell);
            engine.apply_yt_nonstop(p.yt_nonstop);
        }
        engine.setup_password_query();
        engine.downloads.attach(&engine.session, false);
        engine.restore_session_cookies();
        engine.register_internal_scheme();
        engine.load_adblock();
        engine
    }

    fn register_internal_scheme(self: &Rc<Self>) {
        if let Some(sec) = self.context.security_manager() {
            // "local": trang web bên ngoài không thể điều hướng/nhúng chome://
            sec.register_uri_scheme_as_local("chome");
            sec.register_uri_scheme_as_secure("chome");
        }
        let weak = Rc::downgrade(self);
        self.context.register_uri_scheme("chome", move |req| {
            let Some(engine) = weak.upgrade() else { return };
            let uri = req.uri().map(|u| u.to_string()).unwrap_or_default();
            let incognito = req.web_view().and_then(|v| v.network_session()).is_some_and(|s| s.is_ephemeral());
            let html = pages::handle(&engine, &uri, incognito);
            let bytes = glib::Bytes::from_owned(html.into_bytes());
            let len = bytes.len() as i64;
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            req.finish(&stream, len, Some("text/html"));
        });
    }

    fn load_adblock(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let store_dir = glib::user_cache_dir().join("chome").join("adblock");
        let custom = self.data_dir.join("adblock.json");
        adblock::load(&store_dir, &custom, move |filter| {
            let Some(engine) = weak.upgrade() else { return };
            if engine.adblock_on.get() {
                engine.content.add_filter(&filter);
                engine.private_content.add_filter(&filter);
            }
            *engine.adblock_filter.borrow_mut() = Some(filter);
        });
    }

    pub fn adblock_enabled(&self) -> bool {
        self.adblock_on.get()
    }

    pub fn set_adblock(&self, on: bool) {
        self.adblock_on.set(on);
        self.prefs.borrow_mut().adblock = on;
        self.db.set_setting("adblock", if on { "1" } else { "0" });
        // Gỡ trước rồi mới thêm để không bao giờ nhúng trùng hai lần.
        self.content.remove_script(&self.yt_script);
        self.content.remove_style_sheet(&self.yt_style);
        self.private_content.remove_script(&self.yt_script);
        self.private_content.remove_style_sheet(&self.yt_style);
        if on {
            self.content.add_script(&self.yt_script);
            self.content.add_style_sheet(&self.yt_style);
            self.private_content.add_script(&self.yt_script);
            self.private_content.add_style_sheet(&self.yt_style);
        }
        if let Some(f) = self.adblock_filter.borrow().as_ref() {
            if on {
                self.content.add_filter(f);
                self.private_content.add_filter(f);
            } else {
                self.content.remove_filter(f);
                self.private_content.remove_filter(f);
            }
        }
    }

    pub fn search_url(&self) -> String {
        if let Some(u) = &self.config.search_url {
            return u.clone();
        }
        let id = self.prefs.borrow().search.clone();
        SEARCH_ENGINES.iter().find(|(i, _, _)| *i == id).map(|e| e.2).unwrap_or(SEARCH_ENGINES[0].2).into()
    }

    pub fn search_name(&self) -> &'static str {
        let id = self.prefs.borrow().search.clone();
        SEARCH_ENGINES.iter().find(|(i, _, _)| *i == id).map(|e| e.1).unwrap_or("Google")
    }

    /// Thời gian không dùng trước khi tab nền ngủ (0 = tắt).
    pub fn sleep_after(&self) -> Duration {
        self.config
            .sleep_after
            .unwrap_or_else(|| Duration::from_secs(self.prefs.borrow().sleep_minutes * 60))
    }

    pub fn zoom(&self) -> f64 {
        self.prefs.borrow().zoom
    }

    fn apply_gpu(&self, on: bool) {
        self.settings.set_hardware_acceleration_policy(if on {
            HardwareAccelerationPolicy::Always
        } else {
            HardwareAccelerationPolicy::Never // tiết kiệm RAM nhưng cuộn kém mượt
        });
    }

    /// Kiểm tra chính tả tiếng Việt (nếu đã cài hunspell-vi) và tiếng Anh.
    fn apply_spell(&self, on: bool) {
        let mut langs = vec![];
        for (code, dict) in [("vi_VN", "vi_VN"), ("en_US", "en_US")] {
            let found = ["/usr/share/hunspell", "/usr/share/myspell", "/usr/share/myspell/dicts"]
                .iter()
                .any(|d| std::path::Path::new(d).join(format!("{dict}.dic")).exists());
            if found {
                langs.push(code);
            }
        }
        if langs.is_empty() {
            langs.push("en_US"); // aspell/enchant vẫn có thể cung cấp
        }
        self.context.set_spell_checking_languages(&langs);
        self.context.set_spell_checking_enabled(on);
    }

    fn apply_yt_nonstop(&self, on: bool) {
        for content in [&self.content, &self.private_content] {
            content.remove_script(&self.yt_nonstop); // gỡ trước để không nhúng trùng
            if on {
                content.add_script(&self.yt_nonstop);
            }
        }
    }

    pub fn has_vietnamese_dictionary(&self) -> bool {
        ["/usr/share/hunspell/vi_VN.dic", "/usr/share/myspell/vi_VN.dic"].iter().any(|p| std::path::Path::new(p).exists())
    }

    fn apply_theme(&self, theme: &str) {
        if let Some(s) = gtk::Settings::default() {
            s.set_gtk_application_prefer_dark_theme(match theme {
                "dark" => true,
                "light" => false,
                _ => self.system_dark.get(),
            });
        }
    }

    /// Đổi một cài đặt từ trang chome://settings: lưu lại và áp dụng ngay.
    pub fn apply_setting(&self, key: &str, value: &str) {
        let on = value == "1";
        let mut p = self.prefs.borrow_mut();
        match key {
            "startup" => p.restore_session = value == "restore",
            "search" if SEARCH_ENGINES.iter().any(|(id, _, _)| *id == value) => p.search = value.into(),
            "theme" if ["system", "light", "dark"].contains(&value) => {
                p.theme = value.into();
                self.apply_theme(value);
            }
            "zoom" => match value.parse::<f64>() {
                Ok(z) if (0.25..=5.0).contains(&z) => p.zoom = z,
                _ => return,
            },
            "adblock" => {
                drop(p);
                self.set_adblock(on);
                return;
            }
            "itp" => {
                p.itp = on;
                self.session.set_itp_enabled(on);
            }
            "sleep" => match value.parse::<u64>() {
                Ok(m) => p.sleep_minutes = m,
                _ => return,
            },
            "gpu" => {
                p.gpu = on;
                self.apply_gpu(on);
            }
            "mcp" => {
                p.mcp = on;
                crate::mcp::set_enabled(on);
            }
            "mcp_scope" if value == "ai" || value == "all" => p.mcp_scope = value.into(),
            "bookmarks_bar" => p.bookmarks_bar = on,
            "update_check" => p.update_check = on,
            "yt_nonstop" => {
                p.yt_nonstop = on;
                self.apply_yt_nonstop(on);
            }
            "spell" => {
                p.spell = on;
                self.apply_spell(on);
            }
            "passwords" => p.passwords = on,
            _ => return,
        }
        self.db.set_setting(key, value);
    }

    /// Trả lời yêu cầu tự điền từ script mật khẩu (chạy trong thế giới JS riêng của Chỏme).
    fn setup_password_query(self: &Rc<Self>) {
        use crate::passwords;
        // Script chạy trong thế giới JS riêng (trang web không thấy, không gọi được handler).
        // Chỉ nạp vào bộ thường — cửa sổ ẩn danh dùng private_content, không có script này.
        self.content.add_script(&webkit6::UserScript::for_world(
            include_str!("passwords.js"),
            webkit6::UserContentInjectedFrames::AllFrames,
            webkit6::UserScriptInjectionTime::End,
            passwords::WORLD,
            &[],
            &[],
        ));
        self.content.register_script_message_handler(passwords::SAVE_HANDLER, Some(passwords::WORLD));
        self.content.register_script_message_handler_with_reply(passwords::QUERY_HANDLER, Some(passwords::WORLD));
        let weak = Rc::downgrade(self);
        self.content.connect_script_message_with_reply_received(Some(passwords::QUERY_HANDLER), move |_, value, reply| {
            let Some(engine) = weak.upgrade() else { return false };
            let Some(json) = passwords::message_json(value) else {
                reply.return_error_message("Thông điệp mật khẩu không hợp lệ");
                return true;
            };
            let Some(passwords::Message::Query { origin }) = passwords::parse_message(&json) else {
                reply.return_error_message("Yêu cầu mật khẩu không hợp lệ");
                return true;
            };
            let result = if engine.prefs.borrow().passwords && engine.passwords.available() && origin.starts_with("http") {
                engine.passwords.reply_json(&origin)
            } else {
                "{\"found\":false}".into()
            };
            let Some(context) = value.context() else {
                reply.return_error_message("Không tạo được ngữ cảnh JavaScript");
                return true;
            };
            reply.return_value(&webkit6::javascriptcore::Value::from_json(&context, &result));
            true
        });
    }

    fn session_cookie_file(&self) -> PathBuf {
        self.data_dir.join("session-cookies.json")
    }

    /// Lưu cookie phiên (không có hạn dùng — thường là cookie đăng nhập) khi thoát, để
    /// "Tiếp tục từ chỗ đã dừng" giữ được đăng nhập như Chrome. File chỉ chủ tài khoản đọc được.
    pub async fn save_session_cookies(&self) {
        let Some(manager) = self.session.cookie_manager() else { return };
        let Ok(cookies) = manager.all_cookies_future().await else { return };
        let list: Vec<serde_json::Value> = cookies
            .into_iter()
            .filter_map(|mut c| {
                if c.expires().is_some() {
                    return None; // cookie có hạn dùng đã nằm trong cookies.sqlite
                }
                let same_site = match c.same_site_policy() {
                    webkit6::soup::SameSitePolicy::Strict => "strict",
                    webkit6::soup::SameSitePolicy::Lax => "lax",
                    _ => "none",
                };
                Some(serde_json::json!({
                    "name": c.name()?.to_string(), "value": c.value()?.to_string(),
                    "domain": c.domain()?.to_string(), "path": c.path().map(|p| p.to_string()).unwrap_or_else(|| "/".into()),
                    "secure": c.is_secure(), "http_only": c.is_http_only(), "same_site": same_site,
                }))
            })
            .collect();
        let path = self.session_cookie_file();
        if std::fs::write(&path, serde_json::Value::Array(list).to_string()).is_ok() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }

    /// Nạp lại cookie phiên đã lưu (chỉ khi bật "Tiếp tục từ chỗ đã dừng").
    fn restore_session_cookies(&self) {
        let path = self.session_cookie_file();
        if !self.prefs.borrow().restore_session {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let (Some(manager), Ok(text)) = (self.session.cookie_manager(), std::fs::read_to_string(&path)) else { return };
        let Ok(serde_json::Value::Array(list)) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        for c in list {
            let get = |k: &str| c[k].as_str().unwrap_or_default().to_string();
            let mut cookie = webkit6::soup::Cookie::new(&get("name"), &get("value"), &get("domain"), &get("path"), -1);
            cookie.set_secure(c["secure"].as_bool().unwrap_or(false));
            cookie.set_http_only(c["http_only"].as_bool().unwrap_or(false));
            cookie.set_same_site_policy(match c["same_site"].as_str() {
                Some("strict") => webkit6::soup::SameSitePolicy::Strict,
                Some("lax") => webkit6::soup::SameSitePolicy::Lax,
                _ => webkit6::soup::SameSitePolicy::None,
            });
            manager.add_cookie(&cookie, None::<&gio::Cancellable>, |_| {});
        }
    }

    /// Xóa toàn bộ dữ liệu duyệt web: lịch sử, cookie, cache, bộ nhớ trang.
    pub fn clear_browsing_data(&self) {
        self.db.clear_history();
        let _ = std::fs::remove_file(self.session_cookie_file());
        if let Some(wdm) = self.session.website_data_manager() {
            wdm.clear(webkit6::WebsiteDataTypes::ALL, glib::TimeSpan::from_seconds(0), None::<&gio::Cancellable>, |_| {});
        }
    }

    /// Biến nội dung gõ vào thanh địa chỉ thành URL.
    pub fn resolve_input(&self, input: &str) -> String {
        let s = input.trim();
        if s.is_empty() {
            return pages::NEWTAB.into();
        }
        let has_scheme = ["http://", "https://", "file://", "chome://", "about:", "data:", "view-source:"]
            .iter()
            .any(|p| s.to_ascii_lowercase().starts_with(p));
        if has_scheme {
            return s.into();
        }
        if s.starts_with('/') {
            return format!("file://{s}");
        }
        let looks_like_host = !s.contains(char::is_whitespace)
            && (s.starts_with("localhost")
                || s.split(['/', '?', '#']).next().is_some_and(|h| {
                    let h = h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h);
                    h.contains('.') && !h.starts_with('.') && !h.ends_with('.')
                }));
        if looks_like_host {
            let local = s.starts_with("localhost") || s.starts_with("127.") || s.starts_with("192.168.");
            return format!("{}://{s}", if local { "http" } else { "https" });
        }
        format!(
            "{}{}",
            self.search_url(),
            glib::Uri::escape_string(s, None, false)
        )
    }
}
