//! Cửa sổ trình duyệt: thanh tab, thanh địa chỉ, quản lý tab và cơ chế "ngủ tab".
//!
//! Tab ngủ = WebView bị hủy hoàn toàn (giải phóng RAM của trang), chỉ giữ lại
//! URL, tiêu đề và lịch sử back/forward đã tuần tự hóa. Khi chọn lại tab, WebView
//! được tạo lại và khôi phục đúng trạng thái điều hướng.

use crate::engine::Engine;
use crate::pages::{self, NEWTAB};
use crate::passwords::{self, Credential, Message};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;
use webkit6::{gdk, gio, glib, gtk, prelude::*};
use webkit6::{
    LoadEvent, NavigationPolicyDecision, NavigationType, PolicyDecisionType, ResponsePolicyDecision,
    WebProcessTerminationReason, WebView, WebViewSessionState,
};

/// Trạng thái thanh báo cập nhật (dùng chung cho mọi cửa sổ).
#[derive(Clone)]
enum UpdateState {
    Available(crate::updater::Release),
    Working(String),
    Done(String),
    Error(String, String),
    UpToDate,
}

thread_local! {
    static UPDATE: RefCell<Option<UpdateState>> = const { RefCell::new(None) };
    /// Giữ các cửa sổ sống; mọi closure chỉ giữ Weak để không tạo vòng tham chiếu.
    static WINDOWS: RefCell<Vec<Rc<Browser>>> = const { RefCell::new(Vec::new()) };
}

pub enum Startup {
    Restore(Vec<String>),
    Urls(Vec<String>),
    /// Cửa sổ ẩn danh: dữ liệu chỉ nằm trong RAM, không lưu lịch sử/phiên.
    Incognito,
}

enum Open {
    Foreground,
    Background,
    Sleeping,
    /// WebView mở ra tab này + nhóm tiến trình của tab mở nó.
    Related(WebView, u64),
}

pub struct Tab {
    id: u64,
    audio: gtk::Button,
    root: gtk::Box,
    button: gtk::Box,
    icon: gtk::Image,
    spinner: gtk::Spinner,
    label: gtk::Label,
    view: RefCell<Option<WebView>>,
    url: RefCell<String>,
    title: RefCell<String>,
    saved_state: RefCell<Option<WebViewSessionState>>,
    last_active: Cell<Instant>,
    /// Các tab cùng nhóm dùng chung một web process (popup mở từ tab khác).
    group: Cell<u64>,
    /// Tab do công cụ AI (MCP) mở — hiện biểu tượng 🤖 trên tab.
    ai: Cell<bool>,
    ai_mark: gtk::Label,
    /// DevTools đang mở (gắn bên phải hoặc cửa sổ rời).
    devtools_open: Cell<bool>,
    /// Lần gần nhất tab phát âm thanh — tab vừa nghe nhạc không bị cho ngủ.
    last_audio: Cell<Option<Instant>>,
    /// Tab ghim: chỉ hiện biểu tượng, nằm bên trái, không bị cho ngủ.
    pinned: Cell<bool>,
    close_btn: gtk::Button,
}

impl Tab {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn is_ai(&self) -> bool {
        self.ai.get()
    }

    pub(crate) fn title(&self) -> String {
        self.title.borrow().clone()
    }

    pub(crate) fn url(&self) -> String {
        self.current_url()
    }

    pub(crate) fn is_sleeping(&self) -> bool {
        self.view.borrow().is_none()
    }

    pub(crate) fn web_view(&self) -> Option<WebView> {
        self.view()
    }

    fn view(&self) -> Option<WebView> {
        self.view.borrow().clone()
    }

    fn current_url(&self) -> String {
        self.view()
            .and_then(|v| v.uri())
            .map(|u| u.to_string())
            .unwrap_or_else(|| self.url.borrow().clone())
    }
}

pub struct Browser {
    engine: Rc<Engine>,
    /// Phiên mạng riêng, tạm thời (chỉ có ở cửa sổ ẩn danh).
    private: Option<webkit6::NetworkSession>,
    window: gtk::ApplicationWindow,
    titlebar: gtk::WindowHandle,
    toolbar: gtk::Box,
    strip: gtk::Box,
    /// Dải tab ghim (không chia đều độ rộng như dải tab thường).
    pinned_strip: gtk::Box,
    stack: gtk::Stack,
    url_entry: gtk::Entry,
    back_btn: gtk::Button,
    fwd_btn: gtk::Button,
    reload_btn: gtk::Button,
    progress: gtk::ProgressBar,
    status: gtk::Label,
    find_bar: gtk::Revealer,
    find_entry: gtk::SearchEntry,
    find_label: gtk::Label,
    suggest_pop: gtk::Popover,
    suggest_list: gtk::ListBox,
    suggestions: RefCell<Vec<String>>,
    url_editing: Cell<bool>,
    setting_url: Cell<bool>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    current: RefCell<Option<Rc<Tab>>>,
    closed: RefCell<Vec<String>>,
    /// Tab đang được mở menu chuột phải.
    ctx_tab: RefCell<Option<std::rc::Weak<Tab>>>,
    url_focused: Cell<bool>,
    /// Dấu báo "AI đang điều khiển" trên thanh công cụ.
    ai_badge: gtk::Button,
    /// Nút tải xuống (bong bóng kiểu Chrome).
    dl_btn: gtk::MenuButton,
    dl_label: gtk::Label,
    dl_list: gtk::Box,
    /// Thanh bookmark dưới thanh địa chỉ.
    bm_scroll: gtk::ScrolledWindow,
    bm_bar: gtk::Box,
    bm_cache: RefCell<Vec<(String, String)>>,
    /// Hỏi "Lưu mật khẩu?" (popover gắn vào thanh địa chỉ).
    password_pop: gtk::Popover,
    password_heading: gtk::Label,
    password_user: gtk::Label,
    password_save: gtk::Button,
    password_never: gtk::Button,
    password_pending: RefCell<Option<Credential>>,
    /// Thanh báo cập nhật (dưới thanh địa chỉ).
    update_bar: gtk::Revealer,
    update_label: gtk::Label,
    update_primary: gtk::Button,
    update_secondary: gtk::Button,
}

fn is_web(uri: &str) -> bool {
    uri.starts_with("http://") || uri.starts_with("https://")
}

fn next_group() -> u64 {
    thread_local!(static NEXT: Cell<u64> = const { Cell::new(1) });
    NEXT.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    })
}

/// URL rút gọn khi không sửa: bỏ "https://", "www." và dấu "/" cuối (như Chrome).
fn pretty_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("chome://") {
        return format!("chome://{}", rest.trim_end_matches('/'));
    }
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_suffix('/').filter(|r| !r.contains('/')).unwrap_or(rest);
    // Hiện chữ tiếng Việt thay vì mã %XX (vd. Vi%E1%BB%87t_Nam → Việt_Nam).
    glib::Uri::unescape_string(rest, None::<&str>)
        .map(|s| s.to_string())
        .unwrap_or_else(|| rest.to_string())
}

/// RAM còn trống của cả máy (MB), đọc từ /proc/meminfo.
fn mem_available_mb() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemAvailable:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

fn set_content(root: &gtk::Box, child: &impl IsA<gtk::Widget>) {
    while let Some(c) = root.first_child() {
        root.remove(&c);
    }
    root.append(child);
}

fn placeholder(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    l.set_vexpand(true);
    l
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("flat");
    b.set_focusable(false);
    b
}

impl Browser {
    pub fn new(app: &gtk::Application, engine: Rc<Engine>, startup: Startup) -> Rc<Self> {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(1280)
            .default_height(820)
            .title("Chỏme")
            .build();

        // ---- Thanh tab (đặt trong titlebar, kiểu Chrome) ----
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        strip.add_css_class("tabstrip");
        strip.set_homogeneous(true); // mọi tab rộng bằng nhau, co lại đều khi nhiều tab
        let strip_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .child(&strip)
            .build();
        // Không để nhãn tab (hexpand) kéo giãn cả dải tab: tab giữ ~240px như Chrome.
        strip_scroll.set_hexpand(false);
        let new_tab_btn = icon_button("list-add-symbolic", "Tab mới (Ctrl+T)");
        new_tab_btn.set_action_name(Some("win.new-tab"));
        new_tab_btn.set_valign(gtk::Align::Center);
        let filler = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        filler.set_hexpand(true);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let pinned_strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        pinned_strip.add_css_class("tabstrip");
        top.append(&pinned_strip);
        top.append(&strip_scroll);
        top.append(&new_tab_btn);
        top.append(&filler);
        top.append(&gtk::WindowControls::new(gtk::PackType::End));
        let titlebar = gtk::WindowHandle::new();
        titlebar.set_child(Some(&top));
        titlebar.add_css_class("tabbar");
        window.set_titlebar(Some(&titlebar));

        // ---- Thanh công cụ ----
        let back_btn = icon_button("go-previous-symbolic", "Quay lại (Alt+←)");
        back_btn.set_action_name(Some("win.back"));
        let fwd_btn = icon_button("go-next-symbolic", "Tiến tới (Alt+→)");
        fwd_btn.set_action_name(Some("win.forward"));
        let reload_btn = icon_button("view-refresh-symbolic", "Tải lại (F5)");
        reload_btn.set_action_name(Some("win.reload-or-stop"));
        let url_entry = gtk::Entry::new();
        url_entry.set_hexpand(true);
        url_entry.set_placeholder_text(Some("Tìm kiếm hoặc nhập địa chỉ"));
        url_entry.add_css_class("urlbar");
        url_entry.set_input_purpose(gtk::InputPurpose::Url);

        let menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("Tab mới"), Some("win.new-tab"));
        s1.append(Some("Cửa sổ mới"), Some("win.new-window"));
        s1.append(Some("Cửa sổ ẩn danh mới"), Some("win.new-incognito"));
        s1.append(Some("Mở lại tab vừa đóng"), Some("win.reopen-tab"));
        menu.append_section(None, &s1);
        let s2 = gio::Menu::new();
        s2.append(Some("Lịch sử"), Some("win.history"));
        s2.append(Some("Tải xuống"), Some("win.downloads"));
        s2.append(Some("Hiện/ẩn thanh bookmark"), Some("win.bookmarks-bar"));
        s2.append(Some("Bookmark"), Some("win.bookmarks"));
        s2.append(Some("Tìm trong trang"), Some("win.find"));
        s2.append(Some("Dịch trang sang tiếng Việt"), Some("win.translate"));
        menu.append_section(None, &s2);
        let s3 = gio::Menu::new();
        s3.append(Some("Phóng to"), Some("win.zoom-in"));
        s3.append(Some("Thu nhỏ"), Some("win.zoom-out"));
        s3.append(Some("Cỡ mặc định"), Some("win.zoom-reset"));
        s3.append(Some("Toàn màn hình"), Some("win.fullscreen"));
        s3.append(Some("In…"), Some("win.print"));
        menu.append_section(None, &s3);
        let s4 = gio::Menu::new();
        s4.append(Some("Chặn quảng cáo"), Some("win.adblock"));
        s4.append(Some("Cho các tab nền ngủ ngay"), Some("win.sleep-all"));
        s4.append(Some("Công cụ nhà phát triển"), Some("win.devtools"));
        s4.append(Some("Mật khẩu"), Some("win.passwords"));
        s4.append(Some("Kiểm tra cập nhật"), Some("win.check-update"));
        s4.append(Some("Cài đặt"), Some("win.settings"));
        menu.append_section(None, &s4);
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .tooltip_text("Menu")
            .build();
        menu_btn.add_css_class("flat");
        menu_btn.set_has_frame(false);
        menu_btn.set_focusable(false);

        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        toolbar.add_css_class("navbar");
        for w in [back_btn.upcast_ref::<gtk::Widget>(), fwd_btn.upcast_ref(), reload_btn.upcast_ref()] {
            toolbar.append(w);
        }
        let ai_badge = gtk::Button::with_label("🤖 AI");
        ai_badge.add_css_class("ai-badge");
        ai_badge.set_tooltip_text(Some("Một công cụ AI (MCP) đang điều khiển trình duyệt. Bấm để mở Cài đặt."));
        ai_badge.set_action_name(Some("win.settings"));
        ai_badge.set_focusable(false);
        ai_badge.set_visible(false);
        toolbar.append(&url_entry);
        toolbar.append(&ai_badge);
        // ---- Nút tải xuống ----
        let dl_icon = gtk::Image::from_icon_name("folder-download-symbolic");
        let dl_label = gtk::Label::new(None);
        dl_label.add_css_class("dl-percent");
        dl_label.set_visible(false);
        let dl_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        dl_box.append(&dl_icon);
        dl_box.append(&dl_label);
        let dl_list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        dl_list.add_css_class("dl-list");
        let dl_pop = gtk::Popover::new();
        dl_pop.set_child(Some(&dl_list));
        let dl_btn = gtk::MenuButton::builder()
            .child(&dl_box)
            .popover(&dl_pop)
            .tooltip_text("Tải xuống (Ctrl+J)")
            .build();
        dl_btn.add_css_class("flat");
        dl_btn.set_has_frame(false);
        dl_btn.set_focusable(false);
        dl_btn.set_visible(false);
        toolbar.append(&dl_btn);
        toolbar.append(&menu_btn);

        let password_heading = gtk::Label::new(Some("Lưu mật khẩu?"));
        password_heading.set_xalign(0.0);
        password_heading.add_css_class("heading");
        let password_user = gtk::Label::new(None);
        password_user.set_xalign(0.0);
        password_user.add_css_class("dim-label");
        let password_save = gtk::Button::with_label("Lưu");
        password_save.add_css_class("suggested-action");
        let password_never = gtk::Button::with_label("Không bao giờ cho trang này");
        password_never.add_css_class("flat");
        let password_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        password_buttons.set_halign(gtk::Align::End);
        password_buttons.append(&password_never);
        password_buttons.append(&password_save);
        let password_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        password_box.set_margin_top(12);
        password_box.set_margin_bottom(12);
        password_box.set_margin_start(14);
        password_box.set_margin_end(14);
        password_box.append(&password_heading);
        password_box.append(&password_user);
        password_box.append(&password_buttons);
        let password_pop = gtk::Popover::new();
        password_pop.set_child(Some(&password_box));
        password_pop.set_parent(&url_entry);
        password_pop.set_has_arrow(true);
        password_pop.set_autohide(true);

        // ---- Gợi ý thanh địa chỉ ----
        let suggest_list = gtk::ListBox::new();
        suggest_list.set_selection_mode(gtk::SelectionMode::Single);
        suggest_list.set_focusable(false);
        suggest_list.add_css_class("suggestions");
        let suggest_pop = gtk::Popover::new();
        suggest_pop.set_child(Some(&suggest_list));
        suggest_pop.set_autohide(false);
        suggest_pop.set_has_arrow(false);
        suggest_pop.set_can_focus(false);
        suggest_pop.set_position(gtk::PositionType::Bottom);
        suggest_pop.set_halign(gtk::Align::Start);
        suggest_pop.set_parent(&url_entry);

        // ---- Nội dung ----
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        let progress = gtk::ProgressBar::new();
        progress.add_css_class("loadbar");
        progress.set_valign(gtk::Align::Start);
        progress.set_visible(false);
        progress.set_can_target(false);
        let status = gtk::Label::new(None);
        status.add_css_class("statusbubble");
        status.set_halign(gtk::Align::Start);
        status.set_valign(gtk::Align::End);
        status.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        status.set_max_width_chars(80);
        status.set_visible(false);
        status.set_can_target(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&stack));
        overlay.add_overlay(&progress);
        overlay.add_overlay(&status);

        // ---- Thanh tìm trong trang ----
        let find_entry = gtk::SearchEntry::new();
        find_entry.set_placeholder_text(Some("Tìm trong trang"));
        find_entry.set_width_chars(30);
        let find_label = gtk::Label::new(None);
        find_label.add_css_class("dim-label");
        let find_prev = icon_button("go-up-symbolic", "Kết quả trước (Ctrl+Shift+G)");
        let find_next = icon_button("go-down-symbolic", "Kết quả sau (Enter / Ctrl+G)");
        let find_close = icon_button("window-close-symbolic", "Đóng (Esc)");
        let find_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        find_box.add_css_class("findbar");
        find_box.append(&find_entry);
        find_box.append(&find_label);
        find_box.append(&find_prev);
        find_box.append(&find_next);
        let find_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        find_spacer.set_hexpand(true);
        find_box.append(&find_spacer);
        find_box.append(&find_close);
        let find_bar = gtk::Revealer::new();
        find_bar.set_child(Some(&find_box));
        find_bar.set_transition_type(gtk::RevealerTransitionType::SlideUp);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&toolbar);
        let bm_bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let bm_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&bm_bar)
            .build();
        bm_scroll.add_css_class("bookmarks-bar");
        bm_scroll.set_visible(false);
        content.append(&bm_scroll);
        // ---- Thanh báo cập nhật ----
        let update_label = gtk::Label::new(None);
        update_label.set_xalign(0.0);
        update_label.set_hexpand(true);
        update_label.set_wrap(true);
        let update_primary = gtk::Button::with_label("Cập nhật");
        update_primary.add_css_class("suggested-action");
        update_primary.set_action_name(Some("win.update-primary"));
        let update_secondary = gtk::Button::with_label("Xem có gì mới");
        update_secondary.add_css_class("flat");
        update_secondary.set_action_name(Some("win.update-notes"));
        let update_close = icon_button("window-close-symbolic", "Để sau");
        let update_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        update_box.add_css_class("update-bar");
        update_box.append(&gtk::Image::from_icon_name("software-update-available-symbolic"));
        update_box.append(&update_label);
        update_box.append(&update_secondary);
        update_box.append(&update_primary);
        update_box.append(&update_close);
        let update_bar = gtk::Revealer::new();
        update_bar.set_child(Some(&update_box));
        let bar = update_bar.clone();
        update_close.connect_clicked(move |_| bar.set_reveal_child(false));
        content.append(&update_bar);
        content.append(&overlay);
        content.append(&find_bar);
        window.set_child(Some(&content));

        let private = matches!(startup, Startup::Incognito).then(webkit6::NetworkSession::new_ephemeral);
        if let Some(p) = &private {
            engine.downloads.attach(p, true);
        }
        if private.is_some() {
            window.add_css_class("incognito");
            let badge = gtk::Label::new(Some("🕶 Ẩn danh"));
            badge.add_css_class("incognito-badge");
            badge.set_tooltip_text(Some("Cửa sổ ẩn danh: không lưu lịch sử, cookie và dữ liệu trang khi đóng"));
            toolbar.prepend(&badge);
        }
        let b = Rc::new(Self {
            engine,
            private,
            window,
            titlebar,
            toolbar,
            strip,
            pinned_strip,
            stack,
            url_entry,
            back_btn,
            fwd_btn,
            reload_btn,
            progress,
            status,
            find_bar,
            find_entry,
            find_label,
            suggest_pop,
            suggest_list,
            suggestions: RefCell::new(vec![]),
            url_editing: Cell::new(false),
            setting_url: Cell::new(false),
            tabs: RefCell::new(vec![]),
            current: RefCell::new(None),
            closed: RefCell::new(vec![]),
            ctx_tab: RefCell::new(None),
            url_focused: Cell::new(false),
            ai_badge,
            dl_btn,
            dl_label,
            dl_list,
            bm_scroll,
            bm_bar,
            bm_cache: RefCell::new(vec![]),
            password_pop,
            password_heading,
            password_user,
            password_save,
            password_never,
            password_pending: RefCell::new(None),
            update_bar,
            update_label,
            update_primary,
            update_secondary,
        });

        b.setup_actions();
        let weak = Rc::downgrade(&b);
        b.engine.downloads.add_listener(move |new| {
            if let Some(b) = weak.upgrade() {
                b.refresh_downloads(new);
            }
        });
        b.refresh_downloads(None);
        if let Some(state) = UPDATE.with(|u| u.borrow().clone()) {
            b.show_update(&state); // cửa sổ mở sau vẫn thấy thông báo đang có
        }
        b.setup_url_entry();
        b.setup_passwords();
        b.setup_find_bar(&find_prev, &find_next, &find_close);
        b.setup_window_events();
        b.start_sleeper();

        // ---- Mở tab ban đầu ----
        let (urls, restore) = match startup {
            Startup::Restore(urls) => (urls, true),
            Startup::Urls(urls) => (urls, false),
            Startup::Incognito => (vec![], false),
        };
        if restore && b.engine.prefs.borrow().restore_session {
            let (saved, active) = b.engine.db.load_session();
            for (url, title, pinned) in &saved {
                let t = b.add_tab(url, title, Open::Sleeping);
                if *pinned {
                    b.set_pinned(&t, true);
                }
            }
            if let Some(t) = b.tabs.borrow().get(active).cloned() {
                b.select(&t);
            }
        }
        for url in &urls {
            let resolved = b.engine.resolve_input(url);
            b.add_tab(&resolved, "", Open::Foreground);
        }
        if b.tabs.borrow().is_empty() {
            b.add_tab(NEWTAB, "Tab mới", Open::Foreground);
        }

        WINDOWS.with(|w| w.borrow_mut().push(b.clone()));
        b.debug_screenshot();
        b.debug_js();
        b.window.present();
        b
    }

    /// Chỉ dùng khi đo đạc: `CHOME_DEBUG_JS=file` — mỗi dòng "giây<TAB>script"; script chạy
    /// trên tab hiện tại sau số giây đó, kết quả in ra stderr dạng "[js] <kết quả>".
    fn debug_js(self: &Rc<Self>) {
        let Some(text) = std::env::var("CHOME_DEBUG_JS").ok().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return;
        };
        for line in text.lines() {
            let Some((secs, script)) = line.split_once('\t') else { continue };
            let Ok(secs) = secs.trim().parse::<u32>() else { continue };
            let (weak, script) = (Rc::downgrade(self), script.to_string());
            glib::timeout_add_seconds_local_once(secs, move || {
                // "@tên" = kích hoạt hành động của cửa sổ (vd. @devtools tương đương F12).
                if script == "@@update" {
                    if let Some(b) = weak.upgrade() {
                        eprintln!(
                            "[js] thanh cập nhật hiện: {} | \"{}\" | nút: {}",
                            b.update_bar.reveals_child(),
                            b.update_label.text(),
                            if b.update_primary.is_visible() { b.update_primary.label().map(|l| l.to_string()).unwrap_or_default() } else { "(không)".into() }
                        );
                    }
                    return;
                }
                if script == "@@password-save" {
                    if let Some(b) = weak.upgrade() {
                        eprintln!("[js] popover hỏi lưu đang mở: {} | tiêu đề: {}", b.password_pop.is_visible(), b.password_heading.text());
                        b.password_save.emit_clicked();
                    }
                    return;
                }
                if script == "@@layout" {
                    if let Some(t) = weak.upgrade().and_then(|b| b.current()) {
                        let child = t.root.first_child();
                        let desc = match child.as_ref().and_then(|c| c.clone().downcast::<gtk::Paned>().ok()) {
                            Some(p) => format!(
                                "khung chia đôi: vị trí {} | trái {} ({}px) | phải {} ({}px)",
                                p.position(),
                                p.start_child().map(|w| w.type_().name().to_string()).unwrap_or_default(),
                                p.start_child().map(|w| w.width()).unwrap_or(0),
                                p.end_child().map(|w| w.type_().name().to_string()).unwrap_or_default(),
                                p.end_child().map(|w| w.width()).unwrap_or(0)
                            ),
                            None => format!("một khung: {}", child.map(|w| w.type_().name().to_string()).unwrap_or_default()),
                        };
                        eprintln!("[js] bố cục (cửa sổ {}px): {desc}", t.root.width());
                    }
                    return;
                }
                if let Some(action) = script.strip_prefix('@') {
                    if let Some(b) = weak.upgrade() {
                        let name = action.trim();
                        let full = if name.contains('.') { name.to_string() } else { format!("win.{name}") };
                        let r = WidgetExt::activate_action(&b.window, &full, None);
                        eprintln!("[js] hành động {action}: {}", if r.is_ok() { "ok" } else { "lỗi" });
                    }
                    return;
                }
                let Some(v) = weak.upgrade().and_then(|b| b.current_view()) else { return };
                v.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |r| match r {
                    Ok(val) => eprintln!("[js] {}", val.to_str()),
                    Err(e) => eprintln!("[js] lỗi: {e}"),
                });
            });
        }
    }

    /// Chỉ dùng khi kiểm tra giao diện: `CHOME_SCREENSHOT=out.png [CHOME_SCREENSHOT_DELAY=giây]
    /// [CHOME_SCREENSHOT_TYPE=chữ]` → app tự vẽ cửa sổ ra PNG rồi thoát.
    fn debug_screenshot(self: &Rc<Self>) {
        let Ok(path) = std::env::var("CHOME_SCREENSHOT") else { return };
        let delay: u32 = std::env::var("CHOME_SCREENSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(8);
        let weak = Rc::downgrade(self);
        if let Ok(text) = std::env::var("CHOME_SCREENSHOT_TYPE") {
            let weak = weak.clone();
            glib::timeout_add_seconds_local_once(delay.saturating_sub(2).max(1), move || {
                if let Some(b) = weak.upgrade() {
                    b.url_entry.grab_focus();
                    b.url_entry.set_text(&text);
                    b.url_entry.set_position(-1);
                }
            });
        }
        let tries = Cell::new(0);
        glib::timeout_add_seconds_local(delay, move || {
            let Some(b) = weak.upgrade() else { return glib::ControlFlow::Break };
            let w = &b.window;
            let whole = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(w)).snapshot(&whole, w.width() as f64, w.height() as f64);
            // Cách 2 nếu cách 1 không ra gì: vẽ từng phần (thanh tiêu đề + nội dung) rồi ghép.
            let snap = gtk::Snapshot::new();
            let tb = &b.titlebar;
            let th = tb.height() as f32;
            gtk::WidgetPaintable::new(Some(tb)).snapshot(&snap, tb.width() as f64, th as f64);
            if let Some(content) = w.child() {
                snap.save();
                snap.translate(&gtk::graphene::Point::new(0.0, th));
                gtk::WidgetPaintable::new(Some(&content)).snapshot(&snap, content.width() as f64, content.height() as f64);
                snap.restore();
            }
            let Some(node) = whole.to_node().or_else(|| snap.to_node()) else {
                // Cửa sổ chưa vẽ khung hình nào (đang bị che): yêu cầu vẽ rồi thử lại.
                tries.set(tries.get() + 1);
                w.queue_draw();
                return if tries.get() < 15 { glib::ControlFlow::Continue } else { glib::ControlFlow::Break };
            };
            if let Some(renderer) = w.renderer() {
                let tex = renderer.render_texture(&node, None);
                if let Err(e) = tex.save_to_png(&path) {
                    eprintln!("[screenshot] {e}");
                }
            }
            // Khi DevTools đang gắn: chụp riêng khung DevTools để kiểm tra giao diện.
            if let Some(paned) = b.current().and_then(|t| t.root.first_child()).and_then(|c| c.downcast::<gtk::Paned>().ok()) {
                for (child, suffix) in [(paned.start_child(), "-page.png"), (paned.end_child(), "-devtools.png")] {
                    let Some(child) = child else { continue };
                    let cs = gtk::Snapshot::new();
                    gtk::WidgetPaintable::new(Some(&child)).snapshot(&cs, child.width() as f64, child.height() as f64);
                    match (cs.to_node(), w.renderer()) {
                        (Some(node), Some(r)) => {
                            let _ = r.render_texture(&node, None).save_to_png(path.replace(".png", suffix));
                        }
                        _ => eprintln!("[screenshot] {suffix}: không vẽ được ({}x{})", child.width(), child.height()),
                    }
                }
            }
            if b.suggest_pop.is_visible() {
                let pop = &b.suggest_pop;
                let pp = gtk::WidgetPaintable::new(Some(pop));
                let ps = gtk::Snapshot::new();
                pp.snapshot(&ps, pop.width() as f64, pop.height() as f64);
                if let (Some(node), Some(r)) = (ps.to_node(), pop.renderer()) {
                    let _ = r.render_texture(&node, None).save_to_png(path.replace(".png", "-popup.png"));
                }
            }
            if let Some(app) = w.application() {
                app.quit();
            }
            glib::ControlFlow::Break
        });
    }

    // ================= API cho máy chủ MCP =================

    pub(crate) fn all() -> Vec<Rc<Self>> {
        WINDOWS.with(|w| w.borrow().clone())
    }

    pub(crate) fn tabs(&self) -> Vec<Rc<Tab>> {
        self.tabs.borrow().clone()
    }

    pub(crate) fn is_active(&self, tab: &Rc<Tab>) -> bool {
        self.is_current(tab)
    }

    pub(crate) fn active_tab(&self) -> Option<Rc<Tab>> {
        self.current()
    }

    /// Tìm tab theo id trên mọi cửa sổ.
    pub(crate) fn find_tab(id: u64) -> Option<(Rc<Browser>, Rc<Tab>)> {
        Self::all()
            .into_iter()
            .find_map(|b| b.tabs().into_iter().find(|t| t.id == id).map(|t| (b.clone(), t)))
    }

    /// Mở tab mới do AI yêu cầu (đánh dấu 🤖) và chuyển sang tab đó.
    pub(crate) fn open_ai_tab(self: &Rc<Self>, url: &str) -> Rc<Tab> {
        let resolved = self.engine.resolve_input(url);
        let tab = self.add_tab(&resolved, "", Open::Foreground);
        tab.ai.set(true);
        tab.ai_mark.set_visible(true);
        tab
    }

    pub(crate) fn select_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        self.select(tab);
    }

    pub(crate) fn close(self: &Rc<Self>, tab: &Rc<Tab>) {
        self.close_tab(tab);
    }

    pub(crate) fn resolve(&self, input: &str) -> String {
        self.engine.resolve_input(input)
    }

    pub(crate) fn set_ai_active(&self, on: bool) {
        self.ai_badge.set_visible(on);
    }

    pub(crate) fn window(&self) -> &gtk::ApplicationWindow {
        &self.window
    }

    pub fn first() -> Option<Rc<Self>> {
        WINDOWS.with(|w| w.borrow().last().cloned())
    }

    pub fn open_urls(self: &Rc<Self>, urls: &[String]) {
        for url in urls {
            let resolved = self.engine.resolve_input(url);
            self.add_tab(&resolved, "", Open::Foreground);
        }
        self.window.present();
    }

    // ================= Tab =================

    fn current(&self) -> Option<Rc<Tab>> {
        self.current.borrow().clone()
    }

    fn current_view(&self) -> Option<WebView> {
        self.current().and_then(|t| t.view())
    }

    fn is_current(&self, tab: &Rc<Tab>) -> bool {
        self.current.borrow().as_ref().is_some_and(|c| Rc::ptr_eq(c, tab))
    }

    fn index_of(&self, tab: &Rc<Tab>) -> Option<usize> {
        self.tabs.borrow().iter().position(|t| Rc::ptr_eq(t, tab))
    }

    fn add_tab(self: &Rc<Self>, url: &str, title: &str, mode: Open) -> Rc<Tab> {
        let icon = gtk::Image::from_icon_name(if url.starts_with("chome://") { crate::APP_ID } else { "web-browser-symbolic" });
        icon.set_pixel_size(16);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        let label = gtk::Label::new(Some(if title.is_empty() { url } else { title }));
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_width_chars(1);
        label.set_max_width_chars(26); // tab tối đa ~240px như Chrome
        let audio = icon_button("audio-volume-high-symbolic", "Tắt tiếng tab");
        audio.add_css_class("tab-audio");
        audio.set_visible(false);
        let close = icon_button("window-close-symbolic", "Đóng tab (Ctrl+W)");
        close.add_css_class("tab-close");
        let button = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        button.add_css_class("tab");
        let ai_mark = gtk::Label::new(Some("🤖"));
        ai_mark.set_tooltip_text(Some("Tab do công cụ AI mở"));
        ai_mark.set_visible(false);
        button.append(&ai_mark);
        button.append(&icon);
        button.append(&spinner);
        button.append(&label);
        button.append(&audio);
        button.append(&close);
        button.set_tooltip_text(Some(if title.is_empty() { url } else { title }));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let tab = Rc::new(Tab {
            root,
            button,
            icon,
            spinner,
            label,
            view: RefCell::new(None),
            url: RefCell::new(url.to_string()),
            title: RefCell::new(title.to_string()),
            saved_state: RefCell::new(None),
            last_active: Cell::new(Instant::now()),
            group: Cell::new(0),
            ai: Cell::new(false),
            ai_mark,
            devtools_open: Cell::new(false),
            last_audio: Cell::new(None),
            pinned: Cell::new(false),
            close_btn: close.clone(),
            id: next_group(),
            audio,
        });

        // Click trái: chọn tab. Click giữa: đóng tab.
        let gesture = gtk::GestureClick::new();
        gesture.set_button(0);
        let (wb, wt) = (Rc::downgrade(self), Rc::downgrade(&tab));
        gesture.connect_pressed(move |g, _, _, _| {
            let (Some(b), Some(t)) = (wb.upgrade(), wt.upgrade()) else { return };
            match g.current_button() {
                1 => b.select(&t),
                2 => b.close_tab(&t),
                3 => b.show_tab_menu(&t),
                _ => {}
            }
        });
        tab.button.add_controller(gesture);
        let (wb, wt) = (Rc::downgrade(self), Rc::downgrade(&tab));
        tab.audio.connect_clicked(move |_| {
            let (Some(b), Some(t)) = (wb.upgrade(), wt.upgrade()) else { return };
            b.toggle_mute(&t);
        });
        self.setup_tab_dnd(&tab);
        let (wb, wt) = (Rc::downgrade(self), Rc::downgrade(&tab));
        close.connect_clicked(move |_| {
            if let (Some(b), Some(t)) = (wb.upgrade(), wt.upgrade()) {
                b.close_tab(&t);
            }
        });

        // Tab mở từ trang hiện tại được đặt ngay bên phải tab đó (giống Chrome).
        let after_current = matches!(mode, Open::Background | Open::Related(..));
        let pos = match (after_current, self.current()) {
            (true, Some(cur)) => self.index_of(&cur).map(|i| i + 1),
            _ => None,
        };
        // Tab mới luôn vào dải thường: nếu tab đang xem là tab ghim, đặt ở đầu dải thường.
        let pinned_count = self.tabs.borrow().iter().filter(|t| t.pinned.get()).count();
        let pos = pos.map(|i| i.max(pinned_count));
        match pos {
            Some(i) if i == pinned_count => {
                self.strip.prepend(&tab.button);
                self.tabs.borrow_mut().insert(i, tab.clone());
            }
            Some(i) => {
                let prev = self.tabs.borrow()[i - 1].button.clone();
                self.strip.insert_child_after(&tab.button, Some(&prev));
                self.tabs.borrow_mut().insert(i, tab.clone());
            }
            None => {
                self.strip.append(&tab.button);
                self.tabs.borrow_mut().push(tab.clone());
            }
        }
        self.stack.add_child(&tab.root);

        match mode {
            Open::Sleeping => {
                set_content(&tab.root, &placeholder("💤"));
                tab.button.add_css_class("sleeping");
            }
            Open::Related(related, group) => {
                self.make_view(&tab, Some(&related));
                tab.group.set(group);
                self.select(&tab);
            }
            Open::Background => {
                // Tab mở nền chưa tải gì cho tới khi người dùng bấm vào → 0 MB.
                set_content(&tab.root, &placeholder("💤"));
                tab.button.add_css_class("sleeping");
            }
            Open::Foreground => {
                self.make_view(&tab, None).load_uri(url);
                self.select(&tab);
            }
        }
        tab
    }

    fn select(self: &Rc<Self>, tab: &Rc<Tab>) {
        if let Some(prev) = self.current() {
            prev.last_active.set(Instant::now());
            prev.button.remove_css_class("active");
        }
        *self.current.borrow_mut() = Some(tab.clone());
        tab.button.add_css_class("active");
        tab.last_active.set(Instant::now());
        self.wake(tab);
        self.stack.set_visible_child(&tab.root);
        self.hide_find();
        self.status.set_visible(false);
        self.sync_ui();
        if tab.current_url() == NEWTAB {
            self.url_entry.grab_focus();
        } else if let Some(v) = tab.view() {
            v.grab_focus();
        }
    }

    fn close_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        let Some(idx) = self.index_of(tab) else { return };
        let url = tab.current_url();
        if url != NEWTAB {
            self.closed.borrow_mut().push(url);
        }
        if self.tabs.borrow().len() == 1 {
            self.window.close();
            return;
        }
        let was_current = self.is_current(tab);
        self.tabs.borrow_mut().remove(idx);
        self.strip.remove(&tab.button);
        if let Some(v) = tab.view.borrow_mut().take() {
            tab.root.remove(&v);
        }
        self.stack.remove(&tab.root);
        if was_current {
            *self.current.borrow_mut() = None;
            let next = {
                let tabs = self.tabs.borrow();
                tabs[idx.min(tabs.len() - 1)].clone()
            };
            self.select(&next);
        }
    }

    /// Giải phóng WebView của tab nền, chỉ giữ trạng thái điều hướng.
    fn sleep(&self, tab: &Rc<Tab>) {
        if self.is_current(tab) {
            return;
        }
        let Some(view) = tab.view.borrow_mut().take() else { return };
        if let Some(u) = view.uri() {
            *tab.url.borrow_mut() = u.to_string();
        }
        *tab.saved_state.borrow_mut() = view.session_state();
        // WebKit giữ web process sống sau khi WebView bị hủy, nên phải tắt hẳn nó —
        // trừ khi còn tab đang thức khác dùng chung process (cùng nhóm).
        let group = tab.group.get();
        let shared = self
            .tabs
            .borrow()
            .iter()
            .any(|t| !Rc::ptr_eq(t, tab) && t.group.get() == group && t.view.borrow().is_some());
        if !shared {
            view.terminate_web_process();
        }
        set_content(&tab.root, &placeholder("💤"));
        tab.spinner.set_visible(false);
        tab.spinner.stop();
        tab.icon.set_visible(true);
        tab.button.add_css_class("sleeping");
    }

    pub(crate) fn wake(self: &Rc<Self>, tab: &Rc<Tab>) {
        if tab.view.borrow().is_some() {
            return;
        }
        tab.button.remove_css_class("sleeping");
        let view = self.make_view(tab, None);
        let url = tab.url.borrow().clone();
        let state = tab.saved_state.borrow_mut().take();
        if let Some(state) = state {
            view.restore_session_state(&state);
            if let Some(item) = view.back_forward_list().and_then(|l| l.current_item()) {
                view.go_to_back_forward_list_item(&item);
                return;
            }
        }
        view.load_uri(&url);
    }

    /// F12: mở DevTools gắn bên phải trang (như Chrome); bấm lần nữa thì đóng.
    fn toggle_devtools(&self, allow_close: bool) {
        let Some(tab) = self.current() else { return };
        let Some(inspector) = tab.view().and_then(|v| v.inspector()) else { return };
        if tab.devtools_open.get() {
            if allow_close {
                inspector.close();
            }
            return;
        }
        inspector.show();
        if !inspector.is_attached() && inspector.can_attach() {
            inspector.attach();
        }
        if std::env::var_os("CHOME_DEBUG_JS").is_some() {
            eprintln!("[js] F12: attached={} can_attach={}", inspector.is_attached(), inspector.can_attach());
        }
    }

    /// Đặt DevTools vào khung chia đôi: trang | DevTools.
    fn dock_devtools(tab: &Rc<Tab>, devtools: &gtk::Widget) -> bool {
        let Some(view) = tab.view() else { return false };
        if devtools.parent().is_some() {
            return true; // đã gắn
        }
        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_wide_handle(true);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_resize_end_child(false);
        paned.add_css_class("devtools-split");
        devtools.set_size_request(320, -1);
        let width = tab.root.width();
        set_content(&tab.root, &gtk::Box::new(gtk::Orientation::Vertical, 0)); // gỡ view khỏi root
        paned.set_start_child(Some(&view));
        paned.set_end_child(Some(devtools));
        set_content(&tab.root, &paned);
        paned.set_vexpand(true);
        paned.set_hexpand(true);
        if width > 0 {
            paned.set_position((width as f64 * 0.6) as i32); // DevTools chiếm ~40% bên phải
        }
        true
    }

    /// Gỡ DevTools khỏi khung chia đôi, trả trang về như cũ.
    fn undock_devtools(tab: &Rc<Tab>, devtools: Option<&gtk::Widget>) {
        let Some(view) = tab.view() else { return };
        if let Some(paned) = tab.root.first_child().and_then(|c| c.downcast::<gtk::Paned>().ok()) {
            paned.set_start_child(None::<&gtk::Widget>);
            paned.set_end_child(None::<&gtk::Widget>);
            set_content(&tab.root, &view);
        } else if let Some(d) = devtools {
            if let Some(parent) = d.parent().and_then(|p| p.downcast::<gtk::Paned>().ok()) {
                parent.set_end_child(None::<&gtk::Widget>);
            }
        }
    }

    /// Cập nhật nút + danh sách tải xuống. `new` = vừa bắt đầu tải → mở danh sách (như Chrome).
    fn refresh_downloads(&self, new: Option<&Rc<crate::downloads::Item>>) {
        use crate::downloads::{self as dl, State};
        let items = self.engine.downloads.items();
        self.dl_btn.set_visible(!items.is_empty());
        let running: Vec<_> = items.iter().filter(|i| i.state.get() == State::Running).collect();
        if running.is_empty() {
            self.dl_label.set_visible(false);
            self.dl_btn.remove_css_class("downloading");
        } else {
            let fracs: Vec<f64> = running.iter().filter_map(|i| i.fraction()).collect();
            let text = if fracs.is_empty() {
                "…".to_string()
            } else {
                format!("{:.0}%", fracs.iter().sum::<f64>() / fracs.len() as f64 * 100.0)
            };
            self.dl_label.set_text(&text);
            self.dl_label.set_visible(true);
            self.dl_btn.add_css_class("downloading");
        }

        while let Some(c) = self.dl_list.first_child() {
            self.dl_list.remove(&c);
        }
        let title = gtk::Label::new(Some("Tải xuống gần đây"));
        title.add_css_class("heading");
        title.set_xalign(0.0);
        self.dl_list.append(&title);
        for it in items.iter().take(8) {
            let name = it.filename.borrow().clone();
            let name_label = gtk::Label::new(Some(if name.is_empty() { "Đang chuẩn bị…" } else { &name }));
            name_label.set_xalign(0.0);
            name_label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            name_label.set_max_width_chars(36);
            let status = gtk::Label::new(Some(&it.status_text()));
            status.set_xalign(0.0);
            status.add_css_class("dim-label");
            status.add_css_class("caption");
            if it.state.get() == State::Failed {
                status.add_css_class("error");
            }
            let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
            col.set_hexpand(true);
            col.append(&name_label);
            col.append(&status);
            if let Some(f) = it.fraction() {
                let bar = gtk::ProgressBar::new();
                bar.set_fraction(f);
                col.append(&bar);
            }
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("dl-row");
            row.append(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
            row.append(&col);
            let path = it.path.borrow().clone();
            match it.state.get() {
                State::Running => {
                    let cancel = icon_button("process-stop-symbolic", "Hủy");
                    let item = it.clone();
                    cancel.connect_clicked(move |_| item.cancel());
                    row.append(&cancel);
                }
                State::Done => {
                    if let Some(p) = path.filter(|p| p.exists()) {
                        let open = icon_button("document-open-symbolic", "Mở");
                        let p1 = p.clone();
                        open.connect_clicked(move |_| dl::open_file(&p1));
                        let folder = icon_button("folder-open-symbolic", "Hiện trong thư mục");
                        folder.connect_clicked(move |_| dl::show_in_folder(&p));
                        row.append(&open);
                        row.append(&folder);
                    }
                }
                _ => {}
            }
            self.dl_list.append(&row);
        }
        let all = gtk::Button::with_label("Xem tất cả tải xuống");
        all.add_css_class("flat");
        all.set_action_name(Some("win.downloads"));
        let pop = self.dl_btn.popover();
        all.connect_clicked(move |_| {
            if let Some(p) = &pop {
                p.popdown();
            }
        });
        self.dl_list.append(&all);

        if new.is_some() && self.window.is_active() {
            self.dl_btn.popup();
        }
    }

    /// Ghim / bỏ ghim: chuyển nút tab giữa dải ghim và dải thường, tab ghim luôn đứng đầu.
    fn set_pinned(&self, tab: &Rc<Tab>, on: bool) {
        if tab.pinned.get() == on {
            return;
        }
        tab.pinned.set(on);
        if let Some(parent) = tab.button.parent().and_then(|p| p.downcast::<gtk::Box>().ok()) {
            parent.remove(&tab.button);
        }
        let mut tabs = self.tabs.borrow_mut();
        if let Some(i) = tabs.iter().position(|t| Rc::ptr_eq(t, tab)) {
            tabs.remove(i);
        }
        // Vị trí mới: cuối nhóm ghim (khi ghim) hoặc đầu nhóm thường (khi bỏ ghim) — cùng một chỗ.
        let boundary = tabs.iter().take_while(|t| t.pinned.get()).count();
        tabs.insert(boundary, tab.clone());
        drop(tabs);
        if on {
            self.pinned_strip.append(&tab.button);
            tab.button.add_css_class("pinned");
        } else {
            self.strip.prepend(&tab.button);
            tab.button.remove_css_class("pinned");
        }
        tab.label.set_visible(!on);
        tab.close_btn.set_visible(!on);
        tab.button.set_tooltip_text(Some(&tab.title.borrow()));
    }

    /// Thanh bookmark: hiện khi bật trong cài đặt hoặc đang ở tab mới (như Chrome).
    /// Chỉ dựng lại nút khi danh sách bookmark thay đổi.
    fn refresh_bookmarks_bar(&self, on_newtab: bool) {
        let mut list: Vec<(String, String)> =
            self.engine.db.bookmarks().into_iter().map(|e| (e.url, e.title)).collect();
        list.reverse(); // cũ nhất bên trái, như Chrome
        let show = !list.is_empty() && (self.engine.prefs.borrow().bookmarks_bar || on_newtab);
        self.bm_scroll.set_visible(show);
        if *self.bm_cache.borrow() == list {
            return;
        }
        while let Some(c) = self.bm_bar.first_child() {
            self.bm_bar.remove(&c);
        }
        for (url, title) in &list {
            let text = if title.trim().is_empty() { pretty_url(url) } else { title.clone() };
            let label = gtk::Label::new(Some(&text));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(22);
            let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let icon = gtk::Image::from_icon_name("web-browser-symbolic");
            icon.set_pixel_size(14);
            inner.append(&icon);
            inner.append(&label);
            let btn = gtk::Button::new();
            btn.set_child(Some(&inner));
            btn.add_css_class("flat");
            btn.add_css_class("bookmark-item");
            btn.set_focusable(false);
            btn.set_tooltip_text(Some(&format!("{text}\n{url}")));
            let window = self.window.clone();
            let u = url.clone();
            btn.connect_clicked(move |_| {
                // Mở trong tab đang xem qua thanh địa chỉ (dùng chung đường xử lý URL).
                let _ = WidgetExt::activate_action(&window, "win.open-url", Some(&u.to_variant()));
            });
            let middle = gtk::GestureClick::new();
            middle.set_button(2);
            let (window, u) = (self.window.clone(), url.clone());
            middle.connect_pressed(move |_, _, _, _| {
                let _ = WidgetExt::activate_action(&window, "win.open-url-background", Some(&u.to_variant()));
            });
            btn.add_controller(middle);
            self.bm_bar.append(&btn);
        }
        *self.bm_cache.borrow_mut() = list;
    }

    fn show_update(&self, state: &UpdateState) {
        let cur = crate::updater::current_version();
        let (text, primary, secondary): (String, Option<&str>, bool) = match state {
            UpdateState::Available(r) => (
                format!("Đã có Chỏme {} (bạn đang dùng {cur}).", r.version),
                Some(if crate::updater::installed_from_package() && r.deb_url.is_some() { "Cập nhật" } else { "Tải về" }),
                true,
            ),
            UpdateState::Working(t) => (t.clone(), None, false),
            UpdateState::Done(v) => (format!("Đã cài Chỏme {v}. Khởi động lại để dùng bản mới."), Some("Khởi động lại"), false),
            UpdateState::Error(t, _) => (format!("Không cập nhật được: {t}."), Some("Tải thủ công"), false),
            UpdateState::UpToDate => (format!("Bạn đang dùng bản mới nhất ({cur})."), None, false),
        };
        self.update_label.set_text(&text);
        self.update_primary.set_visible(primary.is_some());
        if let Some(p) = primary {
            self.update_primary.set_label(p);
        }
        self.update_secondary.set_visible(secondary);
        self.update_bar.set_reveal_child(true);
    }

    fn show_tab_menu(self: &Rc<Self>, tab: &Rc<Tab>) {
        *self.ctx_tab.borrow_mut() = Some(Rc::downgrade(tab));
        let muted = tab.view().is_some_and(|v| v.is_muted());
        let menu = gio::Menu::new();
        let s1 = gio::Menu::new();
        s1.append(Some("Tab mới bên phải"), Some("win.tab-new-right"));
        s1.append(Some(if tab.pinned.get() { "Bỏ ghim tab" } else { "Ghim tab" }), Some("win.tab-pin"));
        menu.append_section(None, &s1);
        let s2 = gio::Menu::new();
        s2.append(Some("Tải lại"), Some("win.tab-reload"));
        s2.append(Some("Nhân bản"), Some("win.tab-duplicate"));
        s2.append(Some(if muted { "Bật tiếng trang web" } else { "Tắt tiếng trang web" }), Some("win.tab-mute"));
        menu.append_section(None, &s2);
        let s3 = gio::Menu::new();
        s3.append(Some("Đóng"), Some("win.tab-close"));
        s3.append(Some("Đóng các tab khác"), Some("win.tab-close-others"));
        s3.append(Some("Đóng các tab bên phải"), Some("win.tab-close-right"));
        menu.append_section(None, &s3);
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&tab.button);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        // Gỡ popover sau khi hành động đã chạy (hành động cần popover còn trong cây widget).
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    fn ctx_tab(&self) -> Option<Rc<Tab>> {
        self.ctx_tab.borrow().as_ref().and_then(|w| w.upgrade())
    }

    fn toggle_mute(&self, tab: &Rc<Tab>) {
        if let Some(v) = tab.view() {
            v.set_is_muted(!v.is_muted());
            self.sync_audio(tab, &v);
        }
    }

    fn sync_audio(&self, tab: &Rc<Tab>, v: &WebView) {
        if v.is_playing_audio() {
            tab.last_audio.set(Some(Instant::now()));
        }
        let muted = v.is_muted();
        tab.audio.set_visible(muted || v.is_playing_audio());
        tab.audio.set_icon_name(if muted { "audio-volume-muted-symbolic" } else { "audio-volume-high-symbolic" });
        tab.audio.set_tooltip_text(Some(if muted { "Bật tiếng tab" } else { "Tắt tiếng tab" }));
    }

    /// Kéo thả để sắp xếp lại tab.
    fn setup_tab_dnd(self: &Rc<Self>, tab: &Rc<Tab>) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let id = tab.id;
        source.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&format!("lite-tab:{id}").to_value())));
        let btn = tab.button.clone();
        source.connect_drag_begin(move |src, _| {
            src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&btn))), 20, 16);
        });
        tab.button.add_controller(source);

        let target = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
        let (wb, wt) = (Rc::downgrade(self), Rc::downgrade(tab));
        target.connect_drop(move |_, value, x, _| {
            let (Some(b), Some(dest)) = (wb.upgrade(), wt.upgrade()) else { return false };
            let Some(src_id) = value
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("lite-tab:").and_then(|n| n.parse::<u64>().ok()))
            else {
                return false;
            };
            if src_id == dest.id {
                return false;
            }
            let after = x > dest.button.width() as f64 / 2.0;
            b.move_tab(src_id, &dest, after);
            true
        });
        tab.button.add_controller(target);
    }

    fn move_tab(&self, src_id: u64, dest: &Rc<Tab>, after: bool) {
        let mut tabs = self.tabs.borrow_mut();
        let Some(from) = tabs.iter().position(|t| t.id == src_id) else { return };
        if tabs[from].pinned.get() != dest.pinned.get() {
            return; // chỉ sắp xếp trong cùng dải (ghim / thường)
        }
        let src = tabs.remove(from);
        let Some(to) = tabs.iter().position(|t| Rc::ptr_eq(t, dest)) else {
            tabs.insert(from, src);
            return;
        };
        let pos = if after { to + 1 } else { to };
        tabs.insert(pos, src.clone());
        let prev = pos
            .checked_sub(1)
            .map(|i| tabs[i].clone())
            .filter(|p| p.pinned.get() == src.pinned.get())
            .map(|p| p.button.clone());
        drop(tabs);
        let strip = if src.pinned.get() { &self.pinned_strip } else { &self.strip };
        strip.reorder_child_after(&src.button, prev.as_ref());
    }

    fn start_sleeper(self: &Rc<Self>) {
        let low_mb = self.engine.config.low_memory_mb;
        let weak = Rc::downgrade(self);
        let first = self.engine.sleep_after();
        let interval = if first.is_zero() { 10 } else { first.as_secs().clamp(1, 10) as u32 };
        glib::timeout_add_seconds_local(interval, move || {
            let Some(b) = weak.upgrade() else { return glib::ControlFlow::Break };
            let limit = b.engine.sleep_after(); // đọc lại mỗi lần: đổi trong Cài đặt có hiệu lực ngay
            let mut awake: Vec<_> = b
                .tabs
                .borrow()
                .iter()
                .filter(|t| t.view().is_some() && !b.is_current(t) && !t.pinned.get())
                .filter(|t| !t.view().is_some_and(|v| v.is_playing_audio()))
                // Vừa phát âm thanh (vd. nhạc YouTube tạm dừng) trong 30 phút qua → giữ thức.
                .filter(|t| t.last_audio.get().is_none_or(|a| a.elapsed() > std::time::Duration::from_secs(30 * 60)))
                .cloned()
                .collect();
            // 1) Ngủ theo thời gian không dùng.
            if !limit.is_zero() {
                awake.retain(|t| {
                    let idle = t.last_active.get().elapsed() >= limit;
                    if idle {
                        b.sleep(t);
                    }
                    !idle
                });
            }
            // 2) Máy sắp hết RAM → cho tab nền ngủ, tab lâu không dùng nhất trước.
            if mem_available_mb().is_some_and(|m| m < low_mb) && !awake.is_empty() {
                awake.sort_by_key(|t| t.last_active.get());
                let n = awake.len().div_ceil(2);
                for t in awake.iter().take(n) {
                    b.sleep(t);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fn make_view(self: &Rc<Self>, tab: &Rc<Tab>, related: Option<&WebView>) -> WebView {
        let e = &self.engine;
        let content = if self.private.is_some() { &e.private_content } else { &e.content };
        let builder = WebView::builder()
            .settings(&e.settings)
            .user_content_manager(content);
        let view = match related {
            Some(r) => builder.related_view(r).build(),
            None => builder
                .web_context(&e.context)
                .network_session(self.private.as_ref().unwrap_or(&e.session))
                .build(),
        };
        view.set_vexpand(true);
        view.set_hexpand(true);
        let zoom = self.engine.zoom();
        if (zoom - 1.0).abs() > f64::EPSILON {
            view.set_zoom_level(zoom);
        }
        set_content(&tab.root, &view);
        tab.group.set(next_group());
        *tab.view.borrow_mut() = Some(view.clone());
        self.connect_view(tab, &view);
        view
    }

    fn connect_view(self: &Rc<Self>, tab: &Rc<Tab>, view: &WebView) {
        let wb = Rc::downgrade(self);
        let wt = Rc::downgrade(tab);
        let both = move || -> Option<(Rc<Browser>, Rc<Tab>)> { Some((wb.upgrade()?, wt.upgrade()?)) };

        let f = both.clone();
        view.connect_title_notify(move |v| {
            let Some((b, t)) = f() else { return };
            let title = v.title().map(|s| s.to_string()).unwrap_or_default();
            if title.is_empty() {
                return;
            }
            t.label.set_text(&title);
            t.button.set_tooltip_text(Some(&title));
            *t.title.borrow_mut() = title.clone();
            if let Some(u) = v.uri().filter(|u| is_web(u)) {
                if b.private.is_none() {
                    b.engine.db.set_title(&u, &title);
                }
            }
            if b.is_current(&t) {
                b.window.set_title(Some(&format!("{title} — Chỏme")));
            }
        });

        let f = both.clone();
        view.connect_uri_notify(move |v| {
            let Some((b, t)) = f() else { return };
            if let Some(u) = v.uri() {
                *t.url.borrow_mut() = u.to_string();
            }
            if b.is_current(&t) {
                b.sync_ui();
            }
        });

        let f = both.clone();
        view.connect_load_changed(move |v, ev| {
            let Some((b, t)) = f() else { return };
            if ev == LoadEvent::Committed {
                if let Some(u) = v.uri().filter(|u| is_web(u)) {
                    let title = v.title().map(|s| s.to_string()).unwrap_or_default();
                    if b.private.is_none() {
                        b.engine.db.add_visit(&u, &title);
                    }
                }
            }
            if b.is_current(&t) {
                b.sync_ui();
            }
        });

        let f = both.clone();
        view.connect_is_loading_notify(move |v| {
            let Some((b, t)) = f() else { return };
            let loading = v.is_loading();
            t.spinner.set_visible(loading);
            t.icon.set_visible(!loading);
            if loading {
                t.spinner.start();
            } else {
                t.spinner.stop();
            }
            if b.is_current(&t) {
                b.sync_ui();
            }
        });

        let f = both.clone();
        view.connect_is_playing_audio_notify(move |v| {
            if let Some((b, t)) = f() {
                b.sync_audio(&t, v);
            }
        });

        let f = both.clone();
        view.connect_estimated_load_progress_notify(move |v| {
            let Some((b, t)) = f() else { return };
            if b.is_current(&t) {
                b.sync_progress(v);
            }
        });

        let f = both.clone();
        view.connect_favicon_notify(move |v| {
            let Some((_, t)) = f() else { return };
            let internal = v.uri().is_some_and(|u| u.starts_with("chome://"));
            match v.favicon() {
                Some(tex) if !internal => t.icon.set_paintable(Some(&tex)),
                _ if internal => t.icon.set_icon_name(Some(crate::APP_ID)),
                _ => t.icon.set_icon_name(Some("web-browser-symbolic")),
            }
        });

        // window.open / target=_blank → tab mới dùng chung web process.
        let f = both.clone();
        view.connect_create(move |v, _action| {
            let (b, opener) = f()?;
            let tab = b.add_tab("", "", Open::Related(v.clone(), opener.group.get()));
            tab.view().map(|nv| nv.upcast())
        });

        // Ctrl+click / click chuột giữa vào link → mở tab nền.
        let f = both.clone();
        view.connect_decide_policy(move |_, decision, kind| {
            let Some((b, _)) = f() else { return false };
            match kind {
                PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                    let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                        return false;
                    };
                    let Some(action) = nav.navigation_action() else { return false };
                    let ctrl = gdk::ModifierType::from_bits_truncate(action.modifiers())
                        .contains(gdk::ModifierType::CONTROL_MASK);
                    let middle = action.mouse_button() == 2;
                    if action.navigation_type() == NavigationType::LinkClicked && (ctrl || middle) {
                        if let Some(uri) = action.request().and_then(|r| r.uri()) {
                            b.add_tab(&uri, "", Open::Background);
                            decision.ignore();
                            return true;
                        }
                    }
                    false
                }
                PolicyDecisionType::Response => {
                    let Some(resp) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                        return false;
                    };
                    if resp.is_main_frame_main_resource() && !resp.is_mime_type_supported() {
                        decision.download();
                        return true;
                    }
                    false
                }
                _ => false,
            }
        });

        if let Some(inspector) = view.inspector() {
            let f = both.clone();
            inspector.connect_attach(move |ins| {
                let (Some((_, t)), Some(dev)) = (f(), ins.web_view()) else { return false };
                t.devtools_open.set(true);
                Browser::dock_devtools(&t, dev.upcast_ref())
            });
            let f = both.clone();
            inspector.connect_detach(move |ins| {
                let Some((_, t)) = f() else { return false };
                let dev = ins.web_view().map(|d| d.upcast::<gtk::Widget>());
                Browser::undock_devtools(&t, dev.as_ref());
                true
            });
            let f = both.clone();
            inspector.connect_open_window(move |_| {
                if std::env::var_os("CHOME_DEBUG_JS").is_some() {
                    eprintln!("[js] tín hiệu: open-window");
                }
                if let Some((_, t)) = f() {
                    t.devtools_open.set(true);
                }
                false // để WebKit tự mở cửa sổ rời
            });
            let f = both.clone();
            inspector.connect_closed(move |ins| {
                let Some((_, t)) = f() else { return };
                t.devtools_open.set(false);
                let dev = ins.web_view().map(|d| d.upcast::<gtk::Widget>());
                Browser::undock_devtools(&t, dev.as_ref());
            });
        }

        let f = both.clone();
        view.connect_permission_request(move |v, req| {
            let Some((b, _)) = f() else { return false };
            let what = if req.is::<webkit6::GeolocationPermissionRequest>() {
                "biết vị trí của bạn"
            } else if req.is::<webkit6::NotificationPermissionRequest>() {
                "hiển thị thông báo"
            } else if let Some(m) = req.downcast_ref::<webkit6::UserMediaPermissionRequest>() {
                match (m.is_for_audio_device(), m.is_for_video_device()) {
                    (true, true) => "dùng micro và camera",
                    (true, false) => "dùng micro",
                    _ => "dùng camera",
                }
            } else if req.is::<webkit6::ClipboardPermissionRequest>() {
                "đọc clipboard"
            } else {
                return false; // để WebKit tự xử lý (mặc định từ chối)
            };
            let host = v
                .uri()
                .and_then(|u| glib::Uri::parse(&u, glib::UriFlags::NONE).ok())
                .and_then(|u| u.host())
                .map(|h| h.to_string())
                .unwrap_or_default();
            let dialog = gtk::AlertDialog::builder()
                .message(format!("{host} muốn {what}"))
                .buttons(["Chặn", "Cho phép"])
                .cancel_button(0)
                .default_button(1)
                .build();
            let req = req.clone();
            dialog.choose(Some(&b.window), None::<&gio::Cancellable>, move |res| {
                if res == Ok(1) {
                    req.allow();
                } else {
                    req.deny();
                }
            });
            true
        });

        let f = both.clone();
        view.connect_mouse_target_changed(move |_, hit, _| {
            let Some((b, t)) = f() else { return };
            if !b.is_current(&t) {
                return;
            }
            match hit.link_uri() {
                Some(link) if hit.context_is_link() => {
                    b.status.set_text(&link);
                    b.status.set_visible(true);
                }
                _ => b.status.set_visible(false),
            }
        });

        let f = both.clone();
        view.connect_enter_fullscreen(move |_| {
            if let Some((b, _)) = f() {
                b.titlebar.set_visible(false);
                b.toolbar.set_visible(false);
            }
            false
        });
        let f = both.clone();
        view.connect_leave_fullscreen(move |_| {
            if let Some((b, _)) = f() {
                b.titlebar.set_visible(true);
                b.toolbar.set_visible(true);
            }
            false
        });

        let f = both.clone();
        view.connect_close(move |_| {
            if let Some((b, t)) = f() {
                b.close_tab(&t);
            }
        });

        // Web process chết (crash / vượt giới hạn RAM): đưa tab về trạng thái ngủ,
        // nếu là tab đang xem thì hiện thông báo để người dùng tải lại.
        let f = both.clone();
        view.connect_web_process_terminated(move |v, reason| {
            let Some((b, t)) = f() else { return };
            if reason == WebProcessTerminationReason::TerminatedByApi {
                return;
            }
            if b.is_current(&t) {
                let msg = if reason == WebProcessTerminationReason::ExceededMemoryLimit {
                    "Trang dùng quá nhiều bộ nhớ nên đã bị dừng."
                } else {
                    "Trang đã bị lỗi và dừng hoạt động."
                };
                let uri = v.uri().map(|u| u.to_string()).unwrap_or_default();
                let html = format!(
                    "<html><body style=\"font:16px system-ui;text-align:center;padding-top:20vh\">\
                     <h2>😵 {msg}</h2><p><a href=\"{0}\">Tải lại trang</a></p></body></html>",
                    pages::escape(&uri)
                );
                v.load_alternate_html(&html, &uri, None);
            } else {
                *t.saved_state.borrow_mut() = None;
                if let Some(old) = t.view.borrow_mut().take() {
                    t.root.remove(&old);
                }
                set_content(&t.root, &placeholder("💤"));
                t.button.add_css_class("sleeping");
            }
        });

        let f = both;
        if let Some(fc) = view.find_controller() {
            let g = f.clone();
            fc.connect_found_text(move |_, n| {
                if let Some((b, t)) = g() {
                    if b.is_current(&t) {
                        b.find_label.set_text(&format!("{n} kết quả"));
                    }
                }
            });
            fc.connect_failed_to_find_text(move |_| {
                if let Some((b, t)) = f() {
                    if b.is_current(&t) {
                        b.find_label.set_text("Không tìm thấy");
                    }
                }
            });
        }
    }

    // ================= Đồng bộ giao diện =================

    fn sync_progress(&self, v: &WebView) {
        let p = v.estimated_load_progress();
        let show = v.is_loading() && p < 1.0;
        self.progress.set_visible(show);
        self.progress.set_fraction(p);
    }

    fn sync_ui(&self) {
        let Some(tab) = self.current() else { return };
        self.refresh_bookmarks_bar(tab.current_url() == NEWTAB);
        let url = tab.current_url();
        if !self.url_editing.get() {
            let text = if url == NEWTAB {
                String::new()
            } else if self.url_focused.get() {
                url.clone()
            } else {
                pretty_url(&url)
            };
            self.set_url_text(&text);
        }
        let title = tab.title.borrow().clone();
        let win_title = if title.is_empty() { "Chỏme".to_string() } else { format!("{title} — Chỏme") };
        self.window.set_title(Some(&win_title));
        let view = tab.view();
        self.back_btn.set_sensitive(view.as_ref().is_some_and(|v| v.can_go_back()));
        self.fwd_btn.set_sensitive(view.as_ref().is_some_and(|v| v.can_go_forward()));
        let loading = view.as_ref().is_some_and(|v| v.is_loading());
        self.reload_btn.set_icon_name(if loading { "process-stop-symbolic" } else { "view-refresh-symbolic" });
        if let Some(v) = &view {
            self.sync_progress(v);
        }
        // Biểu tượng trong ô địa chỉ: ổ khóa bên trái, ngôi sao bên phải (giống Chrome).
        use gtk::EntryIconPosition::{Primary, Secondary};
        let tls_bad = view.as_ref().and_then(|v| v.tls_info()).is_some_and(|(_, f)| !f.is_empty());
        let (icon, tip) = if url == NEWTAB || url.is_empty() || url.starts_with("chome://") {
            ("system-search-symbolic", "Tìm kiếm hoặc nhập địa chỉ")
        } else if url.starts_with("https://") && !tls_bad {
            ("channel-secure-symbolic", "Kết nối an toàn")
        } else if url.starts_with("https://") {
            ("dialog-warning-symbolic", "Chứng chỉ không hợp lệ — kết nối không an toàn")
        } else if url.starts_with("http://") {
            ("channel-insecure-symbolic", "Không an toàn — kết nối không được mã hóa")
        } else {
            ("text-x-generic-symbolic", "Tệp cục bộ")
        };
        self.url_entry.set_icon_from_icon_name(Primary, Some(icon));
        self.url_entry.set_icon_tooltip_text(Primary, Some(tip));
        if is_web(&url) {
            let starred = self.engine.db.is_bookmarked(&url);
            self.url_entry
                .set_icon_from_icon_name(Secondary, Some(if starred { "starred-symbolic" } else { "non-starred-symbolic" }));
            self.url_entry.set_icon_tooltip_text(
                Secondary,
                Some(if starred { "Xóa bookmark (Ctrl+D)" } else { "Bookmark trang này (Ctrl+D)" }),
            );
        } else {
            self.url_entry.set_icon_from_icon_name(Secondary, None);
        }
    }

    fn set_url_text(&self, text: &str) {
        self.setting_url.set(true);
        self.url_entry.set_text(text);
        self.setting_url.set(false);
    }

    fn navigate(self: &Rc<Self>, input: &str) {
        let url = self.engine.resolve_input(input);
        self.hide_suggestions();
        match self.current_view() {
            Some(v) => {
                v.load_uri(&url);
                v.grab_focus();
            }
            None => {
                self.add_tab(&url, "", Open::Foreground);
            }
        }
    }

    // ================= Thanh địa chỉ + gợi ý =================

    fn setup_passwords(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.password_save.connect_clicked(move |_| {
            let Some(b) = weak.upgrade() else { return };
            if let Some(credential) = b.password_pending.borrow_mut().take() {
                let _ = b.engine.passwords.save(&credential);
            }
            b.password_pop.popdown();
        });
        let weak = Rc::downgrade(self);
        self.password_never.connect_clicked(move |_| {
            let Some(b) = weak.upgrade() else { return };
            if let Some(credential) = b.password_pending.borrow_mut().take() {
                b.engine.passwords.add_never(&credential.origin);
            }
            b.password_pop.popdown();
        });

        // Handler được dùng chung cho mọi WebView thường; chỉ cửa sổ đang hiển thị đúng
        // origin mới được phép hiện popover. WebView ẩn danh dùng content manager khác.
        let weak = Rc::downgrade(self);
        self.engine.content.connect_script_message_received(Some(passwords::SAVE_HANDLER), move |_, value| {
            let Some(b) = weak.upgrade() else { return };
            let Some(json) = passwords::message_json(value) else { return };
            let Some(Message::Credentials(credential)) = passwords::parse_message(&json) else { return };
            if std::env::var_os("CHOME_DEBUG_JS").is_some() {
                eprintln!(
                    "[js] nhận thông tin đăng nhập từ {} | cửa sổ đang dùng: {} | ẩn danh: {} | kho khóa: {}",
                    credential.origin,
                    b.window.is_active(),
                    b.private.is_some(),
                    b.engine.passwords.available()
                );
            }
            if b.private.is_some()
                // Nhiều cửa sổ: chỉ cửa sổ đang dùng hỏi (nếu không cửa sổ nào đang được focus thì
                // cửa sổ nào đang mở đúng trang đó sẽ hỏi).
                || (!b.window.is_active() && Browser::all().iter().any(|w| w.window.is_active()))
                || !b.engine.prefs.borrow().passwords
                || !b.engine.passwords.available()
                || !credential.origin.starts_with("http")
                || b.engine.passwords.is_never(&credential.origin)
            {
                return;
            }
            let current = b.current().map(|t| t.current_url()).unwrap_or_default();
            if passwords::url_origin(&current).as_deref() != Some(credential.origin.as_str()) {
                return;
            }
            let saved_user = b.engine.passwords.lookup_user(&credential.origin, &credential.username);
            if saved_user.as_ref().is_some_and(|saved| saved.password == credential.password) {
                return;
            }
            let host = passwords::origin_host(&credential.origin);
            let heading = if saved_user.is_some() {
                format!("Cập nhật mật khẩu cho {host}?")
            } else {
                format!("Lưu mật khẩu cho {host}?")
            };
            b.password_heading.set_text(&heading);
            b.password_user.set_text(if credential.username.is_empty() { "Tên người dùng không có" } else { &credential.username });
            *b.password_pending.borrow_mut() = Some(credential);
            b.password_pop.popup();
        });
    }

    fn setup_url_entry(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.url_entry.connect_activate(move |e| {
            let Some(b) = weak.upgrade() else { return };
            let picked = b
                .suggest_list
                .selected_row()
                .filter(|_| b.suggest_pop.is_visible())
                .and_then(|r| b.suggestions.borrow().get(r.index() as usize).cloned());
            let input = picked.unwrap_or_else(|| e.text().to_string());
            b.url_editing.set(false);
            b.navigate(&input);
        });

        let weak = Rc::downgrade(self);
        self.url_entry.connect_changed(move |e| {
            let Some(b) = weak.upgrade() else { return };
            if b.setting_url.get() {
                return;
            }
            b.url_editing.set(true);
            b.update_suggestions(&e.text());
        });

        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_enter(move |_| {
            let Some(b) = weak.upgrade() else { return };
            b.url_focused.set(true);
            if !b.url_editing.get() {
                b.sync_ui(); // hiện URL đầy đủ khi đang sửa
                let e = b.url_entry.clone();
                glib::idle_add_local_once(move || e.select_region(0, -1));
            }
        });
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            let Some(b) = weak.upgrade() else { return };
            b.url_focused.set(false);
            b.url_editing.set(false);
            b.hide_suggestions();
            b.sync_ui();
        });
        self.url_entry.add_controller(focus);

        // Bấm vào ô địa chỉ lần đầu → chọn hết chữ (như Chrome), không đặt con trỏ.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, _, _| {
            let Some(b) = weak.upgrade() else { return };
            if !b.url_focused.get() {
                g.set_state(gtk::EventSequenceState::Claimed);
                b.url_entry.grab_focus();
            }
        });
        self.url_entry.add_controller(click);

        let weak = Rc::downgrade(self);
        self.url_entry.connect_icon_press(move |_, pos| {
            if pos == gtk::EntryIconPosition::Secondary {
                if let Some(b) = weak.upgrade() {
                    let _ = gtk::prelude::WidgetExt::activate_action(&b.window, "win.bookmark", None);
                }
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(b) = weak.upgrade() else { return glib::Propagation::Proceed };
            let n = b.suggestions.borrow().len() as i32;
            let open = b.suggest_pop.is_visible() && n > 0;
            let cur = b.suggest_list.selected_row().map(|r| r.index()).unwrap_or(-1);
            let go = |i: i32| {
                match b.suggest_list.row_at_index(i) {
                    Some(row) => b.suggest_list.select_row(Some(&row)),
                    None => b.suggest_list.unselect_all(),
                }
                glib::Propagation::Stop
            };
            match key {
                gdk::Key::Down if open => go((cur + 1).min(n - 1)),
                gdk::Key::Up if open => go(cur - 1),
                gdk::Key::Escape => {
                    b.url_editing.set(false);
                    b.hide_suggestions();
                    b.sync_ui();
                    if let Some(v) = b.current_view() {
                        v.grab_focus();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.url_entry.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.suggest_list.connect_row_activated(move |_, row| {
            let Some(b) = weak.upgrade() else { return };
            let url = b.suggestions.borrow().get(row.index() as usize).cloned();
            if let Some(url) = url {
                b.url_editing.set(false);
                b.navigate(&url);
            }
        });
    }

    fn update_suggestions(&self, text: &str) {
        while let Some(c) = self.suggest_list.first_child() {
            self.suggest_list.remove(&c);
        }
        if text.trim().is_empty() {
            self.hide_suggestions();
            return;
        }
        let row = |icon: &str, main: &str, sub: &str| {
            let img = gtk::Image::from_icon_name(icon);
            img.add_css_class("dim-label");
            let a = gtk::Label::new(Some(main));
            a.set_xalign(0.0);
            a.set_ellipsize(gtk::pango::EllipsizeMode::End);
            a.set_max_width_chars(60);
            let b = gtk::Label::new(Some(sub));
            b.set_xalign(0.0);
            b.set_hexpand(true);
            b.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.add_css_class("dim-label");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            line.append(&img);
            line.append(&a);
            line.append(&b);
            let r = gtk::ListBoxRow::new();
            r.set_child(Some(&line));
            r.set_focusable(false);
            r
        };
        // Dòng đầu luôn là chính nội dung đang gõ (tìm kiếm hoặc mở địa chỉ).
        let resolved = self.engine.resolve_input(text);
        let is_search = resolved.starts_with(&self.engine.search_url());
        let first = if is_search {
            row("system-search-symbolic", text.trim(), &format!("— Tìm kiếm {}", self.engine.search_name()))
        } else {
            row("web-browser-symbolic", &resolved, "— Mở trang")
        };
        self.suggest_list.append(&first);
        let mut values = vec![text.to_string()];
        for e in self.engine.db.suggest(text, 6) {
            let title = if e.title.is_empty() { e.url.clone() } else { e.title.clone() };
            let icon = if self.engine.db.is_bookmarked(&e.url) { "starred-symbolic" } else { "document-open-recent-symbolic" };
            self.suggest_list.append(&row(icon, &title, &format!("— {}", pretty_url(&e.url))));
            values.push(e.url);
        }
        *self.suggestions.borrow_mut() = values;
        if let Some(r) = self.suggest_list.row_at_index(0) {
            self.suggest_list.select_row(Some(&r));
        }
        self.suggest_pop.set_size_request(self.url_entry.width().max(300), -1);
        if !self.suggest_pop.is_visible() {
            self.suggest_pop.popup();
        }
    }

    fn hide_suggestions(&self) {
        self.suggestions.borrow_mut().clear();
        self.suggest_list.unselect_all();
        if self.suggest_pop.is_visible() {
            self.suggest_pop.popdown();
        }
    }

    // ================= Tìm trong trang =================

    fn setup_find_bar(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button, close: &gtk::Button) {
        let opts = (webkit6::FindOptions::CASE_INSENSITIVE | webkit6::FindOptions::WRAP_AROUND).bits();
        let weak = Rc::downgrade(self);
        self.find_entry.connect_search_changed(move |e| {
            let Some(b) = weak.upgrade() else { return };
            let Some(fc) = b.current_view().and_then(|v| v.find_controller()) else { return };
            let text = e.text();
            if text.is_empty() {
                fc.search_finish();
                b.find_label.set_text("");
            } else {
                fc.search(&text, opts, 1000);
            }
        });
        let step = |b: &Browser, forward: bool| {
            if let Some(fc) = b.current_view().and_then(|v| v.find_controller()) {
                if forward {
                    fc.search_next();
                } else {
                    fc.search_previous();
                }
            }
        };
        let weak = Rc::downgrade(self);
        self.find_entry.connect_activate(move |_| {
            if let Some(b) = weak.upgrade() {
                step(&b, true);
            }
        });
        for (btn, forward) in [(next, true), (prev, false)] {
            let weak = Rc::downgrade(self);
            btn.connect_clicked(move |_| {
                if let Some(b) = weak.upgrade() {
                    step(&b, forward);
                }
            });
        }
        for (sig_next, forward) in [(true, true), (false, false)] {
            let weak = Rc::downgrade(self);
            let handler = move |_: &gtk::SearchEntry| {
                if let Some(b) = weak.upgrade() {
                    step(&b, forward);
                }
            };
            if sig_next {
                self.find_entry.connect_next_match(handler);
            } else {
                self.find_entry.connect_previous_match(handler);
            }
        }
        let weak = Rc::downgrade(self);
        self.find_entry.connect_stop_search(move |_| {
            if let Some(b) = weak.upgrade() {
                b.hide_find();
                if let Some(v) = b.current_view() {
                    v.grab_focus();
                }
            }
        });
        let weak = Rc::downgrade(self);
        close.connect_clicked(move |_| {
            if let Some(b) = weak.upgrade() {
                b.hide_find();
            }
        });
    }

    fn show_find(&self) {
        self.find_bar.set_reveal_child(true);
        self.find_entry.grab_focus();
        let text = self.find_entry.text();
        if !text.is_empty() {
            self.find_entry.select_region(0, -1);
            self.find_entry.emit_by_name::<()>("search-changed", &[]);
        }
    }

    fn hide_find(&self) {
        if !self.find_bar.reveals_child() {
            return;
        }
        self.find_bar.set_reveal_child(false);
        self.find_label.set_text("");
        if let Some(fc) = self.current_view().and_then(|v| v.find_controller()) {
            fc.search_finish();
        }
    }

    // ================= Hành động & phím tắt =================

    fn add_action(self: &Rc<Self>, name: &str, f: impl Fn(&Rc<Browser>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(b) = weak.upgrade() {
                f(&b);
            }
        });
        self.window.add_action(&action);
    }

    fn setup_actions(self: &Rc<Self>) {
        self.add_action("new-tab", |b| {
            b.add_tab(NEWTAB, "Tab mới", Open::Foreground);
        });
        self.add_action("new-window", |b| {
            if let Some(app) = b.window.application() {
                Browser::new(&app, b.engine.clone(), Startup::Urls(vec![]));
            }
        });
        self.add_action("close-tab", |b| {
            if let Some(t) = b.current() {
                b.close_tab(&t);
            }
        });
        self.add_action("reopen-tab", |b| {
            let url = b.closed.borrow_mut().pop();
            if let Some(url) = url {
                b.add_tab(&url, "", Open::Foreground);
            }
        });
        self.add_action("next-tab", |b| b.cycle_tab(1));
        self.add_action("prev-tab", |b| b.cycle_tab(-1));
        self.add_action("back", |b| {
            if let Some(v) = b.current_view() {
                v.go_back();
            }
        });
        self.add_action("forward", |b| {
            if let Some(v) = b.current_view() {
                v.go_forward();
            }
        });
        self.add_action("reload", |b| {
            if let Some(v) = b.current_view() {
                v.reload();
            }
        });
        self.add_action("hard-reload", |b| {
            if let Some(v) = b.current_view() {
                v.reload_bypass_cache();
            }
        });
        self.add_action("reload-or-stop", |b| {
            if let Some(v) = b.current_view() {
                if v.is_loading() {
                    v.stop_loading();
                } else {
                    v.reload();
                }
            }
        });
        self.add_action("focus-url", |b| {
            b.url_entry.grab_focus();
        });
        self.add_action("find", |b| b.show_find());
        self.add_action("bookmark", |b| {
            let Some(t) = b.current() else { return };
            let url = t.current_url();
            if is_web(&url) {
                b.engine.db.toggle_bookmark(&url, &t.title.borrow());
                b.sync_ui();
            }
        });
        self.add_action("history", |b| {
            b.add_tab("chome://history", "Lịch sử", Open::Foreground);
        });
        self.add_action("bookmarks", |b| {
            b.add_tab("chome://bookmarks", "Bookmark", Open::Foreground);
        });
        self.add_action("passwords", |b| {
            let existing = b.tabs.borrow().iter().find(|t| t.current_url().starts_with("chome://passwords")).cloned();
            match existing {
                Some(t) => b.select(&t),
                None => { b.add_tab("chome://passwords", "Mật khẩu", Open::Foreground); }
            }
        });
        let zoom = |factor: Option<f64>| {
            move |b: &Rc<Browser>| {
                if let Some(v) = b.current_view() {
                    let z = factor.map(|f| (v.zoom_level() * f).clamp(0.3, 5.0)).unwrap_or(1.0);
                    v.set_zoom_level(z);
                }
            }
        };
        self.add_action("zoom-in", zoom(Some(1.1)));
        self.add_action("zoom-out", zoom(Some(1.0 / 1.1)));
        self.add_action("zoom-reset", zoom(None));
        self.add_action("devtools", |b| b.toggle_devtools(true));
        self.add_action("devtools-open", |b| b.toggle_devtools(false));
        self.add_action("settings", |b| {
            // Đã có tab Cài đặt thì chuyển sang tab đó (như Chrome).
            let existing = b.tabs.borrow().iter().find(|t| t.current_url().starts_with("chome://settings")).cloned();
            match existing {
                Some(t) => b.select(&t),
                None => {
                    b.add_tab("chome://settings", "Cài đặt", Open::Foreground);
                }
            }
        });
        self.add_action("check-update", |_| run_update_check(true));
        self.add_action("update-primary", |b| {
            let Some(state) = UPDATE.with(|u| u.borrow().clone()) else { return };
            match state {
                UpdateState::Available(r) => {
                    if crate::updater::installed_from_package() && r.deb_url.is_some() {
                        glib::MainContext::default().spawn_local(install_update(r));
                    } else {
                        b.add_tab(&r.page_url, "Chỏme — bản phát hành", Open::Foreground);
                    }
                }
                UpdateState::Done(_) => {
                    crate::updater::relaunch_after_exit();
                    let _ = WidgetExt::activate_action(&b.window, "app.quit", None);
                }
                UpdateState::Error(_, page) => {
                    b.add_tab(&page, "Chỏme — bản phát hành", Open::Foreground);
                }
                _ => {}
            }
        });
        self.add_action("update-notes", |b| {
            if let Some(UpdateState::Available(r)) = UPDATE.with(|u| u.borrow().clone()) {
                b.add_tab(&r.page_url, "Chỏme — có gì mới", Open::Foreground);
            }
        });
        self.add_action("downloads", |b| {
            let existing = b.tabs.borrow().iter().find(|t| t.current_url().starts_with("chome://downloads")).cloned();
            match existing {
                Some(t) => {
                    b.select(&t);
                    if let Some(v) = t.view() {
                        v.reload();
                    }
                }
                None => {
                    b.add_tab("chome://downloads", "Tải xuống", Open::Foreground);
                }
            }
        });
        self.add_action("new-incognito", |b| {
            if let Some(app) = b.window.application() {
                Browser::new(&app, b.engine.clone(), Startup::Incognito);
            }
        });
        self.add_action("translate", |b| {
            let Some(t) = b.current() else { return };
            let url = t.current_url();
            if !is_web(&url) {
                return;
            }
            // Google Dịch mở trang qua proxy dịch (giống nút "Dịch" của Chrome).
            let target = format!(
                "https://translate.google.com/translate?sl=auto&tl=vi&u={}",
                glib::Uri::escape_string(&url, None, false)
            );
            if let Some(v) = t.view() {
                v.load_uri(&target);
            }
        });
        self.add_action("fullscreen", |b| {
            if b.window.is_fullscreen() {
                b.window.unfullscreen();
            } else {
                b.window.fullscreen();
            }
        });
        self.add_action("print", |b| {
            if let Some(v) = b.current_view() {
                webkit6::PrintOperation::new(&v).run_dialog(Some(&b.window));
            }
        });
        self.add_action("sleep-all", |b| {
            let tabs: Vec<_> = b.tabs.borrow().clone();
            for t in tabs {
                b.sleep(&t);
            }
        });

        self.add_action("tab-new-right", |b| {
            if let Some(t) = b.ctx_tab() {
                b.select(&t);
                b.add_tab(NEWTAB, "Tab mới", Open::Background);
                let next = b.index_of(&t).and_then(|i| b.tabs.borrow().get(i + 1).cloned());
                if let Some(n) = next {
                    b.select(&n);
                }
            }
        });
        self.add_action("tab-reload", |b| {
            if let Some(v) = b.ctx_tab().and_then(|t| t.view()) {
                v.reload();
            }
        });
        self.add_action("tab-duplicate", |b| {
            if let Some(t) = b.ctx_tab() {
                b.select(&t);
                let url = t.current_url();
                b.add_tab(&url, &t.title.borrow(), Open::Background);
            }
        });
        self.add_action("tab-pin", |b| {
            if let Some(t) = b.ctx_tab() {
                b.set_pinned(&t, !t.pinned.get());
            }
        });
        self.add_action("tab-mute", |b| {
            if let Some(t) = b.ctx_tab() {
                b.toggle_mute(&t);
            }
        });
        self.add_action("tab-close", |b| {
            if let Some(t) = b.ctx_tab() {
                b.close_tab(&t);
            }
        });
        let close_where = |keep: fn(usize, usize) -> bool| {
            move |b: &Rc<Browser>| {
                let Some(t) = b.ctx_tab() else { return };
                let Some(i) = b.index_of(&t) else { return };
                b.select(&t);
                let victims: Vec<_> = b
                    .tabs
                    .borrow()
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| !keep(*j, i))
                    .map(|(_, x)| x.clone())
                    .collect();
                for v in victims {
                    b.close_tab(&v);
                }
            }
        };
        self.add_action("tab-close-others", close_where(|j, i| j == i));
        self.add_action("tab-close-right", close_where(|j, i| j <= i));

        for (name, background) in [("open-url", false), ("open-url-background", true)] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, param| {
                let (Some(b), Some(url)) = (weak.upgrade(), param.and_then(|p| p.get::<String>())) else { return };
                if background {
                    b.add_tab(&url, "", Open::Background);
                } else {
                    b.navigate(&url);
                }
            });
            self.window.add_action(&action);
        }
        self.add_action("bookmarks-bar", |b| {
            let on = !b.engine.prefs.borrow().bookmarks_bar;
            b.engine.apply_setting("bookmarks_bar", if on { "1" } else { "0" });
            for w in Browser::all() {
                w.sync_ui();
            }
        });

        let select = gio::SimpleAction::new("select-tab", Some(glib::VariantTy::INT32));
        let weak = Rc::downgrade(self);
        select.connect_activate(move |_, param| {
            let (Some(b), Some(n)) = (weak.upgrade(), param.and_then(|p| p.get::<i32>())) else {
                return;
            };
            let tab = {
                let tabs = b.tabs.borrow();
                if n >= 9 { tabs.last().cloned() } else { tabs.get((n - 1).max(0) as usize).cloned() }
            };
            if let Some(t) = tab {
                b.select(&t);
            }
        });
        self.window.add_action(&select);

        let adblock = gio::SimpleAction::new_stateful("adblock", None, &self.engine.adblock_enabled().to_variant());
        let weak = Rc::downgrade(self);
        adblock.connect_change_state(move |a, state| {
            let (Some(b), Some(on)) = (weak.upgrade(), state.and_then(|s| s.get::<bool>())) else {
                return;
            };
            a.set_state(&on.to_variant());
            b.engine.set_adblock(on);
            if let Some(v) = b.current_view() {
                v.reload();
            }
        });
        self.window.add_action(&adblock);
    }

    fn cycle_tab(self: &Rc<Self>, delta: i32) {
        let Some(cur) = self.current() else { return };
        let Some(i) = self.index_of(&cur) else { return };
        let next = {
            let tabs = self.tabs.borrow();
            let n = tabs.len() as i32;
            tabs[((i as i32 + delta).rem_euclid(n)) as usize].clone()
        };
        self.select(&next);
    }

    fn setup_window_events(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |w| {
            if let Some(b) = weak.upgrade() {
                // Chỉ cửa sổ cuối cùng mới ghi lại phiên để mở lại lần sau.
                let last = b.private.is_none() && w.application().map(|a| a.windows().len() <= 1).unwrap_or(true);
                if last {
                    let tabs = b.tabs.borrow();
                    let session: Vec<_> = tabs
                        .iter()
                        .map(|t| (t.current_url(), t.title.borrow().clone(), t.pinned.get()))
                        .filter(|(u, _, _)| u != NEWTAB && !u.is_empty())
                        .collect();
                    let active = b
                        .current()
                        .and_then(|c| {
                            let url = c.current_url();
                            session.iter().position(|(u, _, _)| *u == url)
                        })
                        .unwrap_or(0);
                    b.engine.db.save_session(&session, active);
                    // Lưu cookie phiên trước khi thoát; giữ app sống tới khi ghi xong.
                    if let Some(app) = w.application() {
                        let guard = app.hold();
                        let engine = b.engine.clone();
                        glib::MainContext::default().spawn_local(async move {
                            engine.save_session_cookies().await;
                            drop(guard);
                        });
                    }
                }
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.window.connect_destroy(move |_| {
            let Some(b) = weak.upgrade() else { return };
            b.suggest_pop.unparent();
            b.tabs.borrow_mut().clear();
            *b.current.borrow_mut() = None;
            WINDOWS.with(|w| w.borrow_mut().retain(|x| !Rc::ptr_eq(x, &b)));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::pretty_url;

    #[test]
    fn pretty_url_like_chrome() {
        assert_eq!(pretty_url("https://vnexpress.net/"), "vnexpress.net");
        assert_eq!(pretty_url("https://www.google.com/"), "google.com");
        assert_eq!(pretty_url("https://github.com/servo/servo"), "github.com/servo/servo");
        assert_eq!(pretty_url("https://vi.wikipedia.org/wiki/Vi%E1%BB%87t_Nam"), "vi.wikipedia.org/wiki/Việt_Nam");
        assert_eq!(pretty_url("http://localhost:8080/a/"), "localhost:8080/a/");
        assert_eq!(pretty_url("chome://history"), "chome://history");
        assert_eq!(pretty_url("chome://settings/"), "chome://settings");
    }
}

// ================= Cập nhật =================

fn set_update_state(state: UpdateState) {
    UPDATE.with(|u| *u.borrow_mut() = Some(state.clone()));
    for b in Browser::all() {
        b.show_update(&state);
    }
}

/// Kiểm tra bản mới. `manual` = người dùng bấm "Kiểm tra cập nhật" (báo cả khi không có bản mới).
fn run_update_check(manual: bool) {
    glib::MainContext::default().spawn_local(async move {
        if manual {
            set_update_state(UpdateState::Working("Đang kiểm tra cập nhật…".into()));
        }
        match crate::updater::check().await {
            Ok(Some(release)) => set_update_state(UpdateState::Available(release)),
            Ok(None) if manual => {
                set_update_state(UpdateState::UpToDate);
                glib::timeout_add_seconds_local_once(5, || {
                    if matches!(UPDATE.with(|u| u.borrow().clone()), Some(UpdateState::UpToDate)) {
                        for b in Browser::all() {
                            b.update_bar.set_reveal_child(false);
                        }
                    }
                });
            }
            Ok(None) => {}
            Err(e) if manual => set_update_state(UpdateState::Error(e, "https://github.com/LILLINH/chome/releases".into())),
            Err(e) => eprintln!("[update] {e}"),
        }
    });
}

async fn install_update(release: crate::updater::Release) {
    set_update_state(UpdateState::Working(format!("Đang tải Chỏme {}…", release.version)));
    let deb = match crate::updater::download(&release).await {
        Ok(p) => p,
        Err(e) => return set_update_state(UpdateState::Error(e, release.page_url)),
    };
    set_update_state(UpdateState::Working("Đang cài đặt — hệ thống sẽ hỏi mật khẩu của bạn…".into()));
    match crate::updater::install(&deb).await {
        Ok(()) => set_update_state(UpdateState::Done(release.version)),
        Err(e) => set_update_state(UpdateState::Error(e, release.page_url)),
    }
}

/// Tự kiểm tra cập nhật 15 giây sau khi mở, tối đa mỗi ngày một lần.
pub fn schedule_update_check(engine: Rc<Engine>) {
    glib::timeout_add_seconds_local_once(15, move || {
        if !engine.prefs.borrow().update_check || std::env::var_os("CHOME_NON_UNIQUE").is_some() {
            return; // tắt trong cài đặt, hoặc bản chạy thử
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let last: u64 = engine.db.get_setting("update_checked_at").and_then(|v| v.parse().ok()).unwrap_or(0);
        if now.saturating_sub(last) < 24 * 3600 {
            return;
        }
        engine.db.set_setting("update_checked_at", &now.to_string());
        run_update_check(false);
    });
}
