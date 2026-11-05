//! Chặn quảng cáo/tracker bằng content blocker gốc của WebKit.
//!
//! Luật được biên dịch thành bytecode một lần rồi lưu lại trên đĩa, nên mỗi lần
//! mở trình duyệt chỉ cần nạp (không phân tích lại JSON) và việc so khớp diễn ra
//! ngay trong engine mạng của WebKit — rẻ hơn nhiều so với tiện ích JS.

use std::path::Path;
use webkit6::{glib, UserContentFilter, UserContentFilterStore};

/// Domain quảng cáo / theo dõi phổ biến (quốc tế + Việt Nam).
const BLOCKED_DOMAINS: &[&str] = &[
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "googletagservices.com",
    "google-analytics.com",
    "googletagmanager.com",
    "adservice.google.com",
    "amazon-adsystem.com",
    "adnxs.com",
    "adsrvr.org",
    "advertising.com",
    "criteo.com",
    "criteo.net",
    "taboola.com",
    "outbrain.com",
    "scorecardresearch.com",
    "quantserve.com",
    "pubmatic.com",
    "rubiconproject.com",
    "openx.net",
    "casalemedia.com",
    "moatads.com",
    "adform.net",
    "smartadserver.com",
    "yieldmo.com",
    "teads.tv",
    "sharethrough.com",
    "3lift.com",
    "bidswitch.net",
    "mathtag.com",
    "demdex.net",
    "everesttech.net",
    "krxd.net",
    "bluekai.com",
    "hotjar.com",
    "mouseflow.com",
    "crazyegg.com",
    "ads-twitter.com",
    "analytics.twitter.com",
    "bat.bing.com",
    "clarity.ms",
    "adcolony.com",
    "popads.net",
    "popcash.net",
    "propellerads.com",
    "exoclick.com",
    "zedo.com",
    "media.net",
    "adroll.com",
    "zemanta.com",
    "revcontent.com",
    "mgid.com",
    "admicro.vn",
    "eclick.vn",
    "adtima.vn",
    "ambientplatform.vn",
];

/// Selector ẩn khung quảng cáo còn sót lại (giữ ở mức an toàn để không vỡ trang).
const HIDE_SELECTORS: &str = ".adsbygoogle, ins.adsbygoogle, [id^=\"div-gpt-ad\"], \
    [id^=\"google_ads_iframe\"], iframe[src*=\"doubleclick.net\"], \
    iframe[src*=\"googlesyndication.com\"], .trc_rbox_container, .OUTBRAIN, \
    .banner-ads, .section-ads-top, #supper_masthead, [id^=\"_ads_\"], #_footer_ads";

fn escape_regex(domain: &str) -> String {
    domain.replace('.', "\\\\.")
}

/// Sinh JSON theo định dạng content blocker của WebKit/Safari.
pub fn builtin_rules() -> String {
    let mut rules: Vec<String> = BLOCKED_DOMAINS
        .iter()
        .map(|d| {
            format!(
                r#"{{"trigger":{{"url-filter":"^[a-z]+://([^/]*\\.)?{}[/:]"}},"action":{{"type":"block"}}}}"#,
                escape_regex(d)
            )
        })
        .collect();
    // Facebook pixel (không chặn cả connect.facebook.net để nút đăng nhập vẫn chạy).
    rules.push(
        r#"{"trigger":{"url-filter":"connect\\.facebook\\.net/.*/fbevents\\.js"},"action":{"type":"block"}}"#
            .into(),
    );
    rules.push(format!(
        r#"{{"trigger":{{"url-filter":".*"}},"action":{{"type":"css-display-none","selector":"{}"}}}}"#,
        HIDE_SELECTORS.replace('"', "\\\"")
    ));
    format!("[{}]", rules.join(","))
}

/// Gộp luật có sẵn với file luật tùy chỉnh của người dùng (nếu có).
fn all_rules(custom: &Path) -> String {
    let builtin = builtin_rules();
    let Ok(extra) = std::fs::read_to_string(custom) else { return builtin };
    let extra = extra.trim();
    // Cả hai đều là mảng JSON: nối phần thân lại với nhau.
    match (builtin.strip_suffix(']'), extra.strip_prefix('[')) {
        (Some(a), Some(b)) if b.trim_start() != "]" => format!("{a},{b}"),
        _ => builtin,
    }
}

fn hash(s: &str) -> u64 {
    // FNV-1a: đủ để biết bộ luật có thay đổi hay không.
    s.bytes()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Nạp filter đã biên dịch; nếu luật thay đổi thì biên dịch lại. Gọi `done` khi xong.
pub fn load(store_dir: &Path, custom_rules: &Path, done: impl FnOnce(UserContentFilter) + 'static) {
    let json = all_rules(custom_rules);
    let id = format!("lite-adblock-{:016x}", hash(&json));
    let store = UserContentFilterStore::new(&store_dir.to_string_lossy());
    let store2 = store.clone();
    let id2 = id.clone();
    store.load(&id, None::<&webkit6::gio::Cancellable>, move |res| match res {
        Ok(filter) => done(filter),
        Err(_) => {
            let bytes = glib::Bytes::from_owned(json.into_bytes());
            store2.save(&id2, &bytes, None::<&webkit6::gio::Cancellable>, move |res| match res {
                Ok(filter) => done(filter),
                Err(e) => eprintln!("[adblock] không biên dịch được luật: {e}"),
            });
        }
    });
}
