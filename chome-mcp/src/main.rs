//! chome-mcp — cầu nối MCP cho trình duyệt Chỏme.
//!
//! Claude Code / Codex / Gemini CLI chạy chương trình này và nói giao thức MCP (JSON-RPC
//! 2.0, mỗi thông điệp một dòng) qua stdin/stdout. Mỗi lần gọi công cụ được chuyển tới
//! Chỏme qua Unix socket `$XDG_RUNTIME_DIR/chome/mcp.sock` (chỉ tài khoản hiện tại mở
//! được). Log in ra stderr — stdout chỉ dành cho giao thức.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CHOME_MCP_SOCKET") {
        return p.into();
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    runtime.join("chome").join("mcp.sock")
}

fn tab_id() -> Value {
    json!({"type": "integer", "description": "Tab id from browser_tabs. Omit to use the tab currently shown."})
}

fn tools() -> Value {
    json!([
        {
            "name": "browser_tabs",
            "description": "List open tabs in the Chỏme browser with their ids, titles and URLs. Depending on the user's settings, only tabs opened by AI may be visible.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "browser_open",
            "description": "Open a URL (or search terms) in a new tab and wait for it to load. Returns the new tab id. Prefer this over navigating the user's own tabs.",
            "inputSchema": {"type": "object", "properties": {
                "url": {"type": "string", "description": "URL or search query"}
            }, "required": ["url"]}
        },
        {
            "name": "browser_navigate",
            "description": "Navigate a tab: load a new URL, or go back / forward / reload. Waits for the page to load.",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "url": {"type": "string", "description": "URL or search query to load"},
                "action": {"type": "string", "enum": ["back", "forward", "reload"]}
            }}
        },
        {
            "name": "browser_select_tab",
            "description": "Switch the browser window to show the given tab.",
            "inputSchema": {"type": "object", "properties": {"tab_id": tab_id()}, "required": ["tab_id"]}
        },
        {
            "name": "browser_close_tab",
            "description": "Close a tab.",
            "inputSchema": {"type": "object", "properties": {"tab_id": tab_id()}, "required": ["tab_id"]}
        },
        {
            "name": "browser_read_page",
            "description": "Read the visible text content of a page (document.body.innerText), with its title and URL.",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "max_chars": {"type": "integer", "description": "Maximum characters to return (default 20000)"}
            }}
        },
        {
            "name": "browser_elements",
            "description": "List interactive elements (links, buttons, inputs…) on the page, each with a numeric id to use with browser_click / browser_type. Call again after the page changes.",
            "inputSchema": {"type": "object", "properties": {"tab_id": tab_id()}}
        },
        {
            "name": "browser_click",
            "description": "Click an element by the id returned from browser_elements.",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "element_id": {"type": "integer"}
            }, "required": ["element_id"]}
        },
        {
            "name": "browser_type",
            "description": "Set the text of an input/textarea/contenteditable element (id from browser_elements). Set submit=true to press Enter / submit the form.",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "element_id": {"type": "integer"},
                "text": {"type": "string"},
                "submit": {"type": "boolean"}
            }, "required": ["element_id", "text"]}
        },
        {
            "name": "browser_scroll",
            "description": "Scroll the page vertically by a number of pixels (negative scrolls up). Default 600.",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "amount": {"type": "integer"}
            }}
        },
        {
            "name": "browser_screenshot",
            "description": "Take a PNG screenshot of the visible part of a page.",
            "inputSchema": {"type": "object", "properties": {"tab_id": tab_id()}}
        },
        {
            "name": "browser_eval",
            "description": "Run JavaScript in the page and return the result as a string (return JSON.stringify(...) for objects).",
            "inputSchema": {"type": "object", "properties": {
                "tab_id": tab_id(),
                "script": {"type": "string"}
            }, "required": ["script"]}
        }
    ])
}

/// Gửi một lệnh tới Chỏme, nhận một dòng JSON trả về.
fn call_browser(tool: &str, args: &Value) -> Result<Value, String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|_| {
        "Không kết nối được trình duyệt Chỏme. Hãy mở Chỏme và bật \"Cho phép công cụ AI điều khiển \
         trình duyệt\" trong Cài đặt → Kết nối AI (MCP)."
            .to_string()
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(90))).ok();
    let mut msg = json!({"tool": tool, "args": args}).to_string();
    msg.push('\n');
    stream.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| format!("Chỏme không trả lời: {e}"))?;
    serde_json::from_str(&line).map_err(|e| format!("Phản hồi không hợp lệ từ Chỏme: {e}"))
}

fn tool_result(name: &str, args: &Value) -> Value {
    match call_browser(name, args) {
        Ok(reply) => {
            let ok = reply["ok"].as_bool().unwrap_or(false);
            let mut content = vec![json!({"type": "text", "text": reply["text"].as_str().unwrap_or("")})];
            if let Some(img) = reply["image"].as_str() {
                content.push(json!({"type": "image", "data": img, "mimeType": "image/png"}));
            }
            json!({"content": content, "isError": !ok})
        }
        Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
    }
}

fn respond(out: &mut impl Write, id: &Value, result: Result<Value, (i64, String)>) {
    let msg = match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    };
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                respond(&mut stdout, &Value::Null, Err((-32700, format!("Parse error: {e}"))));
                continue;
            }
        };
        // Không có "id" → notification (vd. notifications/initialized): không trả lời.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg["method"].as_str().unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = if SUPPORTED_PROTOCOLS.contains(&asked) { asked } else { SUPPORTED_PROTOCOLS[0] };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "chome", "title": "Chỏme browser", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Controls the user's Chỏme web browser. Typical flow: browser_open → browser_read_page or browser_elements → browser_click / browser_type. Element ids change when the page changes; call browser_elements again."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if tools().as_array().is_some_and(|t| t.iter().any(|x| x["name"] == name)) {
                    Ok(tool_result(name, &args))
                } else {
                    Err((-32602, format!("Unknown tool: {name}")))
                }
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err((-32601, format!("Method not found: {other}"))),
        };
        respond(&mut stdout, &id, result);
    }
}
