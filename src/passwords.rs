//! Quản lý mật khẩu: Secret Service, giao tiếp với script và logic thuần.

use crate::storage::Storage;
use std::collections::HashMap;
use std::rc::Rc;
use webkit6::{gio, glib, prelude::*};

type ObjectPath = glib::variant::ObjectPath;

pub const WORLD: &str = "ChomePasswordWorld";
pub const SAVE_HANDLER: &str = "chomePasswordSave";
pub const QUERY_HANDLER: &str = "chomePasswordQuery";
const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SCHEMA: &str = "vn.chome.Password";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credential {
    pub origin: String,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedCredential {
    pub origin: String,
    pub username: String,
    pub last_used: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Credentials(Credential),
    Query { origin: String },
}

pub fn parse_message(json: &str) -> Option<Message> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    match value.get("type")?.as_str()? {
        "credentials" => Some(Message::Credentials(Credential {
            origin: value.get("origin")?.as_str()?.to_string(),
            username: value.get("username")?.as_str()?.to_string(),
            password: value.get("password")?.as_str()?.to_string(),
        })),
        "query" => Some(Message::Query { origin: value.get("origin")?.as_str()?.to_string() }),
        _ => None,
    }
}

pub fn origin_host(origin: &str) -> &str {
    origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or(origin)
}

pub fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") { return None; }
    Some(format!("{scheme}://{}", rest.split(['/', '?', '#']).next()?))
}

#[cfg(test)]
pub fn choose_recent(items: &[SavedCredential]) -> Option<&SavedCredential> {
    items.iter().max_by_key(|item| item.last_used)
}

#[derive(Clone)]
struct SecretService {
    connection: gio::DBusConnection,
}

impl SecretService {
    fn connect() -> Option<Self> {
        let proxy = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            SERVICE,
            SERVICE_PATH,
            SERVICE_IFACE,
            None::<&gio::Cancellable>,
        )
        .ok()?;
        if proxy.name_owner().is_none() {
            return None;
        }
        let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>).ok()?;
        Some(Self { connection })
    }

    fn call(&self, path: &str, iface: &str, method: &str, params: Option<&glib::Variant>) -> Result<glib::Variant, glib::Error> {
        self.connection.call_sync(
            Some(SERVICE),
            path,
            iface,
            method,
            params,
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
        )
    }

    fn session(&self) -> Result<String, glib::Error> {
        let input = "".to_variant();
        let value = self.call(SERVICE_PATH, SERVICE_IFACE, "OpenSession", Some(&("plain", input).to_variant()))?;
        // OpenSession trả về (v output, o result).
        value.get::<(glib::Variant, ObjectPath)>().map(|(_, path)| path.to_string()).ok_or_else(|| glib::Error::new(glib::FileError::Failed, "Secret Service trả về phiên không hợp lệ"))
    }

    fn search(&self, attrs: &HashMap<String, String>) -> Result<Vec<String>, glib::Error> {
        // Tham số D-Bus phải là tuple: (a{ss}).
        let value = self.call(SERVICE_PATH, SERVICE_IFACE, "SearchItems", Some(&(attrs,).to_variant()))?;
        // SearchItems trả về (ao unlocked, ao locked) — gộp cả hai.
        value
            .get::<(Vec<ObjectPath>, Vec<ObjectPath>)>()
            .map(|(unlocked, locked)| unlocked.into_iter().chain(locked).map(String::from).collect()).ok_or_else(|| glib::Error::new(glib::FileError::Failed, "Secret Service trả về danh sách không hợp lệ"))
    }

    fn attributes(&self, path: &str) -> Result<HashMap<String, String>, glib::Error> {
        // "Attributes" là thuộc tính D-Bus (đọc qua org.freedesktop.DBus.Properties), không phải hàm.
        let value = self.call(path, "org.freedesktop.DBus.Properties", "Get", Some(&(ITEM_IFACE, "Attributes").to_variant()))?;
        value
            .get::<(glib::Variant,)>()
            .and_then(|(inner,)| inner.as_variant().unwrap_or(inner).get::<HashMap<String, String>>()).ok_or_else(|| glib::Error::new(glib::FileError::Failed, "Secret Service trả về thuộc tính không hợp lệ"))
    }

    fn unlock(&self, paths: &[String]) {
        let object_paths: Vec<ObjectPath> = paths.iter().filter_map(|path| ObjectPath::try_from(path.as_str()).ok()).collect();
        let _ = self.call(SERVICE_PATH, SERVICE_IFACE, "Unlock", Some(&(object_paths,).to_variant()));
    }

    fn secret(&self, path: &str, session: &str) -> Result<String, glib::Error> {
        let session = ObjectPath::try_from(session).map_err(|e| glib::Error::new(glib::FileError::Failed, &e.to_string()))?;
        let value = self.call(path, ITEM_IFACE, "GetSecret", Some(&(session,).to_variant()))?;
        // Secret = (phiên, tham số, GIÁ TRỊ, kiểu nội dung) — mật khẩu nằm ở trường thứ ba.
        let ((_, _, bytes, _),) = value.get::<((ObjectPath, Vec<u8>, Vec<u8>, String),)>().ok_or_else(|| glib::Error::new(glib::FileError::Failed, "Secret Service trả về bí mật không hợp lệ"))?;
        String::from_utf8(bytes).map_err(|_| glib::Error::new(glib::FileError::Failed, "Mật khẩu không phải UTF-8"))
    }

    fn delete(&self, path: &str) -> Result<(), glib::Error> {
        self.call(path, ITEM_IFACE, "Delete", None).map(|_| ())
    }

    fn set_attributes(&self, path: &str, attrs: &HashMap<String, String>) -> Result<(), glib::Error> {
        // Phần tử Variant trong tuple được glib-rs tự đóng gói thành kiểu "v".
        let params = (ITEM_IFACE, "Attributes", attrs.to_variant()).to_variant();
        self.call(path, "org.freedesktop.DBus.Properties", "Set", Some(&params)).map(|_| ())
    }

    fn save(&self, credential: &Credential) -> Result<(), glib::Error> {
        let attrs = attrs_for(credential, unix_now());
        for path in self.search(&base_attrs(credential))? {
            let _ = self.delete(&path);
        }
        // Bộ khóa mặc định theo chuẩn Secret Service: ReadAlias("default").
        let collection = self
            .call(SERVICE_PATH, SERVICE_IFACE, "ReadAlias", Some(&("default",).to_variant()))?
            .get::<(ObjectPath,)>()
            .map(|(path,)| path.to_string())
            .filter(|p| p != "/")
            .unwrap_or_else(|| "/org/freedesktop/secrets/aliases/default".into());
        let session = self.session()?;
        let mut props = HashMap::<String, glib::Variant>::new();
        props.insert("org.freedesktop.Secret.Item.Label".into(), format!("Chỏme — {} — {}", origin_host(&credential.origin), credential.username).to_variant());
        props.insert("org.freedesktop.Secret.Item.Attributes".into(), attrs.to_variant());
        let session = ObjectPath::try_from(session).map_err(|e| glib::Error::new(glib::FileError::Failed, &e.to_string()))?;
        // Secret = (o phiên, ay tham số — rỗng với phiên "plain", ay giá trị, s kiểu nội dung).
        let secret = (session, Vec::<u8>::new(), credential.password.as_bytes().to_vec(), "text/plain");
        self.call(&collection, "org.freedesktop.Secret.Collection", "CreateItem", Some(&(&props, &secret, true).to_variant())).map(|_| ())
    }
}

fn attrs_for(credential: &Credential, last_used: i64) -> HashMap<String, String> {
    let mut attrs = base_attrs(credential);
    attrs.insert("last_used".into(), last_used.to_string());
    attrs
}

fn base_attrs(credential: &Credential) -> HashMap<String, String> {
    HashMap::from([
        ("xdg:schema".into(), SCHEMA.into()),
        ("origin".into(), credential.origin.clone()),
        ("username".into(), credential.username.clone()),
    ])
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[derive(Clone)]
pub struct PasswordStore {
    secret: Option<SecretService>,
    db: Rc<Storage>,
}

impl PasswordStore {
    pub fn new(db: Rc<Storage>) -> Self {
        let secret = SecretService::connect();
        if secret.is_none() {
            eprintln!("[passwords] Secret Service không khả dụng; tắt hỏi lưu mật khẩu");
        }
        Self { secret, db }
    }

    pub fn available(&self) -> bool { self.secret.is_some() }

    pub fn is_never(&self, origin: &str) -> bool {
        self.db.get_setting("password_never").and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok()).is_some_and(|items| items.iter().any(|item| item == origin))
    }

    pub fn add_never(&self, origin: &str) {
        let mut items: Vec<String> = self.db.get_setting("password_never").and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
        if !items.iter().any(|item| item == origin) { items.push(origin.into()); }
        self.db.set_setting("password_never", &serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()));
    }

    pub fn save(&self, credential: &Credential) -> bool {
        let Some(secret) = &self.secret else { return false };
        match secret.save(credential) {
            Ok(()) => true,
            // Chỉ in lý do lỗi D-Bus — không bao giờ in thông tin đăng nhập.
            Err(e) => { eprintln!("[passwords] không thể lưu mật khẩu vào Secret Service: {}", e.message()); false }
        }
    }

    pub fn lookup(&self, origin: &str) -> Option<Credential> {
        let secret = self.secret.as_ref()?;
        let attrs = HashMap::from([("xdg:schema".into(), SCHEMA.into()), ("origin".into(), origin.into())]);
        let paths = secret.search(&attrs).ok()?;
        let mut candidates = Vec::new();
        for path in paths {
            let Ok(a) = secret.attributes(&path) else { continue };
            let Some(username) = a.get("username").cloned() else { continue };
            let last_used = a.get("last_used").and_then(|s| s.parse().ok()).unwrap_or(0);
            candidates.push((path, SavedCredential { origin: origin.into(), username, last_used }));
        }
        let (path, meta) = candidates.iter().max_by_key(|(_, item)| item.last_used)?;
        let session = secret.session().ok()?;
        let password = secret.secret(path, &session).or_else(|_| {
            secret.unlock(std::slice::from_ref(path));
            secret.secret(path, &session)
        }).ok()?;
        let credential = Credential { origin: meta.origin.clone(), username: meta.username.clone(), password };
        let _ = secret.set_attributes(path, &attrs_for(&credential, unix_now()));
        Some(credential)
    }

    pub fn lookup_user(&self, origin: &str, username: &str) -> Option<Credential> {
        let secret = self.secret.as_ref()?;
        let attrs = HashMap::from([
            ("xdg:schema".into(), SCHEMA.into()),
            ("origin".into(), origin.into()),
            ("username".into(), username.into()),
        ]);
        let path = secret.search(&attrs).ok()?.into_iter().next()?;
        let session = secret.session().ok()?;
        let password = secret.secret(&path, &session).or_else(|_| {
            secret.unlock(std::slice::from_ref(&path));
            secret.secret(&path, &session)
        }).ok()?;
        Some(Credential { origin: origin.into(), username: username.into(), password })
    }

    pub fn list(&self) -> Vec<SavedCredential> {
        let Some(secret) = &self.secret else { return vec![] };
        let attrs = HashMap::from([("xdg:schema".into(), SCHEMA.into())]);
        let Ok(paths) = secret.search(&attrs) else { return vec![] };
        paths.into_iter().filter_map(|path| {
            let a = secret.attributes(&path).ok()?;
            Some(SavedCredential { origin: a.get("origin")?.clone(), username: a.get("username")?.clone(), last_used: a.get("last_used").and_then(|s| s.parse().ok()).unwrap_or(0) })
        }).collect()
    }

    pub fn delete(&self, origin: &str, username: &str) {
        let Some(secret) = &self.secret else { return };
        let attrs = HashMap::from([("xdg:schema".into(), SCHEMA.into()), ("origin".into(), origin.into()), ("username".into(), username.into())]);
        if let Ok(paths) = secret.search(&attrs) { for path in paths { let _ = secret.delete(&path); } }
    }

    pub fn reply_json(&self, origin: &str) -> String {
        match self.lookup(origin) {
            Some(c) => serde_json::json!({ "found": true, "username": c.username, "password": c.password }).to_string(),
            None => "{\"found\":false}".into(),
        }
    }
}

pub fn message_json(value: &webkit6::javascriptcore::Value) -> Option<String> {
    value.to_json(0).map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_credentials_without_losing_unicode() {
        let message = parse_message(r#"{"type":"credentials","origin":"https://例.vn","username":"tôi","password":"bí mật"}"#).unwrap();
        assert_eq!(message, Message::Credentials(Credential { origin: "https://例.vn".into(), username: "tôi".into(), password: "bí mật".into() }));
    }

    #[test]
    fn host_is_safe_for_display() {
        assert_eq!(origin_host("https://example.test:8443/login"), "example.test:8443");
        assert_eq!(url_origin("https://example.test:8443/login"), Some("https://example.test:8443".into()));
    }

    #[test]
    fn recent_item_wins() {
        let items = [
            SavedCredential { origin: "https://a".into(), username: "old".into(), last_used: 2 },
            SavedCredential { origin: "https://a".into(), username: "new".into(), last_used: 8 },
        ];
        assert_eq!(choose_recent(&items).unwrap().username, "new");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Chạy tay: `cargo test --release -- --ignored live --nocapture` (cần Secret Service thật).
    #[test]
    #[ignore]
    fn live_secret_service_roundtrip() {
        let s = SecretService::connect().expect("không có Secret Service");
        let attrs = HashMap::from([("xdg:schema".to_string(), SCHEMA.to_string())]);
        let raw = s.call(SERVICE_PATH, SERVICE_IFACE, "SearchItems", Some(&(&attrs,).to_variant()));
        println!("SearchItems tham số {:?} → {:?}", attrs.to_variant().type_(), raw.as_ref().map(|v| v.type_().to_string()));
        println!("search() → {:?}", s.search(&attrs).map(|v| v.len()));
        let session = s.session();
        println!("session() → {:?}", session.as_ref().map(|_| "ok"));
    }
}
