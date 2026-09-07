//! Read-only connector inventory. Configuration is evidence of configuration,
//! never evidence of a live connection. The projection below is an allowlist:
//! no commands, arguments, environment, headers, OAuth material or complete
//! endpoint URLs cross the API boundary. No MCP process is started here.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInventory {
    pub connectors: Vec<ConnectorInfo>,
    pub agents: Vec<ConnectorAgent>,
    pub sources: Vec<ConnectorSource>,
    pub scanned_at: u64,
}

impl Default for ConnectorInventory {
    fn default() -> Self {
        Self {
            connectors: Vec::new(),
            agents: Vec::new(),
            sources: Vec::new(),
            scanned_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub availability: Vec<ConnectorAvailability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_connection_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorAvailability {
    pub agent_id: String,
    pub state: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    pub source_id: String,
    pub source_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_connection_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorAgent {
    pub id: String,
    pub label: String,
    pub clients: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorSource {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub state: String,
    #[serde(default = "configuration_discovery")]
    pub discovery: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn configuration_discovery() -> String {
    "configuration".into()
}

impl ConnectorInventory {
    /// Merge identical endpoints (or identical local launch definitions), while
    /// retaining every source and client. A source occurrence is the unit of
    /// status: a runtime observation must not silently erase a disabled config.
    pub fn merge(&mut self, other: Self) {
        self.scanned_at = self.scanned_at.max(other.scanned_at);
        for agent in other.agents {
            if let Some(old) = self.agents.iter_mut().find(|a| a.id == agent.id) {
                for client in agent.clients {
                    if !old.clients.contains(&client) {
                        old.clients.push(client);
                    }
                }
            } else {
                self.agents.push(agent);
            }
        }
        for source in other.sources {
            if let Some(old) = self.sources.iter_mut().find(|s| s.id == source.id) {
                *old = source;
            } else {
                self.sources.push(source);
            }
        }
        for connector in other.connectors {
            if let Some(old) = self.connectors.iter_mut().find(|c| c.id == connector.id) {
                for id in connector.managed_connection_ids {
                    if !old.managed_connection_ids.contains(&id) {
                        old.managed_connection_ids.push(id);
                    }
                }
                for availability in connector.availability {
                    if let Some(previous) = old.availability.iter_mut().find(|a| {
                        a.agent_id == availability.agent_id
                            && a.source_id == availability.source_id
                            && a.scope == availability.scope
                            && a.project_path == availability.project_path
                    }) {
                        *previous = availability;
                    } else {
                        old.availability.push(availability);
                    }
                }
            } else {
                self.connectors.push(connector);
            }
        }
        self.connectors.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        self.agents.sort_by(|a, b| a.label.cmp(&b.label));
        self.sources
            .sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    }
}

pub fn stable_id(kind: &str, value: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(value.as_bytes());
    format!("{kind}-{:x}", hash.finalize())
}

pub fn remote_identity(endpoint: &str) -> String {
    // URL parsing canonicalizes scheme/hostname/port. Keep path and query in the
    // opaque identity: different resources/accounts must not be merged by host.
    let normalized = url::Url::parse(endpoint)
        .map(|mut u| {
            u.set_fragment(None);
            u.to_string()
        })
        .unwrap_or_else(|_| endpoint.to_string());
    stable_id("remote", &normalized)
}

pub fn local_identity(command: &Value, args: Option<&Value>) -> String {
    let mut launch = match command {
        Value::Array(a) => a.clone(),
        _ => vec![command.clone()],
    };
    if let Some(Value::Array(args)) = args {
        launch.extend(args.iter().cloned());
    }
    stable_id("local", &Value::Array(launch).to_string())
}

/// The same `node server.js` invocation in two projects is two local servers.
pub fn local_identity_in(
    command: &Value,
    args: Option<&Value>,
    execution_root: Option<&Path>,
) -> String {
    let launch_id = local_identity(command, args);
    match execution_root {
        Some(root) => stable_id(
            "local",
            &serde_json::json!([launch_id, normalized_path(root)]).to_string(),
        ),
        None => launch_id,
    }
}

/// A configured cwd overrides the session cwd. Never expand placeholders.
pub fn local_execution_root(config: &Value, base: Option<&Path>) -> Option<PathBuf> {
    let path = match config
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        Some(cwd) => {
            let cwd = PathBuf::from(cwd);
            if cwd.is_absolute() {
                cwd
            } else {
                base.map(|p| p.join(&cwd)).unwrap_or(cwd)
            }
        }
        None => base?.to_path_buf(),
    };
    Some(normalized_path(&path))
}

fn normalized_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Callers must also verify the returned ID exists in the managed store.
pub fn managed_gateway_id(endpoint: &str) -> Option<String> {
    let url = url::Url::parse(endpoint).ok()?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
    {
        return None;
    }
    let parts: Vec<_> = url.path().split('/').collect();
    (parts.len() == 4 && parts[1] == "gw" && !parts[2].is_empty() && parts[3] == "mcp")
        .then(|| parts[2].to_string())
}

pub fn safe_host(endpoint: &str) -> Option<String> {
    let parsed = url::Url::parse(endpoint).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss" | "sse") {
        return None;
    }
    let host = parsed.host_str()?;
    // An unresolved placeholder is not a hostname. Never expand environment or
    // file placeholders in discovery (their values can themselves be secrets).
    if host.contains(['$', '{', '}', '%']) {
        return None;
    }
    Some(match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

#[derive(Clone, Copy)]
pub enum ConnectorAdapter {
    ClaudeCode,
    Codex,
    OpenCode,
    Json {
        user: &'static str,
        project: &'static str,
        key: &'static str,
        clients: &'static [&'static str],
    },
}

impl ConnectorAdapter {
    fn clients(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["CLI", "VS Code", "Desktop Code"],
            Self::Codex => &["CLI", "Desktop", "VS Code"],
            Self::OpenCode => &["CLI", "Desktop", "IDE"],
            Self::Json { clients, .. } => clients,
        }
    }
}

struct Context {
    home: PathBuf,
    project: Option<PathBuf>,
    env: HashMap<String, String>,
}
impl Context {
    fn env_path(&self, name: &str) -> Option<PathBuf> {
        self.env
            .get(name)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    }
    fn config_home(&self) -> PathBuf {
        self.env_path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| self.home.join(".config"))
    }
    fn claude_home(&self) -> PathBuf {
        self.env_path("CLAUDE_CONFIG_DIR")
            .unwrap_or_else(|| self.home.join(".claude"))
    }
    fn codex_home(&self) -> PathBuf {
        self.env_path("CODEX_HOME")
            .unwrap_or_else(|| self.home.join(".codex"))
    }
}

#[derive(Clone)]
struct SourceRef {
    id: String,
    label: String,
    path: Option<String>,
}

#[derive(Clone)]
struct Definition {
    name: String,
    value: Value,
    source: SourceRef,
    scope: String,
    project: Option<PathBuf>,
    disabled: bool,
    execution_root: Option<PathBuf>,
}

#[derive(Default)]
struct Collector {
    inventory: ConnectorInventory,
    managed: HashMap<String, (String, String)>,
    execution_root: Option<PathBuf>,
}

impl Collector {
    fn definitions(
        &mut self,
        value: &Value,
        key: &str,
        source: &SourceRef,
        scope: &str,
        project: Option<&Path>,
    ) -> Vec<Definition> {
        if value.get(key).is_some_and(|v| !v.is_object()) {
            self.issue(source, "The MCP server map has an unsupported structure.");
        }
        definitions(value, key, source, scope, project)
    }
    fn source(&mut self, agent: &str, path: Option<&Path>, label: &str) -> SourceRef {
        let display = path.map(|p| p.to_string_lossy().into_owned());
        let id = stable_id(
            "source",
            &format!("{agent}:{}:{label}", display.as_deref().unwrap_or("")),
        );
        if !self.inventory.sources.iter().any(|s| s.id == id) {
            self.inventory.sources.push(ConnectorSource {
                id: id.clone(),
                label: label.into(),
                agent_id: Some(agent.into()),
                state: "scanned".into(),
                discovery: "configuration".into(),
                message: None,
            });
        }
        SourceRef {
            id,
            label: label.into(),
            path: display,
        }
    }
    fn issue(&mut self, source: &SourceRef, message: &str) {
        if let Some(s) = self
            .inventory
            .sources
            .iter_mut()
            .find(|s| s.id == source.id)
        {
            s.state = "error".into();
            s.message = Some(message.into());
        }
    }
    fn read(
        &mut self,
        agent: &str,
        path: &Path,
        label: &str,
        format: &str,
    ) -> Option<(Value, SourceRef)> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A config accidentally replaced with a FIFO must not hang the
            // passive HTTP route before we can inspect its file type.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = match options.open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => {
                let source = self.source(agent, Some(path), label);
                self.issue(&source, "Could not read this configuration file.");
                return None;
            }
        };
        let source = self.source(agent, Some(path), label);
        match file.metadata() {
            Ok(metadata) if metadata.is_file() => {}
            _ => {
                self.issue(&source, "Configuration must be a readable regular file.");
                return None;
            }
        }
        let mut content = String::new();
        if file
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut content)
            .is_err()
        {
            self.issue(&source, "Could not read this configuration file.");
            return None;
        }
        if content.len() > 4 * 1024 * 1024 {
            self.issue(
                &source,
                "Configuration file exceeds the discovery size limit.",
            );
            return None;
        }
        self.parse(&content, format, &source).map(|v| (v, source))
    }
    fn parse(&mut self, content: &str, format: &str, source: &SourceRef) -> Option<Value> {
        let value = if format == "toml" {
            toml::from_str::<toml::Value>(content)
                .ok()
                .and_then(|v| serde_json::to_value(v).ok())
        } else if format == "jsonc" {
            parse_jsonc(content)
        } else {
            serde_json::from_str::<Value>(content).ok()
        };
        match value {
            Some(v) if v.is_object() => Some(v),
            _ => {
                self.issue(
                    source,
                    "Configuration is malformed or has an unsupported structure.",
                );
                None
            }
        }
    }
    fn add(&mut self, agent: &str, definition: Definition) {
        let d = definition;
        let v = &d.value;
        if !v.is_object() {
            self.issue(
                &d.source,
                "One or more connector definitions have an unsupported structure.",
            );
            return;
        }
        let endpoint = v
            .get("url")
            .or_else(|| v.get("httpUrl"))
            .and_then(Value::as_str);
        let command = v.get("command");
        let (mut id, kind, host) = if let Some(endpoint) = endpoint.filter(|s| !s.is_empty()) {
            (remote_identity(endpoint), "remote", safe_host(endpoint))
        } else if let Some(command) = command.filter(|v| v.is_string() || v.is_array()) {
            let root = local_execution_root(
                v,
                d.execution_root
                    .as_deref()
                    .or(self.execution_root.as_deref())
                    .or(d.project.as_deref()),
            );
            (
                local_identity_in(command, v.get("args"), root.as_deref()),
                "local",
                None,
            )
        } else {
            self.issue(
                &d.source,
                "One or more connectors lack a usable endpoint or command.",
            );
            return;
        };
        let mut managed_connection_id = None;
        let mut managed_state = None;
        if let Some(gateway_id) = endpoint.and_then(managed_gateway_id) {
            if let Some((known, state)) = self.managed.get(&gateway_id) {
                id = known.clone();
                managed_connection_id = Some(gateway_id);
                managed_state = Some(state.as_str());
            }
        }
        let disabled = d.disabled
            || v.get("disabled").and_then(Value::as_bool) == Some(true)
            || v.get("enabled").and_then(Value::as_bool) == Some(false);
        let connector = ConnectorInfo {
            id,
            name: clean_name(&d.name),
            kind: kind.into(),
            host,
            managed_connection_ids: managed_connection_id.iter().cloned().collect(),
            availability: vec![ConnectorAvailability {
                agent_id: agent.into(),
                state: if disabled {
                    "disabled"
                } else {
                    managed_state.unwrap_or("configured")
                }
                .into(),
                scope: d.scope,
                project_path: d.project.map(|p| p.to_string_lossy().into_owned()),
                source_id: d.source.id,
                source_label: d.source.label,
                source_path: d.source.path,
                managed_connection_id,
            }],
        };
        self.inventory.merge(ConnectorInventory {
            connectors: vec![connector],
            ..Default::default()
        });
    }
}

fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(160).collect()
}

fn definitions(
    value: &Value,
    key: &str,
    source: &SourceRef,
    scope: &str,
    project: Option<&Path>,
) -> Vec<Definition> {
    value
        .get(key)
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|m| m.iter())
        .map(|(name, value)| Definition {
            name: name.clone(),
            value: value.clone(),
            source: source.clone(),
            scope: scope.into(),
            project: project.map(Path::to_path_buf),
            disabled: false,
            execution_root: None,
        })
        .collect()
}

pub fn discover(project: Option<&str>) -> Result<ConnectorInventory, String> {
    let project = canonical_project(project)?;
    let home = dirs::home_dir().ok_or("Cannot locate the server user's home directory.")?;
    let env = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "APPDATA",
        "OPENCODE_CONFIG",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG_CONTENT",
    ]
    .into_iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.into(), v)))
    .collect();
    let context = Context { home, project, env };
    let mut collector = Collector::default();
    add_managed(&mut collector);
    scan(&context, &mut collector);
    Ok(collector.inventory)
}

fn canonical_project(project: Option<&str>) -> Result<Option<PathBuf>, String> {
    project
        .map(|p| {
            let path = Path::new(p);
            if !path.is_absolute() {
                return Err("Project must be an existing absolute directory.".to_string());
            }
            std::fs::canonicalize(path)
                .ok()
                .filter(|p| p.is_dir())
                .ok_or_else(|| "Project must be an existing absolute directory.".to_string())
        })
        .transpose()
}

fn scan(context: &Context, collector: &mut Collector) {
    for agent in crate::agents::AGENTS {
        let Some(adapter) = agent.connector_discovery else {
            continue;
        };
        collector.inventory.agents.push(ConnectorAgent {
            id: agent.family.into(),
            label: agent.label.into(),
            clients: adapter.clients().iter().map(|s| (*s).into()).collect(),
        });
        let before = collector.inventory.sources.len();
        match adapter {
            ConnectorAdapter::ClaudeCode => scan_claude(context, collector, agent.family),
            ConnectorAdapter::Codex => scan_codex(context, collector, agent.family),
            ConnectorAdapter::OpenCode => scan_opencode(context, collector, agent.family),
            ConnectorAdapter::Json {
                user, project, key, ..
            } => scan_json(context, collector, agent.family, user, project, key),
        }
        if collector.inventory.sources.len() == before {
            let source = collector.source(
                agent.family,
                None,
                &format!("{} configuration", agent.label),
            );
            if let Some(s) = collector
                .inventory
                .sources
                .iter_mut()
                .find(|s| s.id == source.id)
            {
                s.state = "unavailable".into();
                s.message = Some("No local configuration found on this server.".into());
            }
        }
    }
    scan_claude_desktop(context, collector);
}

fn add_managed(collector: &mut Collector) {
    let source = collector.source("vibestudio", None, "Managed in VibeStudio");
    if let Some(source) = collector
        .inventory
        .sources
        .iter_mut()
        .find(|s| s.id == source.id)
    {
        source.agent_id = None;
    }
    match crate::connections::list() {
        Ok(connections) => {
            for connection in connections {
                let id = stable_id("managed", &connection.id);
                let state = match connection.status.as_str() {
                    "needs_reauth" => "needs_auth",
                    "error" => "error",
                    _ => "configured",
                };
                collector
                    .managed
                    .insert(connection.id.clone(), (id.clone(), state.into()));
                if state != "configured" {
                    collector.issue(&source, "One or more managed connections need sign-in or have a stored connection error.");
                }
                // agentsConfigured is historical write success, not proof the
                // entry still exists. Per-agent rows come from observed configs.
                collector.inventory.connectors.push(ConnectorInfo {
                    id,
                    name: clean_name(&connection.label),
                    kind: "remote".into(),
                    host: safe_host(&format!("https://{}", connection.host)),
                    availability: Vec::new(),
                    managed_connection_ids: vec![connection.id],
                });
            }
        }
        Err(_) => collector.issue(&source, "Could not read VibeStudio's managed connections."),
    }
}

// JSON with comments and trailing commas, without JSON5's extra expressions.
// Strings are copied verbatim, so // inside a URL and escaped quotes survive.
fn parse_jsonc(input: &str) -> Option<Value> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let (mut i, mut in_string, mut escaped) = (0, false, false);
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            out.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            out.push(b);
            i += 1;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(b' ');
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 >= bytes.len() {
                return None;
            }
            i += 2;
            out.push(b' ');
            continue;
        }
        out.push(b);
        i += 1;
    }
    let mut clean = Vec::with_capacity(out.len());
    in_string = false;
    escaped = false;
    for (i, b) in out.iter().copied().enumerate() {
        if in_string {
            clean.push(b);
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            if b == b'"' {
                in_string = true;
            }
            if b == b','
                && out[i + 1..]
                    .iter()
                    .find(|b| !b.is_ascii_whitespace())
                    .is_some_and(|b| matches!(b, b'}' | b']'))
            {
                continue;
            }
            clean.push(b);
        }
    }
    serde_json::from_slice(&clean).ok()
}

fn insert_definitions(map: &mut BTreeMap<String, Definition>, rows: Vec<Definition>, merge: bool) {
    for mut row in rows {
        if merge {
            if let Some(old) = map.get(&row.name) {
                let mut combined = old.value.clone();
                merge_json(&mut combined, row.value);
                row.value = combined;
            }
        }
        map.insert(row.name.clone(), row);
    }
}

fn merge_json(base: &mut Value, overlay: Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (key, value) in overlay {
            merge_json(base.entry(key).or_insert(Value::Null), value.clone());
        }
    } else {
        *base = overlay;
    }
}

fn contains_name(value: &Value, key: &str, name: &str) -> bool {
    value
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|n| n.as_str() == Some(name)))
}

fn scan_json(
    context: &Context,
    collector: &mut Collector,
    agent: &str,
    user: &str,
    project: &str,
    key: &str,
) {
    collector.execution_root = context.project.clone();
    let mut effective = BTreeMap::new();
    if let Some((value, source)) = collector.read(
        agent,
        &context.home.join(user),
        "User MCP configuration",
        "jsonc",
    ) {
        insert_definitions(
            &mut effective,
            collector.definitions(&value, key, &source, "user", None),
            false,
        );
    }
    if let Some(root) = context.project.as_ref() {
        if let Some((value, source)) = collector.read(
            agent,
            &root.join(project),
            "Project MCP configuration",
            "jsonc",
        ) {
            insert_definitions(
                &mut effective,
                collector.definitions(&value, key, &source, "project", Some(root)),
                false,
            );
        }
    }
    for (_, definition) in effective {
        collector.add(agent, definition);
    }
}

fn scan_claude(context: &Context, collector: &mut Collector, agent: &str) {
    collector.execution_root = context.project.clone();
    let config = context
        .env_path("CLAUDE_CONFIG_DIR")
        .map(|p| p.join(".claude.json"))
        .unwrap_or_else(|| context.home.join(".claude.json"));
    let mut effective = BTreeMap::new();
    let mut global = Value::Null;
    if let Some((value, source)) =
        collector.read(agent, &config, "Claude Code MCP configuration", "json")
    {
        insert_definitions(
            &mut effective,
            collector.definitions(&value, "mcpServers", &source, "user", None),
            false,
        );
        if context.project.is_none() {
            if let Some(projects) = value.get("projects").and_then(Value::as_object) {
                for (path, project) in projects {
                    let root = Path::new(path);
                    if !root.is_absolute() {
                        continue;
                    }
                    for mut row in
                        collector.definitions(project, "mcpServers", &source, "project", Some(root))
                    {
                        row.disabled = contains_name(project, "disabledMcpServers", &row.name)
                            || contains_name(&value, "disabledMcpServers", &row.name);
                        collector.add(agent, row);
                    }
                }
            }
        }
        global = value;
    }
    let mut settings = Value::Object(Default::default());
    if let Some((value, _)) = collector.read(
        agent,
        &context.claude_home().join("settings.json"),
        "Claude Code user settings",
        "json",
    ) {
        merge_json(&mut settings, value);
    }
    if let Some(project) = context.project.as_ref() {
        if let Some((value, source)) = collector.read(
            agent,
            &project.join(".mcp.json"),
            "Claude Code project MCP configuration",
            "json",
        ) {
            insert_definitions(
                &mut effective,
                collector.definitions(&value, "mcpServers", &source, "project", Some(project)),
                false,
            );
        }
        if let Some(local) = global
            .get("projects")
            .and_then(Value::as_object)
            .and_then(|v| {
                v.iter()
                    .find(|(path, _)| normalized_path(Path::new(path)) == *project)
                    .map(|(_, value)| value)
            })
        {
            let source = collector.source(agent, Some(&config), "Claude Code MCP configuration");
            insert_definitions(
                &mut effective,
                collector.definitions(local, "mcpServers", &source, "project", Some(project)),
                false,
            );
            for row in effective.values_mut() {
                row.disabled |= contains_name(local, "disabledMcpServers", &row.name);
            }
        }
        for file in [".claude/settings.json", ".claude/settings.local.json"] {
            if let Some((value, _)) = collector.read(
                agent,
                &project.join(file),
                "Claude Code project settings",
                "json",
            ) {
                merge_json(&mut settings, value);
            }
        }
    }
    for row in effective.values_mut() {
        row.disabled |= contains_name(&global, "disabledMcpServers", &row.name)
            || contains_name(&settings, "disabledMcpServers", &row.name)
            || (row
                .source
                .path
                .as_ref()
                .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n == ".mcp.json"))
                && contains_name(&settings, "disabledMcpjsonServers", &row.name));
    }
    for (_, definition) in effective {
        collector.add(agent, definition);
    }
    scan_claude_plugins(context, collector, agent, &settings);
}

fn scan_claude_desktop(context: &Context, collector: &mut Collector) {
    // Chat's local launch directory is independent of a selected Code project.
    collector.execution_root = None;
    let agent = "claude-desktop";
    collector.inventory.agents.push(ConnectorAgent {
        id: agent.into(),
        label: "Claude Desktop".into(),
        clients: vec!["Chat".into(), "Local Code tab".into()],
    });
    let path = if cfg!(target_os = "macos") {
        context
            .home
            .join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(target_os = "windows") {
        context
            .env_path("APPDATA")
            .unwrap_or_else(|| context.home.join("AppData/Roaming"))
            .join("Claude/claude_desktop_config.json")
    } else {
        context
            .config_home()
            .join("Claude/claude_desktop_config.json")
    };
    if let Some((value, source)) = collector.read(
        agent,
        &path,
        "Claude Desktop local MCP configuration",
        "json",
    ) {
        for definition in collector.definitions(&value, "mcpServers", &source, "user", None) {
            collector.add(agent, definition);
        }
    } else if !collector
        .inventory
        .sources
        .iter()
        .any(|s| s.agent_id.as_deref() == Some(agent))
    {
        let source = collector.source(agent, Some(&path), "Claude Desktop local MCP configuration");
        let source = collector
            .inventory
            .sources
            .iter_mut()
            .find(|s| s.id == source.id)
            .unwrap();
        source.state = "unavailable".into();
        source.message = Some("No local MCP configuration found. Desktop extensions and account connectors may be configured separately.".into());
    }
}

fn scan_claude_plugins(
    context: &Context,
    collector: &mut Collector,
    agent: &str,
    settings: &Value,
) {
    let Some(enabled) = settings.get("enabledPlugins").and_then(Value::as_object) else {
        return;
    };
    let installed_path = context.claude_home().join("plugins/installed_plugins.json");
    let installed = collector.read(
        agent,
        &installed_path,
        "Claude Code installed plugins",
        "json",
    );
    let registry = installed.as_ref().map(|(v, _)| v);
    for (key, enabled_value) in enabled {
        let disabled = enabled_value.as_bool() == Some(false);
        if !enabled_value.is_boolean() {
            continue;
        }
        let mut locations = Vec::new();
        let registered = registry
            .and_then(|v| v.get("plugins"))
            .and_then(|v| v.get(key));
        if let Some(records) = registered {
            let records: Vec<_> = match records {
                Value::Array(a) => a.iter().collect(),
                Value::Object(_) => vec![records],
                _ => Vec::new(),
            };
            for record in records {
                let scope = record
                    .get("scope")
                    .and_then(Value::as_str)
                    .unwrap_or("user");
                let project = record
                    .get("projectPath")
                    .and_then(Value::as_str)
                    .map(|p| normalized_path(Path::new(p)));
                if scope != "user" && project.is_none() {
                    continue;
                }
                if let (Some(requested), Some(installed_project)) = (&context.project, &project) {
                    if requested != installed_project {
                        continue;
                    }
                }
                if let Some(path) = record.get("installPath").and_then(Value::as_str) {
                    locations.push((PathBuf::from(path), project));
                }
            }
        }
        // An authoritative registry record outside the selected project must
        // not reappear as a user-wide plugin merely because its cache exists.
        if locations.is_empty() && registered.is_none() {
            if let Some(path) = cached_plugin(
                context.claude_home().join("plugins/cache"),
                key,
                collector,
                agent,
            ) {
                locations.push((path, None));
            }
        }
        for (path, project) in locations {
            scan_plugin(
                collector,
                agent,
                &path,
                key,
                ".claude-plugin/plugin.json",
                PluginScope { disabled, project: project.as_deref(), overrides: None },
            );
        }
    }
}

fn scan_codex(context: &Context, collector: &mut Collector, agent: &str) {
    collector.execution_root = context.project.clone();
    let mut effective = BTreeMap::new();
    let mut config = Value::Object(Default::default());
    if let Some((value, source)) = collector.read(
        agent,
        &context.codex_home().join("config.toml"),
        "Codex user configuration",
        "toml",
    ) {
        insert_definitions(
            &mut effective,
            collector.definitions(&value, "mcp_servers", &source, "user", None),
            true,
        );
        merge_json(&mut config, value);
    }
    if let Some(project) = context.project.as_ref() {
        // Codex layers .codex/config.toml from the repository root to cwd.
        for root in project_ancestors(project, true) {
            if let Some((value, source)) = collector.read(
                agent,
                &root.join(".codex/config.toml"),
                "Codex project configuration",
                "toml",
            ) {
                insert_definitions(
                    &mut effective,
                    collector.definitions(&value, "mcp_servers", &source, "project", Some(&root)),
                    true,
                );
                merge_json(&mut config, value);
            }
        }
    }
    for (_, definition) in effective {
        collector.add(agent, definition);
    }
    let mut scanned_plugins = HashSet::new();
    if let Some(plugins) = config.get("plugins").and_then(Value::as_object) {
        for (key, options) in plugins {
            let Some(enabled) = options.get("enabled").and_then(Value::as_bool) else {
                continue;
            };
            let path = options
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .or_else(|| {
                    cached_plugin(
                        context.codex_home().join("plugins/cache"),
                        key,
                        collector,
                        agent,
                    )
                });
            if let Some(path) = path {
                scanned_plugins.insert(key.clone());
                scan_codex_plugin(
                    collector,
                    agent,
                    &path,
                    key,
                    !enabled,
                    context.project.as_deref(),
                    options.get("mcp_servers"),
                );
            }
        }
    }
    scan_codex_remote_plugin_files(context, collector, agent, &config, &scanned_plugins);
}

fn scan_codex_remote_plugin_files(
    context: &Context,
    collector: &mut Collector,
    agent: &str,
    config: &Value,
    scanned_plugins: &HashSet<String>,
) {
    let cache = context.codex_home().join("plugins/cache");
    // Only receipt-bearing plugin roots are observations. A downloaded catalog
    // or an old unmarked cache entry is not installation/configuration evidence.
    for marketplace in immediate_directories(&cache) {
        for plugin in immediate_directories(&marketplace) {
            let (Some(marketplace_name), Some(plugin_name)) = (
                marketplace.file_name().and_then(|s| s.to_str()),
                plugin.file_name().and_then(|s| s.to_str()),
            ) else {
                continue;
            };
            let key = format!("{plugin_name}@{marketplace_name}");
            if scanned_plugins.contains(&key) {
                continue;
            }
            let marker = plugin.join(".codex-remote-plugin-install.json");
            let label = format!("Local plugin record {}", clean_name(&key));
            let Some((receipt, source)) = collector.read(agent, &marker, &label, "json") else {
                continue;
            };
            let valid_id = receipt
                .get("remote_plugin_id")
                .and_then(Value::as_str)
                .and_then(|id| id.strip_prefix("plugin_"))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && !suffix.chars().any(char::is_whitespace)
                });
            if receipt.get("schema_version").and_then(Value::as_u64) != Some(1) || !valid_id {
                collector.issue(
                    &source,
                    "Remote plugin record has an unsupported structure.",
                );
                continue;
            }
            let options = config.get("plugins").and_then(|p| p.get(&key));
            let disabled = options
                .and_then(|o| o.get("enabled"))
                .and_then(Value::as_bool)
                == Some(false);
            // Explicit paths override default cache locations even if the path
            // is unusable. Never resurrect the default after an override fails.
            let path = match options.and_then(|o| o.get("path")) {
                Some(Value::String(path)) if Path::new(path).is_absolute() => {
                    Some(PathBuf::from(path))
                }
                Some(_) => {
                    collector.issue(
                        &source,
                        "Configured plugin path must be an absolute directory.",
                    );
                    None
                }
                None => cached_plugin(cache.clone(), &key, collector, agent),
            };
            let Some(path) = path else { continue };
            let prior_sources: HashSet<_> = collector
                .inventory
                .sources
                .iter()
                .map(|s| s.id.clone())
                .collect();
            scan_codex_plugin(
                collector,
                agent,
                &path,
                &key,
                disabled,
                context.project.as_deref(),
                options.and_then(|o| o.get("mcp_servers")),
            );
            let message = "Local plugin files were found; account access has not been checked.";
            if let Some(record) = collector
                .inventory
                .sources
                .iter_mut()
                .find(|s| s.id == source.id)
            {
                record.message = Some(message.into());
            }
            for discovered in &mut collector.inventory.sources {
                if !prior_sources.contains(&discovered.id) && discovered.state == "scanned" {
                    discovered.message = Some(message.into());
                }
            }
        }
    }
}

fn immediate_directories(path: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.take(4096).filter_map(Result::ok) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                directories.push(entry.path());
            }
        }
    }
    directories.sort();
    directories
}

fn scan_codex_plugin(
    collector: &mut Collector,
    agent: &str,
    root: &Path,
    key: &str,
    disabled: bool,
    project: Option<&Path>,
    overrides: Option<&Value>,
) {
    scan_plugin(
        collector,
        agent,
        root,
        key,
        ".codex-plugin/plugin.json",
        PluginScope { disabled, project, overrides },
    );
    let label = format!("Plugin {}", clean_name(key));
    let Some((manifest, manifest_source)) = collector.read(
        agent,
        &root.join(".codex-plugin/plugin.json"),
        &label,
        "json",
    ) else {
        return;
    };
    let Some(apps) = manifest.get("apps") else {
        return;
    };
    let display_name = manifest
        .get("interface")
        .and_then(|v| v.get("displayName"))
        .and_then(Value::as_str);
    let mut maps = Vec::new();
    match apps {
        Value::Object(_) => maps.push((serde_json::json!({"apps":apps}), manifest_source.clone())),
        Value::String(path) => {
            if let Some(path) = plugin_resource(root, path) {
                if let Some(pair) = collector.read(agent, &path, &label, "json") {
                    maps.push(pair);
                } else if !path.exists() {
                    collector.issue(
                        &manifest_source,
                        "A declared plugin app configuration file was not found.",
                    );
                }
            } else {
                collector.issue(
                    &manifest_source,
                    "Plugin app configuration path is outside the plugin directory.",
                );
            }
        }
        Value::Array(paths) => {
            for path in paths {
                if let Some(path) = path.as_str().and_then(|p| plugin_resource(root, p)) {
                    if let Some(pair) = collector.read(agent, &path, &label, "json") {
                        maps.push(pair);
                    } else if !path.exists() {
                        collector.issue(
                            &manifest_source,
                            "A declared plugin app configuration file was not found.",
                        );
                    }
                } else {
                    collector.issue(
                        &manifest_source,
                        "Plugin app configuration has an unsupported path.",
                    );
                }
            }
        }
        _ => collector.issue(
            &manifest_source,
            "Plugin app configuration has an unsupported structure.",
        ),
    }
    for (value, source) in maps {
        let Some(apps) = value.get("apps").and_then(Value::as_object) else {
            collector.issue(&source, "The plugin app map has an unsupported structure.");
            continue;
        };
        for (name, app) in apps {
            let Some(id) = app
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            else {
                collector.issue(
                    &source,
                    "One or more plugin app declarations lack an app ID.",
                );
                continue;
            };
            let name = app
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| (apps.len() == 1).then_some(display_name).flatten())
                .unwrap_or(name);
            let connector = ConnectorInfo {
                id: stable_id("app", &format!("{agent}:{id}")),
                name: clean_name(name),
                kind: "app".into(),
                host: None,
                managed_connection_ids: Vec::new(),
                availability: vec![ConnectorAvailability {
                    agent_id: agent.into(),
                    state: if disabled { "disabled" } else { "configured" }.into(),
                    scope: "plugin".into(),
                    project_path: project.map(|p| p.to_string_lossy().into_owned()),
                    source_id: source.id.clone(),
                    source_label: source.label.clone(),
                    source_path: source.path.clone(),
                    managed_connection_id: None,
                }],
            };
            collector.inventory.merge(ConnectorInventory {
                connectors: vec![connector],
                ..Default::default()
            });
        }
    }
}

fn cached_plugin(
    cache: PathBuf,
    key: &str,
    collector: &mut Collector,
    agent: &str,
) -> Option<PathBuf> {
    let (name, marketplace) = key.rsplit_once('@')?;
    // Components come from config, not trusted path fragments.
    if [name, marketplace]
        .iter()
        .any(|p| p.is_empty() || p.contains(['/', '\\']) || matches!(*p, "." | ".."))
    {
        return None;
    }
    let base = cache.join(marketplace).join(name);
    let mut candidates: Vec<_> = std::fs::read_dir(&base)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    candidates.sort();
    match candidates.len() {
        0 => None,
        1 => candidates.pop(),
        _ => {
            let source =
                collector.source(agent, Some(&base), &format!("Plugin {}", clean_name(key)));
            collector.issue(&source, "Multiple cached plugin versions exist; the active version cannot be determined passively.");
            None
        }
    }
}

struct PluginScope<'a> {
    disabled: bool,
    project: Option<&'a Path>,
    overrides: Option<&'a Value>,
}

fn scan_plugin(
    collector: &mut Collector,
    agent: &str,
    root: &Path,
    key: &str,
    manifest_path: &str,
    scope: PluginScope<'_>,
) {
    let PluginScope { disabled, project, overrides } = scope;
    if !root.is_absolute() {
        return;
    }
    let label = format!("Plugin {}", clean_name(key));
    let manifest = collector.read(agent, &root.join(manifest_path), &label, "json");
    let mut maps: Vec<(Value, SourceRef)> = Vec::new();
    if let Some((value, source)) = manifest {
        if let Some(config) = value.get("mcpServers") {
            match config {
                Value::Object(_) => maps.push((serde_json::json!({"mcpServers":config}), source)),
                Value::String(path) => {
                    if let Some(path) = plugin_resource(root, path) {
                        if let Some(pair) = collector.read(agent, &path, &label, "jsonc") {
                            maps.push(pair);
                        }
                    } else {
                        collector.issue(
                            &source,
                            "Plugin MCP configuration path is outside the plugin directory.",
                        );
                    }
                }
                Value::Array(paths) => {
                    for path in paths {
                        if let Some(path) = path.as_str().and_then(|p| plugin_resource(root, p)) {
                            if let Some(pair) = collector.read(agent, &path, &label, "jsonc") {
                                maps.push(pair);
                            }
                        } else {
                            collector.issue(
                                &source,
                                "Plugin MCP configuration has an unsupported path.",
                            );
                        }
                    }
                }
                _ => collector.issue(
                    &source,
                    "Plugin MCP configuration has an unsupported structure.",
                ),
            }
        }
    }
    let conventional = root.join(".mcp.json");
    if !maps
        .iter()
        .any(|(_, s)| s.path.as_deref() == conventional.to_str())
    {
        if let Some(pair) = collector.read(agent, &conventional, &label, "jsonc") {
            maps.insert(0, pair);
        }
    }
    let mut effective = BTreeMap::new();
    for (value, source) in maps {
        // Some plugin packages use the bare server map rather than the wrapper.
        let wrapped = if value.get("mcpServers").is_some() {
            value
        } else {
            serde_json::json!({"mcpServers":value})
        };
        insert_definitions(
            &mut effective,
            collector.definitions(&wrapped, "mcpServers", &source, "plugin", project),
            false,
        );
    }
    for (_, mut row) in effective {
        row.execution_root = collector
            .execution_root
            .clone()
            .or_else(|| project.map(Path::to_path_buf))
            .or_else(|| Some(root.to_path_buf()));
        row.disabled = disabled
            || overrides
                .and_then(|o| o.get(&row.name))
                .and_then(|o| o.get("enabled"))
                .and_then(Value::as_bool)
                == Some(false);
        // Identical relative commands in different plugins are distinct. Resolve
        // only the explicit plugin-root placeholder, never secrets/env values.
        if let Some(command) = row.value.get_mut("command") {
            replace_plugin_root(command, root);
        }
        if let Some(args) = row.value.get_mut("args") {
            replace_plugin_root(args, root);
        }
        collector.add(agent, row);
    }
}

fn plugin_resource(root: &Path, value: &str) -> Option<PathBuf> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let path = root.join(relative);
    // A manifest symlink must not turn inventory into arbitrary file reads.
    if let (Ok(root), Ok(path)) = (root.canonicalize(), path.canonicalize()) {
        if !path.starts_with(root) {
            return None;
        }
    }
    Some(path)
}

fn replace_plugin_root(value: &mut Value, root: &Path) {
    match value {
        Value::String(s) => {
            *s = s
                .replace("${CLAUDE_PLUGIN_ROOT}", &root.to_string_lossy())
                .replace("${CODEX_PLUGIN_ROOT}", &root.to_string_lossy())
        }
        Value::Array(a) => {
            for value in a {
                replace_plugin_root(value, root);
            }
        }
        _ => {}
    }
}

fn project_ancestors(project: &Path, stop_at_git: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for path in project.ancestors() {
        paths.push(path.to_path_buf());
        if stop_at_git && path.join(".git").exists() {
            break;
        }
    }
    paths.reverse();
    paths
}

fn scan_opencode(context: &Context, collector: &mut Collector, agent: &str) {
    collector.execution_root = context.project.clone();
    let mut effective = BTreeMap::new();
    let mut paths = Vec::new();
    let global = context.config_home().join("opencode");
    for file in ["opencode.json", "opencode.jsonc"] {
        paths.push((global.join(file), "user", None));
    }
    if let Some(path) = context.env_path("OPENCODE_CONFIG") {
        paths.push((path, "user", None));
    }
    if let Some(project) = context.project.as_ref() {
        // V2 applies all direct ancestor configs, then .opencode configs.
        let ancestors = project_ancestors(project, false);
        for prefix in ["", ".opencode"] {
            for root in &ancestors {
                for file in ["opencode.json", "opencode.jsonc"] {
                    paths.push((root.join(prefix).join(file), "project", Some(root.clone())));
                }
            }
        }
    }
    if let Some(root) = context.env_path("OPENCODE_CONFIG_DIR") {
        for file in ["opencode.json", "opencode.jsonc"] {
            paths.push((root.join(file), "user", None));
        }
    }
    for (path, scope, project) in paths {
        if let Some((value, source)) =
            collector.read(agent, &path, "OpenCode MCP configuration", "jsonc")
        {
            opencode_rows(
                collector,
                &mut effective,
                value,
                &source,
                scope,
                project.as_deref(),
            );
        }
    }
    if let Some(content) = context.env.get("OPENCODE_CONFIG_CONTENT") {
        let source = collector.source(agent, None, "OpenCode inline configuration");
        if let Some(value) = collector.parse(content, "jsonc", &source) {
            opencode_rows(collector, &mut effective, value, &source, "user", None);
        }
    }
    for (_, definition) in effective {
        collector.add(agent, definition);
    }
}

fn opencode_rows(
    collector: &mut Collector,
    effective: &mut BTreeMap<String, Definition>,
    value: Value,
    source: &SourceRef,
    scope: &str,
    project: Option<&Path>,
) {
    let Some(mcp) = value.get("mcp") else { return };
    let v2 = mcp.get("servers").is_some();
    let wrapped = if v2 {
        serde_json::json!({"mcpServers": mcp.get("servers")})
    } else {
        serde_json::json!({"mcpServers":mcp})
    };
    let mut rows = collector.definitions(&wrapped, "mcpServers", source, scope, project);
    for row in &mut rows {
        if v2 {
            // V2 has no enabled field. Ignore the obsolete key rather than
            // misreporting a valid V2 server disabled.
            if let Some(o) = row.value.as_object_mut() {
                o.remove("enabled");
            }
        }
    }
    insert_definitions(effective, rows, !v2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "vibestudio-connectors-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self {
                root: root.canonicalize().unwrap(),
            }
        }
        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path
        }
        fn context(&self, project: Option<&str>) -> Context {
            Context {
                home: self.root.clone(),
                project: project.map(|p| self.root.join(p)),
                env: HashMap::new(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn passive_projection_never_contains_secret_fields_or_complete_endpoints() {
        let fixture = Fixture::new();
        fixture.write(".cursor/mcp.json", &json!({"mcpServers":{
            "remote": {"url":"https://user:URL_SECRET@example.test/private/PATH_SECRET?key=QUERY_SECRET#FRAGMENT_SECRET", "headers":{"Authorization":"HEADER_SECRET"}, "env":{"TOKEN":"ENV_SECRET"}},
            "local": {"command":"COMMAND_SECRET", "args":["ARG_SECRET"], "env":{"TOKEN":"ENV_SECRET"}}
        }}).to_string());
        let mut collector = Collector::default();
        scan_json(
            &fixture.context(None),
            &mut collector,
            "cursor",
            ".cursor/mcp.json",
            ".cursor/mcp.json",
            "mcpServers",
        );
        let wire = serde_json::to_string(&collector.inventory).unwrap();
        for secret in [
            "URL_SECRET",
            "PATH_SECRET",
            "QUERY_SECRET",
            "FRAGMENT_SECRET",
            "HEADER_SECRET",
            "ENV_SECRET",
            "COMMAND_SECRET",
            "ARG_SECRET",
        ] {
            assert!(!wire.contains(secret), "Leaked {secret}");
        }
        assert_eq!(collector.inventory.connectors.len(), 2);
        assert_eq!(
            collector
                .inventory
                .connectors
                .iter()
                .find(|c| c.name == "remote")
                .unwrap()
                .host
                .as_deref(),
            Some("example.test")
        );
        assert!(collector
            .inventory
            .connectors
            .iter()
            .all(|c| c.availability[0].state == "configured"));
    }

    #[test]
    fn identities_preserve_resources_and_normalize_equivalent_launch_shapes() {
        assert_eq!(
            remote_identity("HTTPS://EXAMPLE.test:443/a?account=1#x"),
            remote_identity("https://example.test/a?account=1#y")
        );
        assert_ne!(
            remote_identity("https://example.test/a?account=1"),
            remote_identity("https://example.test/a?account=2")
        );
        assert_ne!(
            remote_identity("https://one:secret@example.test/a"),
            remote_identity("https://two:secret@example.test/a")
        );
        assert_eq!(
            local_identity(&json!(["npx", "-y", "server"]), None),
            local_identity(&json!("npx"), Some(&json!(["-y", "server"])))
        );
        assert_eq!(
            safe_host("https://user:password@example.test:8080/secret?token=secret").as_deref(),
            Some("example.test:8080")
        );
        assert!(safe_host("https://${HOST}/mcp").is_none());
    }

    #[test]
    fn claude_selected_project_uses_local_precedence_and_disabled_status() {
        let fixture = Fixture::new();
        let project = fixture.root.join("project");
        fixture.write(".claude.json", &json!({"mcpServers":{"same":{"url":"https://user.test/mcp"},"other":{"url":"https://other.test/mcp"}},"projects":{project.to_string_lossy().as_ref():{"mcpServers":{"same":{"url":"https://local.test/mcp"}},"disabledMcpServers":["same"]}}}).to_string());
        fixture.write(
            "project/.mcp.json",
            r#"{"mcpServers":{"same":{"url":"https://project.test/mcp"}}}"#,
        );
        let mut collector = Collector::default();
        scan_claude(&fixture.context(Some("project")), &mut collector, "claude");
        assert_eq!(collector.inventory.connectors.len(), 2);
        let same = collector
            .inventory
            .connectors
            .iter()
            .find(|c| c.name == "same")
            .unwrap();
        assert_eq!(same.host.as_deref(), Some("local.test"));
        assert_eq!(same.availability[0].state, "disabled");
        assert_eq!(same.availability[0].scope, "project");
        assert_eq!(
            same.availability[0].project_path.as_deref(),
            project.to_str()
        );
        assert!(!collector
            .inventory
            .connectors
            .iter()
            .any(|c| matches!(c.host.as_deref(), Some("user.test" | "project.test"))));
    }

    #[test]
    fn claude_config_override_and_remembered_project_metadata_are_respected() {
        let fixture = Fixture::new();
        let project = fixture.root.join("remembered");
        fixture.write(
            ".claude.json",
            r#"{"mcpServers":{"wrong":{"url":"https://wrong.test"}}}"#,
        );
        fixture.write("isolated/.claude.json", &json!({"projects":{project.to_string_lossy().as_ref():{"mcpServers":{"right":{"command":"mcp","args":[]}}}}}).to_string());
        let mut context = fixture.context(None);
        context.env.insert(
            "CLAUDE_CONFIG_DIR".into(),
            fixture.root.join("isolated").to_string_lossy().into_owned(),
        );
        let mut collector = Collector::default();
        scan_claude(&context, &mut collector, "claude");
        assert_eq!(collector.inventory.connectors.len(), 1);
        assert_eq!(collector.inventory.connectors[0].name, "right");
        assert_eq!(
            collector.inventory.connectors[0].availability[0]
                .project_path
                .as_deref(),
            project.to_str()
        );
    }

    #[test]
    fn claude_plugins_respect_install_scope_and_project_enable_overrides() {
        let fixture = Fixture::new();
        let plugin = fixture.root.join("installed/plugin");
        let other_project = fixture.root.join("other");
        fixture.write(
            ".claude/settings.json",
            r#"{"enabledPlugins":{"calendar@market":true,"other@market":true}}"#,
        );
        fixture.write(
            "project/.claude/settings.local.json",
            r#"{"enabledPlugins":{"calendar@market":false}}"#,
        );
        fixture.write(".claude/plugins/installed_plugins.json", &json!({"version":2,"plugins":{
            "calendar@market":[{"scope":"user","installPath":plugin}],
            "other@market":[{"scope":"project","projectPath":other_project,"installPath":fixture.root.join("wrong-plugin")}]
        }}).to_string());
        fixture.write(
            "installed/plugin/.claude-plugin/plugin.json",
            r#"{"name":"calendar","mcpServers":"./server.json"}"#,
        );
        fixture.write(
            "installed/plugin/server.json",
            r#"{"mcpServers":{"calendar":{"url":"https://calendar.test/mcp"}}}"#,
        );
        fixture.write(
            "wrong-plugin/.mcp.json",
            r#"{"mcpServers":{"wrong":{"url":"https://wrong.test"}}}"#,
        );
        fixture.write(
            ".claude/plugins/cache/market/other/1/.mcp.json",
            r#"{"mcpServers":{"wrong-cached":{"url":"https://wrong-cached.test"}}}"#,
        );
        let mut collector = Collector::default();
        scan_claude(&fixture.context(Some("project")), &mut collector, "claude");
        assert_eq!(collector.inventory.connectors.len(), 1);
        let row = &collector.inventory.connectors[0].availability[0];
        assert_eq!(row.state, "disabled");
        assert_eq!(row.scope, "plugin");
        assert!(row.source_path.as_deref().unwrap().ends_with("server.json"));
    }

    #[test]
    fn codex_toml_project_overlay_and_plugin_server_disable_are_honored() {
        let fixture = Fixture::new();
        fixture.write(".codex/config.toml", "[mcp_servers.example]\nurl = 'https://example.test/mcp'\n[plugins.'tools@market']\nenabled = true\n[plugins.'tools@market'.mcp_servers.calendar]\nenabled = false\n");
        fixture.write("project/.git/HEAD", "ref: refs/heads/main\n");
        fixture.write(
            "project/.codex/config.toml",
            "[mcp_servers.example]\nenabled = false\n",
        );
        fixture.write(
            ".codex/plugins/cache/market/tools/1.0/.codex-plugin/plugin.json",
            r#"{"name":"tools"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/market/tools/1.0/.mcp.json",
            r#"{"mcpServers":{"calendar":{"url":"https://calendar.test/mcp"}}}"#,
        );
        let mut collector = Collector::default();
        scan_codex(&fixture.context(Some("project")), &mut collector, "codex");
        assert_eq!(collector.inventory.connectors.len(), 2);
        assert!(collector
            .inventory
            .connectors
            .iter()
            .all(|c| c.availability[0].state == "disabled"));
        assert_eq!(
            collector
                .inventory
                .connectors
                .iter()
                .find(|c| c.name == "example")
                .unwrap()
                .host
                .as_deref(),
            Some("example.test")
        );
        assert_eq!(
            collector
                .inventory
                .connectors
                .iter()
                .find(|c| c.name == "calendar")
                .unwrap()
                .availability[0]
                .scope,
            "plugin"
        );
    }

    #[test]
    fn codex_app_only_plugins_use_runtime_identity_and_preserve_safe_metadata() {
        let fixture = Fixture::new();
        fixture.write(".codex/config.toml", "[plugins.'mail@market']\nenabled = true\n[plugins.'calendar@market']\nenabled = false\n");
        fixture.write(".codex/plugins/cache/market/mail/1/.codex-plugin/plugin.json", r#"{"name":"mail","apps":"./.app.json","interface":{"displayName":"Example Mail"},"secret":"MANIFEST_SECRET"}"#);
        fixture.write(".codex/plugins/cache/market/mail/1/.app.json", r#"{"apps":{"mail":{"id":"connector_fixture_mail","required":true,"token":"APP_SECRET"}}}"#);
        fixture.write(".codex/plugins/cache/market/calendar/1/.codex-plugin/plugin.json", r#"{"name":"calendar","apps":"./.app.json","interface":{"displayName":"Example Calendar"}}"#);
        fixture.write(
            ".codex/plugins/cache/market/calendar/1/.app.json",
            r#"{"apps":{"calendar":{"id":"connector_fixture_calendar"}}}"#,
        );
        let mut collector = Collector::default();
        scan_codex(&fixture.context(None), &mut collector, "codex");
        assert_eq!(collector.inventory.connectors.len(), 2);
        let mail = collector
            .inventory
            .connectors
            .iter()
            .find(|c| c.name == "Example Mail")
            .unwrap();
        assert_eq!(mail.id, stable_id("app", "codex:connector_fixture_mail"));
        assert_eq!(mail.kind, "app");
        assert_eq!(mail.availability[0].state, "configured");
        assert_eq!(mail.availability[0].scope, "plugin");
        assert!(mail.availability[0]
            .source_path
            .as_deref()
            .unwrap()
            .ends_with(".app.json"));
        assert_eq!(
            collector
                .inventory
                .connectors
                .iter()
                .find(|c| c.name == "Example Calendar")
                .unwrap()
                .availability[0]
                .state,
            "disabled"
        );
        let wire = serde_json::to_string(&collector.inventory).unwrap();
        assert!(!wire.contains("MANIFEST_SECRET"));
        assert!(!wire.contains("APP_SECRET"));
        assert!(!wire.contains("connector_fixture_mail"));

        let mut runtime = mail.clone();
        runtime.availability[0].source_id = "runtime:codex/apps".into();
        runtime.availability[0].scope = "account".into();
        collector.inventory.merge(ConnectorInventory {
            connectors: vec![runtime],
            ..Default::default()
        });
        assert_eq!(collector.inventory.connectors.len(), 2);
        assert_eq!(
            collector
                .inventory
                .connectors
                .iter()
                .find(|c| c.name == "Example Mail")
                .unwrap()
                .availability
                .len(),
            2
        );
    }

    #[test]
    fn codex_app_manifests_keep_per_app_names_and_reject_invalid_references() {
        let fixture = Fixture::new();
        fixture.write("plugin/.codex-plugin/plugin.json", r#"{"name":"bundle","apps":["./.app.json","../outside.json"],"interface":{"displayName":"Whole Bundle"}}"#);
        fixture.write("plugin/.app.json", r#"{"apps":{"mail":{"id":"connector_mail","name":"Mail Service"},"calendar":{"id":"connector_calendar"},"invalid":{"token":"INVALID_SECRET"}}}"#);
        let mut collector = Collector::default();
        scan_codex_plugin(
            &mut collector,
            "codex",
            &fixture.root.join("plugin"),
            "bundle@market",
            false,
            None,
            None,
        );
        let names: Vec<_> = collector
            .inventory
            .connectors
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["calendar", "Mail Service"]);
        assert!(collector
            .inventory
            .sources
            .iter()
            .any(|s| s.message.as_deref()
                == Some("Plugin app configuration has an unsupported path.")));
        assert!(collector
            .inventory
            .sources
            .iter()
            .any(|s| s.message.as_deref()
                == Some("One or more plugin app declarations lack an app ID.")));
        assert!(!serde_json::to_string(&collector.inventory)
            .unwrap()
            .contains("INVALID_SECRET"));
    }

    #[test]
    fn codex_remote_plugin_receipt_discovers_app_without_config_and_excludes_unmarked_cache() {
        let fixture = Fixture::new();
        fixture.write(
            ".codex/plugins/cache/remote/mail/.codex-remote-plugin-install.json",
            r#"{"schema_version":1,"remote_plugin_id":"plugin_connector_fixture"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/mail/1/.codex-plugin/plugin.json",
            r#"{"name":"mail","apps":"./.app.json","interface":{"displayName":"Example Mail"}}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/mail/1/.app.json",
            r#"{"apps":{"mail":{"id":"connector_mail"}}}"#,
        );
        fixture.write(
            ".codex/plugins/cache/catalog/uninstalled/1/.codex-plugin/plugin.json",
            r#"{"name":"uninstalled","apps":"./.app.json"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/catalog/uninstalled/1/.app.json",
            r#"{"apps":{"uninstalled":{"id":"connector_not_installed"}}}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/invalid/.codex-remote-plugin-install.json",
            r#"{"schema_version":2,"remote_plugin_id":"plugin_connector_invalid"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/invalid/1/.codex-plugin/plugin.json",
            r#"{"name":"invalid","apps":"./.app.json"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/invalid/1/.app.json",
            r#"{"apps":{"invalid":{"id":"connector_invalid"}}}"#,
        );
        let mut collector = Collector::default();
        scan_codex(&fixture.context(None), &mut collector, "codex");
        assert_eq!(collector.inventory.connectors.len(), 1);
        let mail = &collector.inventory.connectors[0];
        assert_eq!(mail.name, "Example Mail");
        assert_eq!(mail.id, stable_id("app", "codex:connector_mail"));
        assert_eq!(mail.availability[0].state, "configured");
        assert!(collector
            .inventory
            .sources
            .iter()
            .any(|s| s.id == mail.availability[0].source_id
                && s.message.as_deref()
                    == Some(
                        "Local plugin files were found; account access has not been checked."
                    )));
        assert!(collector
            .inventory
            .sources
            .iter()
            .any(|s| s.state == "error"
                && s.message.as_deref()
                    == Some("Remote plugin record has an unsupported structure.")));
    }

    #[test]
    fn codex_receipts_respect_explicit_disable_and_path_override() {
        let fixture = Fixture::new();
        let custom = fixture.root.join("custom");
        fixture.write(
            ".codex/config.toml",
            &format!(
                "[plugins.'mail@remote']\nenabled = false\npath = '{}'\n",
                custom.to_string_lossy()
            ),
        );
        fixture.write(
            ".codex/plugins/cache/remote/mail/.codex-remote-plugin-install.json",
            r#"{"schema_version":1,"remote_plugin_id":"plugin_connector_fixture"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/mail/1/.codex-plugin/plugin.json",
            r#"{"name":"mail","apps":"./.app.json"}"#,
        );
        fixture.write(
            ".codex/plugins/cache/remote/mail/1/.app.json",
            r#"{"apps":{"wrong-default":{"id":"connector_wrong_default"}}}"#,
        );
        fixture.write(
            "custom/.codex-plugin/plugin.json",
            r#"{"name":"mail","apps":"./.app.json"}"#,
        );
        fixture.write(
            "custom/.app.json",
            r#"{"apps":{"mail":{"id":"connector_custom"}}}"#,
        );
        let mut collector = Collector::default();
        scan_codex(&fixture.context(None), &mut collector, "codex");
        assert_eq!(collector.inventory.connectors.len(), 1);
        assert_eq!(
            collector.inventory.connectors[0].id,
            stable_id("app", "codex:connector_custom")
        );
        assert_eq!(
            collector.inventory.connectors[0].availability[0].state,
            "disabled"
        );
    }

    #[test]
    fn opencode_jsonc_v1_overlay_and_v2_disabled_semantics() {
        let fixture = Fixture::new();
        fixture.write(
            ".config/opencode/opencode.jsonc",
            r#"{
            // Keep the URL intact.
            "mcp":{"example":{"type":"remote","url":"https://global.test/mcp", "enabled":true,},},
        }"#,
        );
        fixture.write(
            "project/opencode.json",
            r#"{"mcp":{"example":{"enabled":false}}}"#,
        );
        let mut collector = Collector::default();
        scan_opencode(
            &fixture.context(Some("project")),
            &mut collector,
            "opencode",
        );
        assert_eq!(
            collector.inventory.connectors[0].host.as_deref(),
            Some("global.test")
        );
        assert_eq!(
            collector.inventory.connectors[0].availability[0].state,
            "disabled"
        );
        fixture.write("project/.opencode/opencode.jsonc", r#"{"mcp":{"servers":{"example":{"type":"remote","url":"https://v2.test/mcp","disabled":false,"enabled":false}}}}"#);
        let mut collector = Collector::default();
        scan_opencode(
            &fixture.context(Some("project")),
            &mut collector,
            "opencode",
        );
        assert_eq!(collector.inventory.connectors.len(), 1);
        assert_eq!(
            collector.inventory.connectors[0].host.as_deref(),
            Some("v2.test")
        );
        assert_eq!(
            collector.inventory.connectors[0].availability[0].state,
            "configured"
        );
    }

    #[test]
    fn source_failures_are_visible_and_do_not_echo_bad_content() {
        let fixture = Fixture::new();
        fixture.write(".cursor/mcp.json", r#"{"mcpServers": "MALFORMED_SECRET"}"#);
        fixture.write(
            "project/.cursor/mcp.json",
            r#"{"TOKEN":"SYNTAX_SECRET" invalid}"#,
        );
        let mut collector = Collector::default();
        scan_json(
            &fixture.context(Some("project")),
            &mut collector,
            "cursor",
            ".cursor/mcp.json",
            ".cursor/mcp.json",
            "mcpServers",
        );
        assert_eq!(collector.inventory.sources.len(), 2);
        assert!(collector
            .inventory
            .sources
            .iter()
            .all(|s| s.state == "error"));
        let wire = serde_json::to_string(&collector.inventory).unwrap();
        assert!(!wire.contains("MALFORMED_SECRET"));
        assert!(!wire.contains("SYNTAX_SECRET"));
    }

    #[test]
    fn claude_global_disabled_flags_apply_without_a_selected_project() {
        let fixture = Fixture::new();
        fixture.write(".claude.json", r#"{"mcpServers":{"paused":{"url":"https://paused.test/mcp"}},"disabledMcpServers":["paused"]}"#);
        let mut collector = Collector::default();
        scan_claude(&fixture.context(None), &mut collector, "claude");
        assert_eq!(
            collector.inventory.connectors[0].availability[0].state,
            "disabled"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_config_does_not_block_discovery() {
        let fixture = Fixture::new();
        let path = fixture.root.join("fifo.json");
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let mut collector = Collector::default();
        assert!(collector.read("claude", &path, "fixture", "json").is_none());
        assert_eq!(collector.inventory.sources[0].state, "error");
    }

    #[test]
    fn cached_plugin_ambiguity_is_reported_without_guessing() {
        let fixture = Fixture::new();
        fixture.write("cache/market/plugin/1/.mcp.json", "{}");
        fixture.write("cache/market/plugin/2/.mcp.json", "{}");
        let mut collector = Collector::default();
        assert!(cached_plugin(
            fixture.root.join("cache"),
            "plugin@market",
            &mut collector,
            "codex"
        )
        .is_none());
        assert_eq!(collector.inventory.sources[0].state, "error");
        assert!(cached_plugin(
            fixture.root.join("cache"),
            "../plugin@market",
            &mut collector,
            "codex"
        )
        .is_none());
    }

    #[test]
    fn relative_local_launches_keep_project_and_unknown_plugin_roots_distinct() {
        let fixture = Fixture::new();
        let a = fixture.root.join("a");
        let b = fixture.root.join("b");
        fixture.write(".claude.json", &json!({"projects":{
            a.to_string_lossy().as_ref(): {"mcpServers":{"local":{"command":"node","args":["server.js"]}}},
            b.to_string_lossy().as_ref(): {"mcpServers":{"local":{"command":"node","args":["server.js"]}}}
        }}).to_string());
        let mut collector = Collector::default();
        scan_claude(&fixture.context(None), &mut collector, "claude");
        assert_eq!(collector.inventory.connectors.len(), 2);
        assert_ne!(
            collector.inventory.connectors[0].id,
            collector.inventory.connectors[1].id
        );

        fixture.write(
            "first/.mcp.json",
            r#"{"mcpServers":{"local":{"command":"node","args":["server.js"]}}}"#,
        );
        fixture.write(
            "second/.mcp.json",
            r#"{"mcpServers":{"local":{"command":"node","args":["server.js"]}}}"#,
        );
        let mut collector = Collector::default();
        for plugin in ["first", "second"] {
            scan_plugin(
                &mut collector,
                "claude",
                &fixture.root.join(plugin),
                plugin,
                ".claude-plugin/plugin.json",
                PluginScope { disabled: false, project: None, overrides: None },
            );
        }
        assert_eq!(collector.inventory.connectors.len(), 2);
    }

    #[test]
    fn selected_project_identity_matches_runtime_shape_and_explicit_cwd() {
        let fixture = Fixture::new();
        fixture.write(
            "project/.cursor/mcp.json",
            r#"{"mcpServers":{"local":{"command":"node","args":["server.js"],"cwd":"../worker"}}}"#,
        );
        fixture.write("worker/server.js", "fixture");
        let context = fixture.context(Some("project"));
        let mut collector = Collector::default();
        scan_json(
            &context,
            &mut collector,
            "cursor",
            ".cursor/mcp.json",
            ".cursor/mcp.json",
            "mcpServers",
        );
        let runtime_config = json!({"command":["node","server.js"],"cwd":"../worker"});
        let runtime_root = local_execution_root(&runtime_config, context.project.as_deref());
        assert_eq!(
            collector.inventory.connectors[0].id,
            local_identity_in(&runtime_config["command"], None, runtime_root.as_deref())
        );
        assert_eq!(runtime_root, Some(fixture.root.join("worker")));
        assert_eq!(
            local_execution_root(&json!({"cwd":"unknown-relative"}), None),
            Some(PathBuf::from("unknown-relative"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_project_alias_is_canonical_and_matches_claude_scope() {
        let fixture = Fixture::new();
        fixture.write("project/.mcp.json", "{}");
        let alias = fixture.root.join("alias");
        std::os::unix::fs::symlink(fixture.root.join("project"), &alias).unwrap();
        let canonical = canonical_project(alias.to_str()).unwrap().unwrap();
        assert_eq!(canonical, fixture.root.join("project"));
        fixture.write(".claude.json", &json!({"projects":{alias.to_string_lossy().as_ref():{"mcpServers":{"local":{"url":"https://example.test"}}}}}).to_string());
        let mut context = fixture.context(None);
        context.project = Some(canonical.clone());
        let mut collector = Collector::default();
        scan_claude(&context, &mut collector, "claude");
        assert_eq!(collector.inventory.connectors.len(), 1);
        assert_eq!(
            collector.inventory.connectors[0].availability[0]
                .project_path
                .as_deref(),
            canonical.to_str()
        );
        assert!(canonical_project(Some("relative/project")).is_err());
    }

    #[test]
    fn managed_gateway_has_agent_availability_only_when_observed() {
        let mut collector = Collector::default();
        let managed_id = "private-slug";
        collector.managed.insert(
            managed_id.into(),
            (stable_id("managed", managed_id), "needs_auth".into()),
        );
        let source = collector.source("claude", None, "fixture");
        collector.add(
            "claude",
            Definition {
                name: "service".into(),
                value: json!({"url":"http://127.0.0.1:1234/gw/private-slug/mcp"}),
                source,
                scope: "user".into(),
                project: None,
                disabled: false,
                execution_root: None,
            },
        );
        let connector = &collector.inventory.connectors[0];
        assert_eq!(connector.id, stable_id("managed", managed_id));
        assert_eq!(connector.managed_connection_ids, vec![managed_id]);
        assert_eq!(connector.availability[0].agent_id, "claude");
        assert_eq!(connector.availability[0].state, "needs_auth");
        assert_eq!(
            connector.availability[0].managed_connection_id.as_deref(),
            Some(managed_id)
        );
    }

    #[test]
    fn jsonc_strings_and_invalid_comments() {
        assert_eq!(parse_jsonc(r#"{/*comment*/"url":"https://host.test/a//b", "text":"escaped\"quote", "a":[1,2,],}"#).unwrap()["url"], "https://host.test/a//b");
        assert!(parse_jsonc("{/* unterminated").is_none());
        assert!(parse_jsonc("{unquoted: true}").is_none());
    }
}
