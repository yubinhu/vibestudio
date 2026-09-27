//! Chrome DevTools viewport catalog: cached locally, refreshed only on request.
//! Upstream TypeScript is parsed as data only, never executed.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SOURCE_URL: &str = "https://raw.githubusercontent.com/ChromeDevTools/devtools-frontend/main/front_end/models/emulation/EmulatedDevices.ts";
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub label: String,
    pub group: String,
    pub width: u32,
    pub height: u32,
    pub show_by_default: bool,
    pub order: u32,
}
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCatalog {
    pub source_url: String,
    pub source_sha256: String,
    #[serde(default)]
    pub checked_at: Option<u64>,
    pub devices: Vec<Device>,
    #[serde(default, skip_deserializing)]
    pub refreshing: bool,
    #[serde(default, skip_deserializing)]
    pub error: Option<String>,
}
struct CatalogState {
    catalog: DeviceCatalog,
}
struct CatalogCache {
    state: Mutex<CatalogState>,
    path: Option<PathBuf>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn bundled() -> DeviceCatalog {
    serde_json::from_str(include_str!(
        "../../../client/web/lib/comparisonDevices.generated.json"
    ))
    .expect("bundled Chrome device catalog must be valid")
}
fn valid(catalog: &DeviceCatalog) -> bool {
    let mut ids = BTreeSet::new();
    catalog.source_url == SOURCE_URL
        && catalog.source_sha256.len() == 64
        && catalog
            .source_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && !catalog.devices.is_empty()
        && catalog.devices.len() <= 512
        && catalog.devices.iter().all(|device| {
            !device.id.is_empty()
                && device.id.len() <= 128
                && device
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && ids.insert(&device.id)
                && !device.label.is_empty()
                && device.label.len() <= 200
                && !device.label.chars().any(char::is_control)
                && matches!(device.group.as_str(), "Phones" | "Tablets")
                && (240..=3840).contains(&device.width)
                && (240..=3840).contains(&device.height)
        })
}
impl CatalogCache {
    fn new(path: Option<PathBuf>) -> Self {
        let cached = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .filter(|bytes| bytes.len() <= MAX_SOURCE_BYTES as usize)
            .and_then(|bytes| serde_json::from_slice::<DeviceCatalog>(&bytes).ok())
            .filter(valid)
            .filter(|catalog| catalog.checked_at.is_some_and(|checked| checked <= now()));
        Self {
            state: Mutex::new(CatalogState {
                catalog: cached.unwrap_or_else(bundled),
            }),
            path,
        }
    }
    fn begin(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.catalog.refreshing {
            return false;
        }
        state.catalog.refreshing = true;
        state.catalog.error = None;
        true
    }
    fn finish(&self, result: Result<DeviceCatalog, String>) {
        if let Ok(catalog) = &result {
            if let Some(path) = &self.path {
                let write = || {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    crate::state_store::update::<DeviceCatalog>(path, |stored| {
                        *stored = catalog.clone();
                        Ok(())
                    })
                    .map(|_| ())
                };
                if let Err(error) = write() {
                    log::debug!("Could not cache Chrome device catalog: {error}");
                }
            }
        }
        let mut state = self.state.lock().unwrap();
        match result {
            Ok(catalog) => state.catalog = catalog,
            Err(error) => {
                log::debug!("Keeping cached Chrome device catalog: {error}");
                state.catalog.error =
                    Some("Could not sync from Chrome. Using the saved device list.".into());
            }
        }
        state.catalog.refreshing = false;
    }
}
fn shared_cache() -> &'static Arc<CatalogCache> {
    static CACHE: OnceLock<Arc<CatalogCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Arc::new(CatalogCache::new(
            crate::paths::config_dir()
                .ok()
                .map(|path| path.join("comparison-devices.json")),
        ))
    })
}
/// Reading the chooser never performs a network request, regardless of cache age.
pub fn catalog() -> DeviceCatalog {
    shared_cache().state.lock().unwrap().catalog.clone()
}
/// Only an explicit user or agent refresh starts a download. Concurrent refresh
/// requests share one worker; failure retains the previous catalog for offline use.
pub fn refresh_catalog() -> DeviceCatalog {
    let cache = shared_cache();
    if cache.begin() {
        let cache = cache.clone();
        std::thread::spawn(move || cache.finish(fetch_catalog()));
    }
    catalog()
}
fn fetch_catalog() -> Result<DeviceCatalog, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .redirects(0)
        .build()
        .get(SOURCE_URL)
        .call()
        .map_err(|error| error.to_string())?;
    let mut source = String::new();
    response
        .into_reader()
        .take(MAX_SOURCE_BYTES + 1)
        .read_to_string(&mut source)
        .map_err(|error| error.to_string())?;
    if source.len() as u64 > MAX_SOURCE_BYTES {
        return Err("Chrome device source exceeded size limit".into());
    }
    parse_source(&source, now())
}
fn slug(label: &str) -> String {
    label
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_lowercase() && !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
/// ChromeDriver exposes literal alternatives for translated laptop titles in
/// this region. Keep those alternatives, then accept only JSON-compatible data.
fn parse_source(source: &str, timestamp: u64) -> Result<DeviceCatalog, String> {
    let region = source
        .split_once("// DEVICE-LIST-BEGIN")
        .and_then(|(_, rest)| {
            rest.split_once("// DEVICE-LIST-END")
                .map(|(region, _)| region)
        })
        .ok_or("Chrome device source markers changed")?;
    let alternatives = regex::Regex::new(
        r"(?s)/\* DEVICE-LIST-IF-JS \*/.*?/\* DEVICE-LIST-ELSE(.*?)DEVICE-LIST-END-IF \*/",
    )
    .unwrap();
    let data = format!("[{}]", alternatives.replace_all(region, "$1"));
    let entries: Vec<Value> = serde_json::from_str(&data_json(&data)?)
        .map_err(|error| format!("Chrome device data changed: {error}"))?;
    let mut devices = Vec::new();
    for entry in entries {
        let group = match entry.get("type").and_then(Value::as_str) {
            Some("phone") => "Phones",
            Some("tablet") => "Tablets",
            _ => continue,
        };
        let label = entry
            .get("title")
            .and_then(Value::as_str)
            .ok_or("Missing Chrome device title")?;
        let vertical = entry
            .get("screen")
            .and_then(|screen| screen.get("vertical"))
            .ok_or("Missing Chrome viewport")?;
        let dimension = |key| {
            vertical
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or("Invalid Chrome viewport size")
        };
        let width = dimension("width")?;
        let height = dimension("height")?;
        // The comparison's supported custom-size limits also apply to imported
        // viewports. Foldables use their primary viewport, without hinge emulation.
        if !(240..=3840).contains(&width) || !(240..=3840).contains(&height) {
            continue;
        }
        devices.push(Device {
            id: slug(label),
            label: label.into(),
            group: group.into(),
            width,
            height,
            show_by_default: entry
                .get("show-by-default")
                .and_then(Value::as_bool)
                .ok_or("Missing Chrome device visibility")?,
            order: entry
                .get("order")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
        });
    }
    devices.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.label.cmp(&right.label))
    });
    let catalog = DeviceCatalog {
        source_url: SOURCE_URL.into(),
        source_sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
        checked_at: Some(timestamp),
        devices,
        refreshing: false,
        error: None,
    };
    if !valid(&catalog) {
        return Err("Invalid or empty Chrome device catalog".into());
    }
    Ok(catalog)
}
/// Convert single-quoted strings/comments/trailing commas to JSON. Deliberately
/// leave identifiers, function calls and other executable syntax invalid JSON.
fn data_json(source: &str) -> Result<String, String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\'' | '"' => {
                let quote = character;
                let mut string = String::from("\"");
                let mut closed = false;
                while let Some(character) = chars.next() {
                    if character == quote {
                        closed = true;
                        break;
                    }
                    if character == '\\' {
                        let escaped = chars.next().ok_or("Incomplete device string escape")?;
                        match escaped {
                            '\'' if quote == '\'' => string.push('\''),
                            '"' => string.push_str("\\\""),
                            _ => {
                                string.push('\\');
                                string.push(escaped);
                            }
                        }
                    } else if character == '"' {
                        string.push_str("\\\"");
                    } else {
                        string.push(character);
                    }
                }
                if !closed {
                    return Err("Unterminated device string".into());
                }
                string.push('"');
                tokens.push(string);
            }
            '/' => match chars.next() {
                Some('/') => {
                    for character in chars.by_ref() {
                        if character == '\n' {
                            break;
                        }
                    }
                }
                Some('*') => {
                    let mut closed = false;
                    while let Some(character) = chars.next() {
                        if character == '*' && chars.peek() == Some(&'/') {
                            chars.next();
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err("Unterminated device comment".into());
                    }
                }
                _ => return Err("Executable syntax in Chrome device source".into()),
            },
            character if character.is_whitespace() => {}
            character => tokens.push(character.to_string()),
        }
    }
    Ok(tokens
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            token.as_str() != ","
                || !tokens
                    .get(index + 1)
                    .is_some_and(|next| next == "]" || next == "}")
        })
        .map(|(_, token)| token.as_str())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = r#"const data = [
// DEVICE-LIST-BEGIN
{'title':'Current Phone','type':'phone','show-by-default':true,'order':5,
'screen':{'vertical':{'width':402,'height':874,},},'user-agent':'https://example.test',},
{'title':'Old Phone','type':'phone','show-by-default':false,'order':6,'screen':{'vertical':{'width':375,'height':667}}},
/* DEVICE-LIST-IF-JS */ 'ignored': call(), /* DEVICE-LIST-ELSE
{'title':'Laptop','type':'notebook','screen':{'vertical':{'width':800,'height':1280}}},
DEVICE-LIST-END-IF */
// DEVICE-LIST-END
];"#;
    #[test]
    fn parses_chrome_data_without_executing_source() {
        let parsed = parse_source(SOURCE, 42).unwrap();
        assert_eq!(parsed.devices.len(), 2);
        assert_eq!(parsed.devices[0].id, "current-phone");
        assert_eq!(parsed.devices[0].width, 402);
        assert!(parsed.devices[0].show_by_default);
        assert!(!parsed.devices[1].show_by_default);
        assert_eq!(parsed.checked_at, Some(42));
        for bad in [
            SOURCE.replace("402", "run()"),
            SOURCE.replace("'Current Phone'", "fetch('https://evil.test')"),
            SOURCE.replace("DEVICE-LIST-BEGIN", "CHANGED"),
        ] {
            assert!(parse_source(&bad, 42).is_err());
        }
    }
    #[test]
    #[ignore = "Checks the live Chrome source; requires network access"]
    fn live_upstream_is_accepted() {
        let fetched = fetch_catalog().unwrap();
        assert!(valid(&fetched));
        let bundled = bundled();
        if fetched.source_sha256 == bundled.source_sha256 {
            assert_eq!(
                fetched.devices, bundled.devices,
                "Rust and the bundle generator must interpret Chrome identically"
            );
        }
    }
    #[test]
    fn bundled_catalog_is_valid_and_source_is_fixed() {
        assert!(valid(&bundled()));
        let mut catalog = bundled();
        catalog.source_url = "https://other.test".into();
        assert!(!valid(&catalog));
    }
    #[test]
    fn refresh_is_explicit_deduplicated_and_retains_last_good_data_on_failure() {
        let cache = CatalogCache::new(None);
        assert!(!cache.state.lock().unwrap().catalog.refreshing);
        assert!(cache.begin());
        assert!(!cache.begin());
        cache.finish(Ok(parse_source(SOURCE, 42).unwrap()));
        assert!(!cache.state.lock().unwrap().catalog.refreshing);
        assert!(cache.begin());
        cache.finish(Err("offline".into()));
        assert_eq!(
            cache.state.lock().unwrap().catalog.devices[0].id,
            "current-phone"
        );
        assert!(cache.state.lock().unwrap().catalog.error.is_some());
        assert!(!cache.state.lock().unwrap().catalog.refreshing);
        assert!(cache.begin());
        assert!(cache.state.lock().unwrap().catalog.error.is_none());
    }
    #[test]
    fn valid_cache_survives_restart_and_malformed_cache_uses_bundle() {
        let dir = crate::state_store::tests::TempDir::new();
        let path = dir.0.join("devices.json");
        let cache = CatalogCache::new(Some(path.clone()));
        cache.finish(Ok(parse_source(SOURCE, now()).unwrap()));
        assert_eq!(
            CatalogCache::new(Some(path.clone()))
                .state
                .lock()
                .unwrap()
                .catalog
                .devices[0]
                .id,
            "current-phone"
        );
        std::fs::write(&path, "invalid").unwrap();
        assert_eq!(
            CatalogCache::new(Some(path))
                .state
                .lock()
                .unwrap()
                .catalog
                .devices,
            bundled().devices
        );
    }
}
