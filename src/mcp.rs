//! Máy chủ điều khiển cho công cụ AI (MCP) — phía trình duyệt.
//!
//! Chỏme lắng nghe trên một Unix socket trong `$XDG_RUNTIME_DIR/chome/` (thư mục 0700,
//! socket 0600 → chỉ chính tài khoản người dùng kết nối được). Chương trình cầu nối
//! `chome-mcp` nói giao thức MCP (stdio) với Claude Code / Codex / Gemini CLI và chuyển
//! từng lệnh sang đây dưới dạng một dòng JSON: `{"tool": "...", "args": {...}}`.
//! Trả về một dòng JSON: `{"ok": bool, "text": "...", "image": "<base64 PNG>"?}`.
//!
//! Mặc định TẮT; bật trong Cài đặt. Ở phạm vi "Chỉ tab do AI mở" (mặc định), AI không
//! thấy và không chạm được vào các tab người dùng tự mở.

use crate::browser::{Browser, Tab};
use crate::engine::Engine;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};
use webkit6::{gio, glib, prelude::*, WebView};

thread_local! {
    static ENGINE: RefCell<Weak<Engine>> = RefCell::new(Weak::new());
    static SERVER: RefCell<Option<gio::SocketService>> = const { RefCell::new(None) };
    static LAST_ACTIVITY: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Sau bấy lâu không có lệnh nào thì ẩn dấu báo "🤖 AI".
const BADGE_IDLE: Duration = Duration::from_secs(15);

pub fn socket_path() -> PathBuf {
    std::env::var_os("CHOME_MCP_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::user_runtime_dir().join("chome").join("mcp.sock"))
}

/// Đường dẫn chương trình cầu nối, để hiển thị hướng dẫn kết nối trong Cài đặt.
pub fn bridge_command() -> String {
    if std::path::Path::new("/usr/bin/chome-mcp").exists() {
        return "chome-mcp".into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("chome-mcp")))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "chome-mcp".into())
}

pub fn init(engine: &Rc<Engine>) {
    ENGINE.with(|e| *e.borrow_mut() = Rc::downgrade(engine));
    let on = std::env::var("LITE_MCP").map(|v| v == "1").unwrap_or(engine.prefs.borrow().mcp);
    set_enabled(on);
}

pub fn is_running() -> bool {
    SERVER.with(|s| s.borrow().is_some())
}

pub fn set_enabled(on: bool) {
    if on {
        if let Err(e) = start() {
            eprintln!("[mcp] không mở được máy chủ: {e}");
        }
    } else {
        stop();
    }
}

fn start() -> Result<(), String> {
    if is_running() {
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path();
    // Linux giới hạn đường dẫn Unix socket ~108 byte; dài hơn sẽ bị cắt âm thầm.
    if path.as_os_str().len() > 100 {
        return Err(format!("đường dẫn socket quá dài: {}", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let _ = std::fs::remove_file(&path); // socket cũ còn sót
    let service = gio::SocketService::new();
    let addr = gio::UnixSocketAddress::new(&path);
    service
        .add_address(&addr, gio::SocketType::Stream, gio::SocketProtocol::Default, None::<&glib::Object>)
        .map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    service.connect_incoming(|_, conn, _| {
        glib::MainContext::default().spawn_local(serve(conn.clone()));
        false
    });
    service.start();
    SERVER.with(|s| *s.borrow_mut() = Some(service));
    eprintln!("[mcp] đang lắng nghe tại {}", path.display());
    Ok(())
}

pub fn stop() {
    if let Some(service) = SERVER.with(|s| s.borrow_mut().take()) {
        service.stop();
        service.close();
        let _ = std::fs::remove_file(socket_path());
    }
    show_badge(false);
}

async fn serve(conn: gio::SocketConnection) {
    let input = gio::DataInputStream::new(&conn.input_stream());
    let output = conn.output_stream();
    loop {
        let line = match input.read_line_utf8_future(glib::Priority::DEFAULT).await {
            Ok(Some(l)) => l.to_string(),
            _ => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = handle(&line).await;
        let mut out = reply.to_string();
        out.push('\n');
        if output.write_all_future(out.into_bytes(), glib::Priority::DEFAULT).await.is_err() {
            break;
        }
    }
    drop(conn);
}

fn show_badge(on: bool) {
    for b in Browser::all() {
        b.set_ai_active(on);
    }
}

fn mark_activity() {
    LAST_ACTIVITY.with(|l| l.set(Some(Instant::now())));
    show_badge(true);
    glib::timeout_add_local_once(BADGE_IDLE + Duration::from_millis(200), || {
        let idle = LAST_ACTIVITY.with(|l| l.get()).is_none_or(|t| t.elapsed() >= BADGE_IDLE);
        if idle {
            show_badge(false);
        }
    });
}

enum Reply {
    Text(String),
    Image { png_base64: String, caption: String },
}

async fn handle(line: &str) -> Value {
    let Ok(req) = serde_json::from_str::<Value>(line) else {
        return json!({"ok": false, "text": "Yêu cầu không phải JSON hợp lệ"});
    };
    if !is_running() {
        return json!({"ok": false, "text": "Chỏme đang tắt kết nối AI (MCP). Bật trong Cài đặt → Kết nối AI."});
    }
    let tool = req["tool"].as_str().unwrap_or_default().to_string();
    let args = req.get("args").cloned().unwrap_or(json!({}));
    mark_activity();
    match run(&tool, &args).await {
        Ok(Reply::Text(text)) => json!({"ok": true, "text": text}),
        Ok(Reply::Image { png_base64, caption }) => json!({"ok": true, "text": caption, "image": png_base64}),
        Err(e) => json!({"ok": false, "text": e}),
    }
}

// ---------------------------------------------------------------------------

fn scope_all() -> bool {
    ENGINE
        .with(|e| e.borrow().upgrade())
        .is_some_and(|e| e.prefs.borrow().mcp_scope == "all")
}

fn allowed(tab: &Tab) -> bool {
    scope_all() || tab.is_ai()
}

const SCOPE_HINT: &str = "Tab này do người dùng mở nên AI không được phép truy cập. \
     Hãy dùng browser_open để mở tab riêng, hoặc người dùng có thể đổi phạm vi thành \
     \"Mọi tab\" trong Cài đặt → Kết nối AI.";

fn window() -> Result<Rc<Browser>, String> {
    Browser::first().ok_or_else(|| "Chỏme chưa mở cửa sổ nào".to_string())
}

/// Tab đích: theo `tab_id`, hoặc tab đang hiển thị. Kiểm tra quyền theo phạm vi.
fn target(args: &Value) -> Result<(Rc<Browser>, Rc<Tab>), String> {
    let (b, t) = match args.get("tab_id").and_then(Value::as_u64) {
        Some(id) => Browser::find_tab(id).ok_or_else(|| format!("Không có tab {id}"))?,
        None => {
            let b = window()?;
            let t = b.active_tab().ok_or("Không có tab nào")?;
            (b, t)
        }
    };
    if !allowed(&t) {
        return Err(SCOPE_HINT.into());
    }
    Ok((b, t))
}

/// WebView của tab (đánh thức nếu tab đang ngủ).
async fn view_of(b: &Rc<Browser>, t: &Rc<Tab>) -> Result<WebView, String> {
    if t.is_sleeping() {
        b.wake(t);
        if let Some(v) = t.web_view() {
            wait_load(&v).await;
        }
    }
    t.web_view().ok_or_else(|| "Tab chưa sẵn sàng".to_string())
}

async fn wait_load(view: &WebView) {
    let start = Instant::now();
    glib::timeout_future(Duration::from_millis(300)).await;
    while view.is_loading() && start.elapsed() < Duration::from_secs(20) {
        glib::timeout_future(Duration::from_millis(100)).await;
    }
}

async fn js(view: &WebView, script: &str) -> Result<String, String> {
    view.evaluate_javascript_future(script, None, None)
        .await
        .map(|v| v.to_str().to_string())
        .map_err(|e| format!("Lỗi JavaScript: {e}"))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("Thiếu tham số \"{key}\""))
}

fn list_tabs() -> String {
    let mut out = Vec::new();
    for b in Browser::all() {
        for t in b.tabs() {
            if !allowed(&t) {
                continue;
            }
            let active = if b.is_active(&t) { " (đang xem)" } else { "" };
            let sleeping = if t.is_sleeping() { " (đang ngủ)" } else { "" };
            let title = if t.title().is_empty() { "(chưa có tiêu đề)".into() } else { t.title() };
            out.push(format!("[{}] {title}{active}{sleeping}\n    {}", t.id(), t.url()));
        }
    }
    if out.is_empty() {
        if scope_all() {
            "Không có tab nào.".into()
        } else {
            "Chưa có tab nào do AI mở. Dùng browser_open để mở trang.".into()
        }
    } else {
        out.join("\n")
    }
}

async fn page_summary(view: &WebView) -> String {
    let title = view.title().map(|s| s.to_string()).unwrap_or_default();
    let url = view.uri().map(|s| s.to_string()).unwrap_or_default();
    format!("{title}\n{url}")
}

const ELEMENTS_JS: &str = r#"(() => {
  for (const el of document.querySelectorAll('[data-chome-id]')) el.removeAttribute('data-chome-id');
  const sel = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[onclick],[contenteditable=true]';
  const out = []; let n = 0;
  for (const el of document.querySelectorAll(sel)) {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) continue;
    const st = getComputedStyle(el);
    if (st.visibility === 'hidden' || st.display === 'none' || st.opacity === '0') continue;
    const id = ++n; el.setAttribute('data-chome-id', String(id));
    const tag = el.tagName.toLowerCase();
    let label = (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || el.title || el.getAttribute('alt') || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    let kind = tag;
    if (tag === 'input') kind = 'input[' + (el.type || 'text') + ']';
    const where = (r.bottom < 0 || r.top > innerHeight) ? ' (ngoài màn hình)' : '';
    const href = tag === 'a' && el.href ? ' → ' + el.href.slice(0, 120) : '';
    out.push('[' + id + '] ' + kind + ': ' + (label || '(không nhãn)') + href + where);
    if (n >= 300) { out.push('… (giới hạn 300 phần tử)'); break; }
  }
  return out.join('\n') || '(không có phần tử tương tác nào)';
})()"#;

async fn run(tool: &str, args: &Value) -> Result<Reply, String> {
    match tool {
        "browser_tabs" => Ok(Reply::Text(list_tabs())),

        "browser_open" => {
            let url = str_arg(args, "url")?;
            let b = window()?;
            let tab = b.open_ai_tab(url);
            let view = tab.web_view().ok_or("Không tạo được tab")?;
            wait_load(&view).await;
            Ok(Reply::Text(format!("Đã mở tab [{}]\n{}", tab.id(), page_summary(&view).await)))
        }

        "browser_navigate" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            match (args.get("url").and_then(Value::as_str), args.get("action").and_then(Value::as_str)) {
                (Some(url), _) => view.load_uri(&b.resolve(url)),
                (None, Some("back")) => view.go_back(),
                (None, Some("forward")) => view.go_forward(),
                (None, Some("reload")) => view.reload(),
                _ => return Err("Cần \"url\" hoặc \"action\" (back | forward | reload)".into()),
            }
            wait_load(&view).await;
            Ok(Reply::Text(format!("Tab [{}]\n{}", t.id(), page_summary(&view).await)))
        }

        "browser_select_tab" => {
            let (b, t) = target(args)?;
            b.select_tab(&t);
            b.window().present();
            Ok(Reply::Text(format!("Đang xem tab [{}] {}", t.id(), t.title())))
        }

        "browser_close_tab" => {
            if args.get("tab_id").is_none() {
                return Err("Cần \"tab_id\"".into());
            }
            let (b, t) = target(args)?;
            let id = t.id();
            b.close(&t);
            Ok(Reply::Text(format!("Đã đóng tab [{id}]")))
        }

        "browser_read_page" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            let max = args.get("max_chars").and_then(Value::as_u64).unwrap_or(20_000) as usize;
            let text = js(&view, "document.body ? document.body.innerText : ''").await?;
            let total = text.chars().count();
            let mut body: String = text.chars().take(max).collect();
            if total > max {
                body.push_str(&format!("\n… (đã cắt, trang có {total} ký tự; tăng max_chars để đọc thêm)"));
            }
            Ok(Reply::Text(format!("Tab [{}]\n{}\n\n{body}", t.id(), page_summary(&view).await)))
        }

        "browser_elements" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            Ok(Reply::Text(js(&view, ELEMENTS_JS).await?))
        }

        "browser_click" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            let id = args.get("element_id").and_then(Value::as_u64).ok_or("Cần \"element_id\" (lấy từ browser_elements)")?;
            let script = format!(
                r#"(() => {{ const el = document.querySelector('[data-chome-id="{id}"]');
                  if (!el) return 'Không thấy phần tử {id} — hãy gọi lại browser_elements';
                  el.scrollIntoView({{block: 'center'}}); el.focus && el.focus(); el.click(); return 'ok'; }})()"#
            );
            let r = js(&view, &script).await?;
            if r != "ok" {
                return Err(r);
            }
            wait_load(&view).await;
            Ok(Reply::Text(format!("Đã bấm phần tử {id}\n{}", page_summary(&view).await)))
        }

        "browser_type" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            let id = args.get("element_id").and_then(Value::as_u64).ok_or("Cần \"element_id\" (lấy từ browser_elements)")?;
            let text = serde_json::to_string(str_arg(args, "text")?).unwrap_or_default();
            let submit = args.get("submit").and_then(Value::as_bool).unwrap_or(false);
            let script = format!(
                r#"(() => {{ const el = document.querySelector('[data-chome-id="{id}"]');
                  if (!el) return 'Không thấy phần tử {id} — hãy gọi lại browser_elements';
                  el.scrollIntoView({{block: 'center'}}); el.focus();
                  if (el.isContentEditable) {{ document.execCommand('insertText', false, {text}); }}
                  else {{
                    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
                                : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
                    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, {text});
                    el.dispatchEvent(new Event('input', {{bubbles: true}}));
                    el.dispatchEvent(new Event('change', {{bubbles: true}}));
                  }}
                  if ({submit}) {{
                    const opts = {{key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true}};
                    el.dispatchEvent(new KeyboardEvent('keydown', opts));
                    el.dispatchEvent(new KeyboardEvent('keypress', opts));
                    el.dispatchEvent(new KeyboardEvent('keyup', opts));
                    if (el.form) {{ el.form.requestSubmit ? el.form.requestSubmit() : el.form.submit(); }}
                  }}
                  return 'ok'; }})()"#
            );
            let r = js(&view, &script).await?;
            if r != "ok" {
                return Err(r);
            }
            if submit {
                wait_load(&view).await;
            }
            Ok(Reply::Text(format!("Đã nhập vào phần tử {id}{}", if submit { " và gửi" } else { "" })))
        }

        "browser_scroll" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            let amount = args.get("amount").and_then(Value::as_i64).unwrap_or(600);
            let r = js(
                &view,
                &format!("window.scrollBy(0, {amount}); Math.round(scrollY) + '/' + Math.round(document.documentElement.scrollHeight - innerHeight)"),
            )
            .await?;
            Ok(Reply::Text(format!("Đã cuộn {amount}px, vị trí {r} px")))
        }

        "browser_screenshot" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            let tex = view
                .snapshot_future(webkit6::SnapshotRegion::Visible, webkit6::SnapshotOptions::NONE)
                .await
                .map_err(|e| format!("Không chụp được: {e}"))?;
            let png = tex.save_to_png_bytes();
            Ok(Reply::Image {
                png_base64: crate::pages::base64(&png),
                caption: format!("Ảnh chụp tab [{}] {}x{}\n{}", t.id(), tex.width(), tex.height(), page_summary(&view).await),
            })
        }

        "browser_eval" => {
            let (b, t) = target(args)?;
            let view = view_of(&b, &t).await?;
            Ok(Reply::Text(js(&view, str_arg(args, "script")?).await?))
        }

        other => Err(format!("Không có công cụ \"{other}\"")),
    }
}
