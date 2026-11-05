//! Trang nội bộ `chome://…`: tab mới, lịch sử, bookmark.
//! Được phục vụ trực tiếp từ bộ nhớ, không chạm mạng.

use crate::engine::{Engine, SEARCH_ENGINES};
use crate::storage::{Entry, Storage};
use webkit6::glib;

pub const NEWTAB: &str = "chome://newtab";

/// Logo trên trang tab mới (icon app 128px), nhúng thẳng vào HTML.
fn logo_base64() -> &'static str {
    use std::sync::OnceLock;
    static LOGO: OnceLock<String> = OnceLock::new();
    LOGO.get_or_init(|| base64(include_bytes!("../assets/logo-128.png")))
}

pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Chỉ cho phép link tới các scheme an toàn (tránh `javascript:` trong dữ liệu đã lưu).
fn safe_href(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if ["http://", "https://", "file://", "chome://"].iter().any(|p| lower.starts_with(p)) {
        escape(url)
    } else {
        "#".into()
    }
}

fn display_title(e: &Entry) -> String {
    escape(if e.title.trim().is_empty() { &e.url } else { &e.title })
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

const CSS: &str = r#"
:root { color-scheme: light dark; --bg:#f6f7f9; --card:#fff; --fg:#1f2328; --muted:#6b7280;
        --line:#e5e7eb; --accent:#2563eb; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#1b1c1f; --card:#25262b; --fg:#e8e8ea; --muted:#9a9ca3; --line:#33353a; --accent:#7aa2ff; }
}
* { box-sizing: border-box; }
body { margin:0; font:15px/1.5 system-ui, sans-serif; background:var(--bg); color:var(--fg); }
main { max-width: 860px; margin: 0 auto; padding: 48px 20px; }
h1 { font-size: 22px; margin: 0 0 20px; }
a { color: inherit; text-decoration: none; }
.logo { display:flex; flex-direction:column; align-items:center; gap:10px; margin: 9vh 0 24px; }
.logo img { width:96px; height:96px; }
.logo span { font-size:26px; font-weight:700; letter-spacing:.01em; }
.logo small { color: var(--muted); margin-top:-6px; }
.search { display:flex; margin: 0 0 40px; }
.search input { flex:1; font-size:17px; padding:14px 20px; border-radius:999px; border:1px solid var(--line);
                background:var(--card); color:var(--fg); outline:none; }
.search input:focus { border-color: var(--accent); }
.grid { display:grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap:12px; }
.tile { background:var(--card); border:1px solid var(--line); border-radius:12px; padding:14px; overflow:hidden; }
.tile:hover { border-color: var(--accent); }
.tile b { display:block; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
.tile span { color:var(--muted); font-size:12px; }
ul { list-style:none; padding:0; margin:0; background:var(--card); border:1px solid var(--line); border-radius:12px; }
li { display:flex; gap:12px; align-items:center; padding:10px 14px; border-top:1px solid var(--line); }
li:first-child { border-top:0; }
li .t { flex:1; min-width:0; }
li .t a { display:block; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
li .t small { color:var(--muted); display:block; white-space:nowrap; overflow:hidden; text-overflow:ellipsis; }
li time { color:var(--muted); font-size:12px; white-space:nowrap; }
.del { color:var(--muted); padding: 0 6px; }
.del:hover { color:#dc2626; }
.bar { display:flex; justify-content:space-between; align-items:center; margin-bottom:16px; }
.btn { border:1px solid var(--line); border-radius:8px; padding:6px 12px; background:var(--card); }
.btn:hover { border-color:#dc2626; color:#dc2626; }
h2 { font-size:13px; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); margin:28px 0 10px; }
.empty { color: var(--muted); }
"#;

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"vi\"><head><meta charset=\"utf-8\"><title>{}</title>\
         <style>{CSS}</style></head><body><main>{body}</main></body></html>",
        escape(title)
    )
}

fn tiles(entries: &[Entry]) -> String {
    let mut s = String::from("<div class=\"grid\">");
    for e in entries {
        s += &format!(
            "<a class=\"tile\" href=\"{}\"><b>{}</b><span>{}</span></a>",
            safe_href(&e.url),
            display_title(e),
            escape(host(&e.url))
        );
    }
    s + "</div>"
}

/// Trang tab mới trong cửa sổ ẩn danh (giống Chrome: nền tối, giải thích những gì không lưu).
fn incognito_tab(search_url: &str, search_name: &str) -> String {
    let body = format!(
        "<div class=\"incog\"><div class=\"big\">🕶</div><h1>Bạn đang ẩn danh</h1>\
         <p>Chỏme <b>không lưu</b>: lịch sử duyệt web, cookie và dữ liệu trang web, thông tin đã nhập trong biểu mẫu. \
         Tất cả bị xóa khi bạn đóng các cửa sổ ẩn danh.</p>\
         <p>Vẫn <b>có thể thấy</b> hoạt động của bạn: trang web bạn truy cập, nơi làm việc/trường học, nhà mạng. \
         File tải xuống và bookmark vẫn được giữ lại.</p>\
         <form class=\"search\" onsubmit=\"var q=this.q.value.trim();if(q)location.href='{}'+encodeURIComponent(q);return false\">\
         <input name=\"q\" autofocus placeholder=\"Tìm kiếm {} hoặc nhập địa chỉ\" autocomplete=\"off\"></form></div>",
        escape(&search_url.replace('\\', "\\\\").replace('\'', "\\'")),
        escape(search_name)
    );
    format!(
        "<!doctype html><html lang=\"vi\"><head><meta charset=\"utf-8\"><title>Tab ẩn danh mới</title>\
         <style>{CSS} body {{ background:#202124; color:#e8eaed; }} .incog {{ max-width:620px; margin:12vh auto 0; }}\
         .incog .big {{ font-size:56px; }} .incog p {{ color:#bdc1c6; line-height:1.7; }}\
         .search input {{ background:#303134; color:#e8eaed; border-color:#5f6368; }}</style></head><body><main>{body}</main></body></html>"
    )
}

pub fn newtab(db: &Storage, search_url: &str, search_name: &str) -> String {
    // Tìm bằng JS để hỗ trợ mọi kiểu URL công cụ tìm kiếm (kể cả dạng #query= của Cốc Cốc).
    let mut body = format!(
        "<div class=\"logo\"><img src=\"data:image/png;base64,{}\" alt=\"\"><span>Chỏme</span><small>Trình duyệt nhẹ, tiết kiệm RAM</small></div>\
         <form class=\"search\" onsubmit=\"var q=this.q.value.trim();if(q)location.href='{}'+encodeURIComponent(q);return false\">\
         <input name=\"q\" autofocus placeholder=\"Tìm kiếm {} hoặc nhập địa chỉ\" autocomplete=\"off\"></form>",
        logo_base64(),
        escape(&search_url.replace('\\', "\\\\").replace('\'', "\\'")),
        escape(search_name)
    );
    let bms: Vec<Entry> = db.bookmarks().into_iter().take(12).collect();
    if !bms.is_empty() {
        body += "<h2>Bookmark</h2>";
        body += &tiles(&bms);
    }
    let top = db.top_sites(8);
    if !top.is_empty() {
        body += "<h2>Hay truy cập</h2>";
        body += &tiles(&top);
    }
    page("Tab mới", &body)
}

fn fmt_time(ts: i64) -> String {
    glib_time(ts).unwrap_or_default()
}

fn glib_time(ts: i64) -> Option<String> {
    let dt = webkit6::glib::DateTime::from_unix_local(ts).ok()?;
    dt.format("%d/%m/%Y %H:%M").ok().map(|s| s.to_string())
}

pub fn history(db: &Storage) -> String {
    let items = db.recent_history(500);
    let mut body = String::from(
        "<div class=\"bar\"><h1>Lịch sử</h1>\
         <a class=\"btn\" href=\"chome://history?clear=all\">Xóa toàn bộ lịch sử</a></div>",
    );
    if items.is_empty() {
        body += "<p class=\"empty\">Chưa có lịch sử.</p>";
    } else {
        body += "<ul>";
        for (e, ts) in &items {
            body += &format!(
                "<li><div class=\"t\"><a href=\"{}\">{}</a><small>{}</small></div>\
                 <time>{}</time><a class=\"del\" title=\"Xóa\" href=\"chome://history?del={}\">✕</a></li>",
                safe_href(&e.url),
                display_title(e),
                escape(&e.url),
                fmt_time(*ts),
                e.id
            );
        }
        body += "</ul>";
    }
    page("Lịch sử", &body)
}

pub fn bookmarks(db: &Storage) -> String {
    let items = db.bookmarks();
    let mut body = String::from("<h1>Bookmark</h1>");
    if items.is_empty() {
        body += "<p class=\"empty\">Chưa có bookmark. Nhấn Ctrl+D hoặc ☆ trên thanh địa chỉ để thêm.</p>";
    } else {
        body += "<ul>";
        for e in &items {
            body += &format!(
                "<li><div class=\"t\"><a href=\"{}\">{}</a><small>{}</small></div>\
                 <a class=\"del\" title=\"Xóa\" href=\"chome://bookmarks?del={}\">✕</a></li>",
                safe_href(&e.url),
                display_title(e),
                escape(&e.url),
                e.id
            );
        }
        body += "</ul>";
    }
    page("Bookmark", &body)
}

fn downloads_page(engine: &Engine) -> String {
    use crate::downloads::{State, human_size};
    let running: Vec<_> = engine.downloads.items().into_iter().filter(|i| i.state.get() == State::Running).collect();
    let history = engine.db.downloads(300);
    let mut body = String::from(
        "<div class=\"bar\"><h1>Tải xuống</h1>\
         <a class=\"btn\" href=\"chome://downloads?clear=all\">Xóa danh sách</a></div>",
    );
    if running.is_empty() && history.is_empty() {
        body += "<p class=\"empty\">Chưa có tệp nào được tải xuống.</p>";
    } else {
        body += "<ul>";
        for it in &running {
            let pct = it.fraction().map(|f| format!("{:.0}%", f * 100.0)).unwrap_or_default();
            body += &format!(
                "<li><div class=\"t\"><b>{}</b><small>Đang tải {pct} · {}</small></div></li>",
                escape(&it.filename.borrow()),
                escape(&it.status_text())
            );
        }
        for (id, url, path, name, size, state, ts) in &history {
            let exists = std::path::Path::new(path).exists();
            let status = match (state.as_str(), exists) {
                ("done", true) => human_size(*size as u64),
                ("done", false) => "Đã bị xóa".into(),
                ("cancelled", _) => "Đã hủy".into(),
                _ => "Lỗi tải xuống".into(),
            };
            let actions = if state == "done" && exists {
                format!(
                    "<a class=\"btn\" href=\"chome://downloads?open={id}\">Mở</a>\
                     <a class=\"btn\" href=\"chome://downloads?folder={id}\">Hiện trong thư mục</a>"
                )
            } else {
                String::new()
            };
            body += &format!(
                "<li><div class=\"t\"><b>{}</b><small>{} · {} · <a href=\"{}\">{}</a></small></div>{actions}\
                 <a class=\"del\" title=\"Xóa khỏi danh sách\" href=\"chome://downloads?del={id}\">✕</a></li>",
                escape(name),
                escape(&status),
                fmt_time(*ts),
                safe_href(url),
                escape(&truncate_middle(url, 70))
            );
        }
        body += "</ul>";
    }
    // Đang có tệp tải dở → tự làm mới mỗi giây để thấy tiến trình.
    let refresh = if running.is_empty() { "" } else { "<meta http-equiv=\"refresh\" content=\"1\">" };
    format!(
        "<!doctype html><html lang=\"vi\"><head><meta charset=\"utf-8\">{refresh}<title>Tải xuống</title>\
         <style>{CSS} li .btn {{ white-space:nowrap; font-size:13px; }}</style></head><body><main>{body}</main></body></html>"
    )
}

fn truncate_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max / 2).collect();
    let tail: String = s.chars().skip(n - max / 2).collect();
    format!("{head}…{tail}")
}

pub fn passwords(engine: &Engine) -> String {
    let items = engine.passwords.list();
    let mut body = String::from("<div class=\"bar\"><h1>Mật khẩu</h1></div>");
    if !engine.passwords.available() {
        body.push_str("<p class=\"empty\">Secret Service chưa khả dụng nên Chỏme không thể lưu hoặc đọc mật khẩu.</p>");
    } else if items.is_empty() {
        body.push_str("<p class=\"empty\">Chưa có mật khẩu đã lưu.</p>");
    } else {
        body.push_str("<ul>");
        for item in items {
            let origin = escape(&item.origin);
            let username = escape(&item.username);
            let href = format!(
                "chome://passwords?del_origin={}&del_username={}",
                glib::Uri::escape_string(&item.origin, None, false),
                glib::Uri::escape_string(&item.username, None, false)
            );
            body.push_str(&format!(
                "<li><div class=\"t\"><b>{origin}</b><small>{username}</small></div><a class=\"del\" title=\"Xóa\" href=\"{href}\">✕</a></li>"
            ));
        }
        body.push_str("</ul>");
    }
    body.push_str("<p class=\"empty\">Mật khẩu được lưu trong kho khóa của hệ thống; Chỏme không ghi mật khẩu vào SQLite.</p>");
    page("Mật khẩu", &body)
}

/// Giải mã %XX trong tham số truy vấn (giá trị cài đặt chỉ là chữ/số ASCII).
fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| {
            glib::Uri::unescape_string(v, None).map(|s| s.to_string()).unwrap_or_else(|| v.to_string())
        }))
}

const SETTINGS_CSS: &str = r#"
main { max-width: 1060px; display:flex; gap:32px; align-items:flex-start; }
nav { position: sticky; top: 24px; width: 220px; flex-shrink:0; }
nav h1 { display:flex; align-items:center; gap:10px; }
nav h1 img { width:32px; height:32px; border-radius:8px; }
nav a { display:block; padding:9px 16px; border-radius:0 999px 999px 0; color:var(--fg); margin-left:-16px; }
nav a:hover { background: color-mix(in srgb, var(--accent) 10%, transparent); }
.content { flex:1; min-width:0; }
section { margin-bottom: 28px; scroll-margin-top: 24px; }
section h2 { margin: 0 0 10px; }
.card { background:var(--card); border:1px solid var(--line); border-radius:12px; }
.row { display:flex; align-items:center; justify-content:space-between; gap:16px; padding:14px 18px; border-top:1px solid var(--line); }
.row:first-child { border-top:0; }
.row .label { min-width:0; }
.row .label b { font-weight:500; display:block; }
.row .label small { color:var(--muted); }
select { font:inherit; padding:7px 10px; border-radius:8px; border:1px solid var(--line); background:var(--card); color:var(--fg); }
.switch { position:relative; width:40px; height:22px; flex-shrink:0; }
.switch input { opacity:0; width:0; height:0; }
.switch span { position:absolute; inset:0; background:var(--line); border-radius:999px; transition:.15s; cursor:pointer; }
.switch span::before { content:""; position:absolute; width:16px; height:16px; left:3px; top:3px; background:#fff; border-radius:50%; transition:.15s; box-shadow:0 1px 2px rgba(0,0,0,.3); }
.switch input:checked + span { background: var(--accent); }
.switch input:checked + span::before { transform: translateX(18px); }
.btn.primary { border-color: var(--accent); color: var(--accent); cursor:pointer; }
pre { background: color-mix(in srgb, var(--fg) 6%, transparent); padding: 10px 12px; border-radius: 8px; margin: 8px 0 0; white-space: pre-wrap; font-size: 13px; }
.saved { position:fixed; bottom:20px; left:50%; transform:translateX(-50%); background:#323232; color:#fff; padding:10px 18px; border-radius:8px; animation: fade 2.2s forwards; }
@keyframes fade { 0%,70% { opacity:1 } 100% { opacity:0; visibility:hidden } }
@media (max-width: 720px) { nav { display:none; } }
"#;

fn row(title: &str, hint: &str, control: &str) -> String {
    format!(
        "<div class=\"row\"><div class=\"label\"><b>{title}</b><small>{hint}</small></div>{control}</div>"
    )
}

fn select(key: &str, current: &str, options: &[(&str, &str)]) -> String {
    let mut s = format!("<select onchange=\"setPref('{key}', this.value)\">");
    for (value, label) in options {
        let sel = if *value == current { " selected" } else { "" };
        s += &format!("<option value=\"{}\"{sel}>{}</option>", escape(value), escape(label));
    }
    s + "</select>"
}

fn switch(key: &str, on: bool) -> String {
    format!(
        "<label class=\"switch\"><input type=\"checkbox\"{} onchange=\"setPref('{key}', this.checked ? '1' : '0')\"><span></span></label>",
        if on { " checked" } else { "" }
    )
}

pub fn settings(engine: &Engine, saved: Option<&str>) -> String {
    let p = engine.prefs.borrow().clone();
    let zoom = format!("{}", p.zoom);
    let sleep = p.sleep_minutes.to_string();
    let searches: Vec<(&str, &str)> = SEARCH_ENGINES.iter().map(|(id, name, _)| (*id, *name)).collect();
    let downloads = webkit6::glib::user_special_dir(webkit6::glib::UserDirectory::Downloads)
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| "~/Downloads".into());
    let env_note = |set: bool| if set { " (đang bị ghi đè bởi biến môi trường)" } else { "" };

    let mut b = format!(
        "<nav><h1><img src=\"data:image/png;base64,{}\" alt=\"\">Cài đặt</h1>\
         <a href=\"#startup\">Khi khởi động</a><a href=\"#search\">Công cụ tìm kiếm</a>\
         <a href=\"#appearance\">Giao diện</a><a href=\"#privacy\">Quyền riêng tư và bảo mật</a>\
         <a href=\"#performance\">Hiệu suất</a><a href=\"#languages\">Ngôn ngữ</a><a href=\"#ai\">Kết nối AI (MCP)</a><a href=\"#downloads\">Tải xuống</a>\
         <a href=\"#about\">Giới thiệu</a></nav><div class=\"content\">",
        logo_base64()
    );
    b += "<section id=\"startup\"><h2>Khi khởi động</h2><div class=\"card\">";
    b += &row(
        "Khi mở trình duyệt",
        "Mở lại các tab của lần trước — tab chỉ tải khi bạn bấm vào",
        &select(
            "startup",
            if p.restore_session { "restore" } else { "newtab" },
            &[("restore", "Tiếp tục từ chỗ đã dừng"), ("newtab", "Mở trang Tab mới")],
        ),
    );
    b += "</div></section>";

    b += "<section id=\"search\"><h2>Công cụ tìm kiếm</h2><div class=\"card\">";
    b += &row(
        "Công cụ tìm kiếm dùng trong thanh địa chỉ",
        env_note(engine.config.search_url.is_some()),
        &select("search", &p.search, &searches),
    );
    b += "</div></section>";

    b += "<section id=\"appearance\"><h2>Giao diện</h2><div class=\"card\">";
    b += &row(
        "Chủ đề",
        "Sáng, tối hoặc theo hệ thống",
        &select("theme", &p.theme, &[("system", "Theo hệ thống"), ("light", "Sáng"), ("dark", "Tối")]),
    );
    b += &row(
        "Hiện thanh bookmark",
        "Luôn hiện thanh bookmark dưới thanh địa chỉ (Ctrl+Shift+B). Tắt thì chỉ hiện ở trang tab mới.",
        &switch("bookmarks_bar", p.bookmarks_bar),
    );
    b += &row(
        "Thu phóng trang",
        "Áp dụng cho các tab mở sau khi đổi",
        &select(
            "zoom",
            &zoom,
            &[("0.75", "75%"), ("0.9", "90%"), ("1", "100%"), ("1.1", "110%"), ("1.25", "125%"), ("1.5", "150%"), ("1.75", "175%")],
        ),
    );
    b += "</div></section>";

    b += "<section id=\"privacy\"><h2>Quyền riêng tư và bảo mật</h2><div class=\"card\">";
    b += &row("Chặn quảng cáo", &format!("Chặn quảng cáo và trình theo dõi phổ biến{}", env_note(engine.config.adblock.is_some())), &switch("adblock", engine.adblock_enabled()));
    b += &row(
        "YouTube không dừng",
        "Tự đóng hộp \"Video đã tạm dừng. Tiếp tục xem?\" để nghe nhạc không bị ngắt (YouTube và YouTube Music)",
        &switch("yt_nonstop", p.yt_nonstop),
    );
    b += &row("Chống theo dõi xuyên trang", "Hạn chế cookie của bên thứ ba dùng để theo dõi bạn", &switch("itp", p.itp));
    b += &row(
        "Xóa dữ liệu duyệt web",
        "Xóa lịch sử, cookie, dữ liệu trang web và bộ nhớ đệm (bạn sẽ bị đăng xuất khỏi các trang)",
        "<a class=\"btn primary\" onclick=\"if(confirm('Xóa toàn bộ dữ liệu duyệt web?'))location.href='chome://settings?clear=all'\">Xóa dữ liệu</a>",
    );
    b += &row("Lịch sử", "Xem và xóa từng mục", "<a class=\"btn\" href=\"chome://history\">Mở lịch sử</a>");
    b += "</div></section>";

    b += "<section id=\"passwords\"><h2>Mật khẩu</h2><div class=\"card\">";
    b += &row(
        "Đề nghị lưu mật khẩu",
        if engine.passwords.available() { "Hỏi trước khi lưu thông tin đăng nhập vào kho khóa của hệ thống" } else { "Secret Service chưa khả dụng — Chỏme sẽ không hỏi lưu mật khẩu" },
        &switch("passwords", p.passwords),
    );
    b += &row("Mật khẩu đã lưu", "Xem tên người dùng; mật khẩu luôn được ẩn", "<a class=\"btn\" href=\"chome://passwords\">Mở trang quản lý</a>");
    b += "</div></section>";

    b += "<section id=\"performance\"><h2>Hiệu suất</h2><div class=\"card\">";
    b += &row(
        "Tiết kiệm bộ nhớ",
        &format!(
            "Tab không dùng một thời gian sẽ ngủ để giải phóng RAM; bấm vào là tải lại{}",
            env_note(engine.config.sleep_after.is_some())
        ),
        &select(
            "sleep",
            &sleep,
            &[("0", "Tắt"), ("1", "Sau 1 phút"), ("5", "Sau 5 phút"), ("15", "Sau 15 phút"), ("30", "Sau 30 phút"), ("60", "Sau 1 giờ")],
        ),
    );
    b += &row(
        "Dùng tăng tốc phần cứng (GPU)",
        &format!("Bật (khuyên dùng): cuộn mượt như Chrome. Tắt: tiết kiệm ~20–35 MB mỗi trang nhưng cuộn bị giật{}", env_note(engine.config.gpu.is_some())),
        &switch("gpu", p.gpu),
    );
    b += "</div></section>";

    b += "<section id=\"languages\"><h2>Ngôn ngữ</h2><div class=\"card\">";
    b += &row(
        "Kiểm tra chính tả",
        if engine.has_vietnamese_dictionary() {
            "Gạch chân chữ viết sai khi gõ (tiếng Việt và tiếng Anh). Chuột phải vào chữ để xem gợi ý."
        } else {
            "Gạch chân chữ viết sai khi gõ. Hiện chỉ có tiếng Anh — cài tiếng Việt: sudo apt install hunspell-vi"
        },
        &switch("spell", p.spell),
    );
    b += &row(
        "Dịch trang",
        "Menu ⋮ → \"Dịch trang sang tiếng Việt\" (dùng Google Dịch)",
        "",
    );
    b += "</div></section>";

    let bridge = escape(&crate::mcp::bridge_command());
    b += "<section id=\"ai\"><h2>Kết nối AI (MCP)</h2><div class=\"card\">";
    b += &row(
        "Cho phép công cụ AI điều khiển trình duyệt",
        "Claude Code, Codex, Gemini CLI… có thể mở trang, đọc nội dung, bấm, gõ chữ. Chỉ chương trình chạy dưới tài khoản của bạn trên máy này mới kết nối được.",
        &switch("mcp", p.mcp),
    );
    b += &row(
        "Phạm vi",
        "Ở chế độ \"Chỉ tab do AI mở\", AI không thấy được các tab bạn đang dùng (email, ngân hàng…). Tab do AI mở có dấu 🤖.",
        &select("mcp_scope", &p.mcp_scope, &[("ai", "Chỉ tab do AI mở"), ("all", "Mọi tab")]),
    );
    b += &format!(
        "<div class=\"row\"><div class=\"label\"><b>Cách kết nối</b><small>Chạy lệnh tương ứng một lần trong terminal:</small>\
         <pre>Claude Code:  claude mcp add chome -- {bridge}\n\
Codex:        codex mcp add chome -- {bridge}\n\
              (không muốn bị hỏi mỗi lần: thêm default_tools_approval_mode = \"approve\" vào [mcp_servers.chome] trong ~/.codex/config.toml)\n\
Gemini CLI:   gemini mcp add chome {bridge}</pre></div></div>"
    );
    b += "</div></section>";

    b += "<section id=\"downloads\"><h2>Tải xuống</h2><div class=\"card\">";
    b += &row("Thư mục lưu", &escape(&downloads), "");
    b += "</div></section>";

    b += "<section id=\"about\"><h2>Giới thiệu</h2><div class=\"card\">";
    b += &row(
        "Chỏme",
        &format!(
            "Phiên bản {} · Engine WebKitGTK {}.{}.{}",
            env!("CARGO_PKG_VERSION"),
            webkit6::functions::major_version(),
            webkit6::functions::minor_version(),
            webkit6::functions::micro_version()
        ),
        "",
    );
    b += &row(
        "Tự kiểm tra cập nhật",
        "Mỗi ngày hỏi GitHub xem có bản mới không; có thì hiện thông báo để bạn cập nhật. Kiểm tra ngay: menu ⋮ → Kiểm tra cập nhật",
        &switch("update_check", p.update_check),
    );
    b += &row("Dữ liệu được lưu tại", &escape(&engine.data_dir.display().to_string()), "");
    b += "</div></section></div>";
    if let Some(msg) = saved {
        b += &format!("<div class=\"saved\">{}</div>", escape(msg));
    }
    b += "<script>function setPref(k,v){location.replace('chome://settings?set='+k+'&v='+encodeURIComponent(v)+location.hash)}</script>";

    format!(
        "<!doctype html><html lang=\"vi\"><head><meta charset=\"utf-8\"><title>Cài đặt</title>\
         <style>{CSS}{SETTINGS_CSS}</style></head><body><main>{b}</main></body></html>"
    )
}

/// Xử lý một yêu cầu `chome://…`, trả về HTML.
pub fn handle(engine: &Engine, uri: &str, incognito: bool) -> String {
    let db: &Storage = &engine.db;
    let rest = uri.strip_prefix("chome://").unwrap_or(uri);
    let rest = rest.split('#').next().unwrap_or(rest);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = path.trim_end_matches('/');
    let param = |key: &str| query_param(query, key);
    match path {
        "settings" => {
            let mut saved = None;
            if let (Some(k), Some(v)) = (param("set"), param("v")) {
                engine.apply_setting(&k, &v);
                saved = Some("Đã lưu cài đặt");
            } else if param("clear").is_some() {
                engine.clear_browsing_data();
                saved = Some("Đã xóa dữ liệu duyệt web");
            }
            settings(engine, saved)
        }
        "downloads" => {
            if let Some(id) = param("open").and_then(|v| v.parse().ok()) {
                if let Some(p) = db.download_path(id) {
                    crate::downloads::open_file(std::path::Path::new(&p));
                }
            } else if let Some(id) = param("folder").and_then(|v| v.parse().ok()) {
                if let Some(p) = db.download_path(id) {
                    crate::downloads::show_in_folder(std::path::Path::new(&p));
                }
            } else if let Some(id) = param("del").and_then(|v| v.parse().ok()) {
                db.delete_download(id);
            } else if param("clear").is_some() {
                db.clear_downloads();
                engine.downloads.clear_finished();
            }
            downloads_page(engine)
        }
        "history" => {
            if param("clear").is_some() {
                db.clear_history();
            } else if let Some(id) = param("del").and_then(|v| v.parse().ok()) {
                db.delete_history(id);
            }
            history(db)
        }
        "bookmarks" => {
            if let Some(id) = param("del").and_then(|v| v.parse().ok()) {
                db.delete_bookmark(id);
            }
            bookmarks(db)
        }
        "passwords" if !incognito => {
            if let (Some(origin), Some(username)) = (param("del_origin"), param("del_username")) {
                engine.passwords.delete(&origin, &username);
            }
            passwords(engine)
        }
        _ if incognito => incognito_tab(&engine.search_url(), engine.search_name()),
        _ => newtab(db, &engine.search_url(), engine.search_name()),
    }
}
