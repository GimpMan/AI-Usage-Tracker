//! Kimi Code usage provider (Moonshot / auth.kimi.com / api.kimi.com).
//!
//! Auth: app OAuth session in Windows Credential Manager (`oauth_kimi`).
//! CLI credentials under `$KIMI_CODE_HOME` are separate and only used for a
//! one-time import when the app has no session yet.
//! Usage: `GET {KIMI_CODE_BASE_URL}/usages` with Bearer only (no X-Msh headers).

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::{self, Secrets};

const PROVIDER_LABEL: &str = "Kimi Code";
const PROVIDER_ID: &str = "kimi";

/// Public OAuth client id (official Kimi Code CLI; no secret).
pub(crate) const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
pub(crate) const DEFAULT_OAUTH_HOST: &str = "https://auth.kimi.com";
const DEFAULT_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const LIVE_TIMEOUT: Duration = Duration::from_secs(12);

/// Must begin with `no auth` so `classify_snapshot` marks missing auth.
const REASON_NO_AUTH: &str = "no auth found — sign in with Kimi";
const REASON_EXPIRED: &str = "session expired — sign in with Kimi";
const REASON_NETWORK: &str = "network error";
const REASON_DECODE: &str = "decode error";

/// Settings badge when credential file is missing/unreadable.
pub(crate) const REASON_NO_AUTH_STATUS: &str = "Sign in with Kimi to configure";

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub struct KimiProvider;

#[async_trait]
impl Provider for KimiProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, _secrets: &Secrets) -> ProviderFetch {
        match fetch_live().await {
            Ok(snap) => classify_snapshot(snap),
            Err(e) => classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, e)),
        }
    }
}

// ============================================================
// Paths & credentials (app CM + optional one-time CLI import)
// ============================================================

pub(crate) fn kimi_code_home() -> Result<PathBuf, String> {
    super::cli_home_dir("KIMI_CODE_HOME", ".kimi-code")
}

/// Legacy CLI credentials path (import only; app never writes here).
pub(crate) fn credentials_path() -> Result<PathBuf, String> {
    Ok(kimi_code_home()?.join("credentials").join("kimi-code.json"))
}

pub(crate) fn device_id_path() -> Result<PathBuf, String> {
    Ok(kimi_code_home()?.join("device_id"))
}

/// OAuth host: non-empty `KIMI_CODE_OAUTH_HOST`, then `KIMI_OAUTH_HOST`, else default.
pub(crate) fn oauth_host() -> String {
    for var in ["KIMI_CODE_OAUTH_HOST", "KIMI_OAUTH_HOST"] {
        if let Ok(v) = std::env::var(var) {
            let t = v.trim();
            if !t.is_empty() {
                return t.trim_end_matches('/').to_string();
            }
        }
    }
    DEFAULT_OAUTH_HOST.to_string()
}

pub(crate) fn base_url() -> String {
    if let Ok(v) = std::env::var("KIMI_CODE_BASE_URL") {
        let t = v.trim();
        if !t.is_empty() {
            return t.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_BASE_URL.to_string()
}

/// Stable device id at `$KIMI_CODE_HOME/device_id` (create if missing). Never delete on logout.
pub(crate) fn load_or_create_device_id() -> String {
    let Ok(path) = device_id_path() else {
        return fallback_device_id();
    };
    if let Ok(raw) = std::fs::read_to_string(&path) {
        let id = raw.trim();
        if !id.is_empty() && id.is_ascii() {
            return id.to_string();
        }
    }
    let id = fallback_device_id();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &id);
    id
}

fn fallback_device_id() -> String {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Headers used on device/token OAuth requests (ASCII-safe; no secrets).
pub(crate) fn device_headers() -> Vec<(&'static str, String)> {
    let device_id = load_or_create_device_id();
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "ai-usage-tracker".into());
    let hostname = ascii_safe(&hostname, "pc");
    vec![
        ("X-Msh-Platform", "kimi_code_cli".into()),
        ("X-Msh-Version", env!("CARGO_PKG_VERSION").into()),
        ("X-Msh-Device-Name", hostname.clone()),
        ("X-Msh-Device-Model", "ai-usage-tracker".into()),
        ("X-Msh-Os-Version", "windows".into()),
        ("X-Msh-Device-Id", device_id),
    ]
}

fn ascii_safe(s: &str, fallback: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(64)
        .collect();
    let t = cleaned.trim();
    if t.is_empty() {
        fallback.into()
    } else {
        t.to_string()
    }
}

pub(crate) struct TokenPersist<'a> {
    pub access_token: &'a str,
    pub refresh_token: Option<&'a str>,
    pub expires_in: u64,
    pub expires_at: Option<i64>,
    pub scope: Option<&'a str>,
    pub token_type: Option<&'a str>,
}

/// Persist Kimi tokens in Windows Credential Manager (app-only).
pub(crate) fn persist_credential_tokens(tokens: &TokenPersist<'_>) -> Result<(), String> {
    let mut doc: Value = secrets::oauth_get_json("kimi").unwrap_or_else(|| json!({}));
    let obj = match doc.as_object_mut() {
        Some(o) => o,
        None => {
            doc = json!({});
            doc.as_object_mut()
                .ok_or_else(|| "kimi credentials not an object".to_string())?
        }
    };

    obj.insert(
        "access_token".into(),
        Value::String(tokens.access_token.to_string()),
    );
    if let Some(rt) = tokens.refresh_token.filter(|s| !s.is_empty()) {
        obj.insert("refresh_token".into(), Value::String(rt.to_string()));
    }
    let expires_at = tokens
        .expires_at
        .unwrap_or_else(|| Utc::now().timestamp() + tokens.expires_in as i64);
    obj.insert("expires_at".into(), json!(expires_at));
    obj.insert("expires_in".into(), json!(tokens.expires_in));
    if let Some(s) = tokens.scope.filter(|s| !s.is_empty()) {
        obj.insert("scope".into(), Value::String(s.to_string()));
    }
    if let Some(t) = tokens.token_type.filter(|s| !s.is_empty()) {
        obj.insert("token_type".into(), Value::String(t.to_string()));
    } else {
        obj.insert("token_type".into(), Value::String("Bearer".into()));
    }

    secrets::oauth_set_json("kimi", &doc).map_err(|e| format!("store kimi oauth: {e}"))?;
    log::info!("kimi oauth: stored session in Credential Manager");
    Ok(())
}

/// Remove the app OAuth session from Credential Manager.
pub(crate) fn remove_credential_file() -> Result<bool, String> {
    secrets::oauth_delete("kimi").map_err(|e| e.to_string())
}

fn read_credential_doc() -> Result<Value, String> {
    let legacy = credentials_path().ok();
    let doc = match legacy.as_ref() {
        Some(path) => secrets::oauth_get_json_or_import_file("kimi", path),
        None => secrets::oauth_get_json("kimi"),
    };
    doc.ok_or_else(|| REASON_NO_AUTH.to_string())
}

struct AuthSession {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

impl AuthSession {
    fn is_expired(&self, now_unix: i64) -> bool {
        self.expires_at
            .map(|expires| expires <= now_unix)
            .unwrap_or(false)
    }
}

fn load_auth_session() -> Result<AuthSession, String> {
    let doc = read_credential_doc()?;
    session_from_doc(&doc)
}

fn session_from_doc(doc: &Value) -> Result<AuthSession, String> {
    let access = non_empty_str(doc, "access_token")
        .map(str::to_string)
        .ok_or_else(|| REASON_NO_AUTH.to_string())?;
    let refresh = non_empty_str(doc, "refresh_token").map(str::to_string);
    let expires_at = parse_expires_at_unix(doc);
    Ok(AuthSession {
        access_token: access,
        refresh_token: refresh,
        expires_at,
    })
}

// ============================================================
// Auth classification (Settings)
// ============================================================

/// Classify Kimi OAuth credential JSON for Settings badge / auth status.
///
/// Wire fields: `access_token`, `refresh_token`, `expires_at` (Unix seconds).
/// Never includes token content or raw JSON in reason strings.
///
/// Returns `(configured, optional non-secret reason when not configured)`.
pub(crate) fn classify_auth_status(doc: &Value) -> (bool, Option<String>) {
    let access = non_empty_str(doc, "access_token");
    let refresh = non_empty_str(doc, "refresh_token");

    // Refreshable credentials count as configured even if access is expired/absent.
    if refresh.is_some() {
        return (true, None);
    }

    if let Some(_access) = access {
        if access_token_usable(doc) {
            return (true, None);
        }
        return (
            false,
            Some("Access token expired; re-auth required".to_string()),
        );
    }

    (false, Some("Sign in with Kimi to configure".to_string()))
}

fn non_empty_str<'a>(doc: &'a Value, key: &str) -> Option<&'a str> {
    doc.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// True when `expires_at` is missing, unparseable, or strictly after now.
/// False when parseable and `expires_at <= now` (expired at/before current Unix seconds).
fn access_token_usable(doc: &Value) -> bool {
    let Some(expires_at) = parse_expires_at_unix(doc) else {
        // Missing or invalidly unparseable → treat as non-expiring / usable.
        return true;
    };
    let now = chrono::Utc::now().timestamp();
    expires_at > now
}

fn parse_expires_at_unix(doc: &Value) -> Option<i64> {
    let v = doc.get("expires_at")?;
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(n) = v.as_u64() {
        return i64::try_from(n).ok();
    }
    if let Some(n) = v.as_f64() {
        if n.is_finite() {
            return Some(n as i64);
        }
        return None;
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<i64>().ok();
    }
    None
}

/// Positive `expires_in` from a refresh token JSON object.
///
/// Accepts unsigned/signed numbers, finite positive floats, or numeric strings.
/// Rejects missing, zero, negative, non-finite, and non-numeric values. Error
/// text never includes raw token JSON.
fn parse_refresh_expires_in(doc: &Value) -> Result<u64, String> {
    let Some(v) = doc.get("expires_in") else {
        return Err(format!(
            "{REASON_DECODE}: refresh missing or non-positive expires_in"
        ));
    };
    let n = v
        .as_u64()
        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| {
            v.as_f64()
                .filter(|f| f.is_finite() && *f > 0.0)
                .map(|f| f as u64)
        })
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("{REASON_DECODE}: refresh missing or non-positive expires_in"))?;
    Ok(n)
}

// ============================================================
// Refresh + fetch
// ============================================================

async fn refresh_access_token(
    client: &reqwest::Client,
    session: &AuthSession,
) -> Result<String, String> {
    let refresh_token = session
        .refresh_token
        .as_deref()
        .ok_or_else(|| REASON_EXPIRED.to_string())?;
    let url = format!("{}/api/oauth/token", oauth_host().trim_end_matches('/'));
    let mut req = client
        .post(&url)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ]);
    for (k, v) in device_headers() {
        req = req.header(k, v);
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: refresh: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: refresh body: {e}"))?;
    if !status.is_success() {
        return Err(format!("{REASON_EXPIRED}: refresh http {status}"));
    }

    let v: Value =
        serde_json::from_str(&body).map_err(|e| format!("{REASON_DECODE}: refresh: {e}"))?;
    let access = non_empty_str(&v, "access_token")
        .ok_or_else(|| format!("{REASON_DECODE}: refresh missing access_token"))?
        .to_string();
    // Prefer rotated refresh; otherwise keep the previous one.
    let new_refresh = non_empty_str(&v, "refresh_token")
        .map(str::to_string)
        .or_else(|| session.refresh_token.clone());
    let expires_in = parse_refresh_expires_in(&v)?;
    let now = Utc::now().timestamp();
    // Prefer a future server-provided expires_at; if missing or already
    // expired/at-now, derive from now + validated expires_in.
    let expires_at = match parse_expires_at_unix(&v) {
        Some(at) if at > now => at,
        _ => now + expires_in as i64,
    };
    let scope = non_empty_str(&v, "scope");
    let token_type = non_empty_str(&v, "token_type");

    persist_credential_tokens(&TokenPersist {
        access_token: &access,
        refresh_token: new_refresh.as_deref(),
        expires_in,
        expires_at: Some(expires_at),
        scope,
        token_type,
    })?;
    Ok(access)
}

async fn refresh_current_token(
    client: &reqwest::Client,
    observed_access: &str,
) -> Result<String, String> {
    let lock = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;
    let current = load_auth_session()?;
    // Another task may have already refreshed.
    if current.access_token != observed_access && !current.is_expired(Utc::now().timestamp()) {
        return Ok(current.access_token);
    }
    refresh_access_token(client, &current).await
}

async fn load_bearer(client: &reqwest::Client) -> Result<String, String> {
    let session = load_auth_session()?;
    if session.is_expired(Utc::now().timestamp()) {
        if session.refresh_token.is_none() {
            return Err(REASON_EXPIRED.into());
        }
        refresh_current_token(client, &session.access_token).await
    } else {
        Ok(session.access_token)
    }
}

async fn fetch_live() -> Result<UsageSnapshot, String> {
    // Usage GET must not set a custom User-Agent or X-Msh headers — only
    // Authorization + Accept on the request (see GET `/usages` below).
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .build()
        .map_err(|e| format!("{REASON_NETWORK}: client: {e}"))?;

    let mut token = load_bearer(&client).await?;
    let url = format!("{}/usages", base_url().trim_end_matches('/'));

    let mut resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;

    if matches!(resp.status().as_u16(), 401 | 403) {
        token = refresh_current_token(&client, &token).await?;
        resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;
    }

    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(REASON_EXPIRED.into());
    }
    if !status.is_success() {
        return Err(format!("api {status}"));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: body: {e}"))?;
    let windows = parse_usage_payload(&body).map_err(|e| {
        if e == "invalid JSON" {
            format!("{REASON_DECODE}: {e}")
        } else {
            e
        }
    })?;

    Ok(UsageSnapshot {
        provider: PROVIDER_LABEL.into(),
        level: None,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    })
}

// ============================================================
// Usage JSON parser
// ============================================================

/// Parse Kimi usage JSON into tracker windows.
///
/// Accepts CLI `GET .../usages` shape (`usage` + `limits[]`). Returns an error
/// for invalid JSON or when no capped windows can be derived.
pub fn parse_usage_payload(json: &str) -> Result<Vec<UsageWindow>, String> {
    let root: Value = serde_json::from_str(json).map_err(|_| "invalid JSON".to_string())?;

    let mut windows: Vec<UsageWindow> = Vec::new();

    if let Some(usage) = root.get("usage") {
        if let Some(w) = window_from_quota_object(usage, true) {
            windows.push(w);
        }
    }

    if let Some(limits) = root.get("limits").and_then(|v| v.as_array()) {
        for item in limits {
            let src = item.get("detail").filter(|d| d.is_object()).unwrap_or(item);
            let effective = merge_limit_fields(item, src);
            if let Some(w) = window_from_quota_object(&effective, false) {
                windows.push(w);
            }
        }
    }

    if windows.is_empty() {
        return Err("no usable capped windows".to_string());
    }
    sort_windows_for_display(&mut windows);
    Ok(windows)
}

/// Stable popup order: `total`, then `5h`, then `weekly`, then any other
/// labels in their original relative order (slice::sort_by_key is stable).
fn sort_windows_for_display(windows: &mut [UsageWindow]) {
    windows.sort_by_key(|w| display_rank(&w.label));
}

fn display_rank(label: &str) -> u8 {
    match label {
        "total" => 0,
        "5h" => 1,
        "weekly" => 2,
        _ => 3,
    }
}

/// Merge label/window metadata into the quota source (`detail` or the limit item).
///
/// Precedence for metadata keys:
/// 1. Fields already on `src` (usually `detail`) win.
/// 2. Nested official `window: { duration, timeUnit, ... }` fills gaps.
/// 3. Flattened fields on the outer limit item fill remaining gaps.
///
/// When `src` is the same object as `parent` (flat payload), still apply nested
/// `window` if present so mixed shapes work.
fn merge_limit_fields(parent: &Value, src: &Value) -> Value {
    let mut obj = src.as_object().cloned().unwrap_or_default();

    // Official nested shape: window metadata under `window`.
    if let Some(window) = parent.get("window").and_then(|w| w.as_object()) {
        fill_missing_metadata(&mut obj, window);
    }

    // Outer item as further fallback (flat duration/timeUnit/name/reset).
    // Skip when src is already the parent so we do not re-read the same keys.
    if !std::ptr::eq(src, parent) {
        if let Some(p) = parent.as_object() {
            fill_missing_metadata(&mut obj, p);
        }
    }

    Value::Object(obj)
}

const LIMIT_METADATA_KEYS: &[&str] = &[
    "name",
    "duration",
    "timeUnit",
    "time_unit",
    "reset_at",
    "resetAt",
    "reset_time",
    "resetTime",
];

fn fill_missing_metadata(
    dest: &mut serde_json::Map<String, Value>,
    fallback: &serde_json::Map<String, Value>,
) {
    for key in LIMIT_METADATA_KEYS {
        if !dest.contains_key(*key) {
            if let Some(v) = fallback.get(*key) {
                dest.insert((*key).to_string(), v.clone());
            }
        }
    }
}

fn window_from_quota_object(obj: &Value, is_usage_summary: bool) -> Option<UsageWindow> {
    let limit = number_field(obj, "limit")?;
    if limit <= 0.0 {
        return None;
    }

    let used = match number_field(obj, "used") {
        Some(u) => u,
        None => {
            let remaining = number_field(obj, "remaining")?;
            limit - remaining
        }
    };

    let used_percent = ((used / limit) * 100.0).clamp(0.0, 100.0) as f32;
    let label = canonicalize_label(obj, is_usage_summary);
    let reset_at = parse_reset_at(obj);

    Some(UsageWindow {
        label,
        used_percent,
        reset_at,
        bar_visible: true,
        is_unlimited: false,
        used_absolute: Some(used),
        limit_absolute: Some(limit),
    })
}

fn number_field(obj: &Value, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_i64().map(|i| i as f64))
            .or_else(|| v.as_u64().map(|u| u as f64))
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

fn string_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str().map(|s| s.to_string()))
}

fn parse_reset_at(obj: &Value) -> Option<DateTime<Utc>> {
    for key in ["reset_at", "resetAt", "reset_time", "resetTime"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.with_timezone(&Utc));
            }
        }
    }
    None
}

/// Map Kimi names / duration+timeUnit to tracker labels.
fn canonicalize_label(obj: &Value, is_usage_summary: bool) -> String {
    // Top-level `usage` summary: prefer name evidence over a hard-coded weekly label.
    // Product evidence shows a distinct "Total usage" row (not a paced weekly window).
    if is_usage_summary {
        let name = string_field(obj, "name")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if name.contains("total") {
            return "total".into();
        }
        if name.contains("week") {
            return "weekly".into();
        }
        // Unnamed / unknown summary: keep historical weekly fallback.
        return "weekly".into();
    }

    // Timed windows: prefer duration + timeUnit (300 MINUTE → 5h).
    if let Some(label) = label_from_duration(obj) {
        return label;
    }

    let name = string_field(obj, "name")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    if name.contains("week") {
        return "weekly".into();
    }
    // Robustness: names like "5h limit" / "5-hour limit" without duration.
    if let Some(label) = label_from_name_hint(&name) {
        return label;
    }
    if !name.is_empty() {
        return name;
    }
    // Live evidence: unnamed timed limits[] rows are the 5-hour quota.
    "5h".into()
}

/// Normalize common human window names when duration metadata is absent.
fn label_from_name_hint(name: &str) -> Option<String> {
    // Collapse separators so "5-hour", "5 hour", "5h" share one path.
    let compact: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();

    // 5h / 5hour / 5hours (optionally followed by "limit"/"window" already stripped of spaces)
    if compact.starts_with("5h") || compact.starts_with("5hour") || compact.starts_with("5hours") {
        return Some("5h".into());
    }
    None
}

fn label_from_duration(obj: &Value) -> Option<String> {
    let duration = number_field(obj, "duration")?;
    let unit = string_field(obj, "timeUnit")
        .or_else(|| string_field(obj, "time_unit"))
        .unwrap_or_default()
        .to_ascii_uppercase();

    // 300 MINUTE → canonical 5h
    if (duration - 300.0).abs() < f64::EPSILON && (unit == "MINUTE" || unit == "MINUTES") {
        return Some("5h".into());
    }

    // Deterministic generic labels for other valid duration units
    let n = duration as i64;
    match unit.as_str() {
        "MINUTE" | "MINUTES" => {
            if n > 0 && n % 60 == 0 {
                Some(format!("{}h", n / 60))
            } else {
                Some(format!("{n}m"))
            }
        }
        "HOUR" | "HOURS" => Some(format!("{n}h")),
        "DAY" | "DAYS" => Some(format!("{n}d")),
        "WEEK" | "WEEKS" => {
            if n == 1 {
                Some("weekly".into())
            } else {
                Some(format!("{n}w"))
            }
        }
        "SECOND" | "SECONDS" => {
            if n > 0 && n % 3600 == 0 {
                Some(format!("{}h", n / 3600))
            } else if n > 0 && n % 60 == 0 {
                Some(format!("{}m", n / 60))
            } else {
                Some(format!("{n}s"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_auth_status, parse_usage_payload};
    use crate::providers::UsageWindow;
    use chrono::{DateTime, TimeZone, Utc};
    use serde_json::json;

    /// Official-shaped payload from `GET https://api.kimi.com/coding/v1/usages`:
    /// top-level `usage` summary plus a `limits` detail array. Windows carry
    /// `used`/`remaining`/`limit`, optional `resetAt`, and for timed windows a
    /// `duration` + `timeUnit` pair (300 MINUTE = 5h).
    const OFFICIAL_SHAPED_PAYLOAD: &str = r#"{
        "usage": {
            "name": "weekly",
            "used": 250,
            "remaining": 750,
            "limit": 1000,
            "resetAt": "2026-07-20T12:00:00Z"
        },
        "limits": [
            {
                "name": "5-hour window",
                "remaining": 40,
                "limit": 100,
                "duration": 300,
                "timeUnit": "MINUTE",
                "resetAt": "2026-07-15T18:00:00Z"
            }
        ]
    }"#;

    /// Unix seconds well in the past (2020-01-01T00:00:00Z). Kimi Code token
    /// wire format stores `expires_at` as Unix seconds, not RFC3339.
    const PAST_EXPIRES_AT: i64 = 1_577_836_800;

    #[test]
    fn parse_usage_payload_maps_weekly_summary_and_5h_limit_to_usage_windows() {
        let windows: Vec<UsageWindow> =
            parse_usage_payload(OFFICIAL_SHAPED_PAYLOAD).expect("parse official-shaped payload");

        let weekly = windows
            .iter()
            .find(|w| w.label == "weekly")
            .expect("weekly summary window");
        // used/limit → percent: 250/1000 = 25%
        assert!(
            (weekly.used_percent - 25.0).abs() < 0.001,
            "weekly used_percent: got {}",
            weekly.used_percent
        );
        assert_eq!(
            weekly.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-20T12:00:00Z")
                    .expect("fixture resetAt")
                    .with_timezone(&Utc)
            )
        );

        let h5 = windows
            .iter()
            .find(|w| w.label == "5h")
            .expect("300-minute limit maps to canonical 5h");
        // remaining-only detail: used = limit - remaining = 100 - 40 = 60 → 60%
        assert!(
            (h5.used_percent - 60.0).abs() < 0.001,
            "5h used_percent from remaining: got {}",
            h5.used_percent
        );
        assert_eq!(
            h5.reset_at,
            Utc.with_ymd_and_hms(2026, 7, 15, 18, 0, 0).single()
        );
    }

    /// Absolute `used`/`limit` counters ride along on every window so the
    /// popup can show "250 / 1,000 used" instead of only a percent.
    #[test]
    fn parse_usage_payload_surfaces_absolute_used_and_limit() {
        let windows: Vec<UsageWindow> =
            parse_usage_payload(OFFICIAL_SHAPED_PAYLOAD).expect("parse official-shaped payload");

        let weekly = windows
            .iter()
            .find(|w| w.label == "weekly")
            .expect("weekly summary window");
        assert_eq!(weekly.used_absolute, Some(250.0));
        assert_eq!(weekly.limit_absolute, Some(1000.0));

        let h5 = windows
            .iter()
            .find(|w| w.label == "5h")
            .expect("5h window");
        // remaining-only detail: used = limit - remaining = 100 - 40 = 60.
        assert_eq!(h5.used_absolute, Some(60.0));
        assert_eq!(h5.limit_absolute, Some(100.0));
    }

    /// Popup card order must match other providers: short window first.
    /// Official CLI shape yields top-level weekly before limits[5h]; product
    /// order must still be `5h`, then `weekly`.
    #[test]
    fn parse_usage_payload_orders_five_hour_before_weekly() {
        let windows: Vec<UsageWindow> =
            parse_usage_payload(OFFICIAL_SHAPED_PAYLOAD).expect("parse official-shaped payload");
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["5h", "weekly"]);
    }

    /// Official nested shape from Moonshot `managed-usage.ts`: each limit is
    /// `{ detail: { used, limit, name?, resetAt }, window: { duration, timeUnit } }`.
    /// Detail may omit `name`/duration; the 5h label must come from nested
    /// `window: { duration: 300, timeUnit: "MINUTE" }` (not flattened parent fields).
    #[test]
    fn parse_usage_payload_reads_window_metadata_from_official_nested_limit() {
        let payload = r#"{
            "usage": {
                "name": "weekly",
                "used": 100,
                "remaining": 900,
                "limit": 1000,
                "resetAt": "2026-07-20T12:00:00Z"
            },
            "limits": [
                {
                    "detail": {
                        "used": 1,
                        "limit": 100,
                        "resetAt": "2026-07-15T18:30:00Z"
                    },
                    "window": {
                        "duration": 300,
                        "timeUnit": "MINUTE"
                    }
                }
            ]
        }"#;

        let windows: Vec<UsageWindow> =
            parse_usage_payload(payload).expect("parse official nested-limit payload");

        let h5 = windows
            .iter()
            .find(|w| w.label == "5h")
            .expect("nested window 300 MINUTE must map to canonical 5h");
        // used/limit → 1/100 = 1%
        assert!(
            (h5.used_percent - 1.0).abs() < 0.001,
            "5h used_percent from detail used/limit: got {}",
            h5.used_percent
        );
        assert_eq!(
            h5.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-15T18:30:00Z")
                    .expect("fixture detail resetAt")
                    .with_timezone(&Utc)
            ),
            "detail resetAt must be preserved"
        );
    }

    /// Product evidence: Kimi web quota shows a distinct "Total usage" summary
    /// (alongside 5-hour and 7-day). Top-level `usage` with that name must keep
    /// label `total`, not be forced to the paced weekly window.
    #[test]
    fn parse_usage_payload_preserves_total_usage_summary_label() {
        let payload = r#"{
            "usage": {
                "name": "Total usage",
                "used": 14,
                "limit": 1000,
                "resetAt": "2026-07-20T12:00:00Z"
            }
        }"#;

        let windows: Vec<UsageWindow> =
            parse_usage_payload(payload).expect("parse Total usage summary");

        let total = windows
            .iter()
            .find(|w| w.label == "total")
            .expect("Total usage summary must label as total");
        // used/limit → 14/1000 = 1.4%
        assert!(
            (total.used_percent - 1.4).abs() < 0.001,
            "total used_percent: got {}",
            total.used_percent
        );
        assert_eq!(
            total.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-20T12:00:00Z")
                    .expect("fixture resetAt")
                    .with_timezone(&Utc)
            )
        );
    }

    /// Live evidence: successful fetch shows `weekly` (~7% used) plus a second
    /// window that was labeled `usage` at 0% used with reset a few hours out.
    /// User confirmed that unnamed/non-duration `limits[]` item is the 5-hour
    /// quota. Unnamed timed limit fallback labels as `5h`.
    #[test]
    fn parse_usage_payload_labels_unnamed_live_limit_as_five_hours() {
        let payload = r#"{
            "usage": {
                "used": 70,
                "limit": 1000,
                "resetAt": "2026-07-22T12:00:00Z"
            },
            "limits": [
                {
                    "used": 0,
                    "limit": 100,
                    "resetAt": "2026-07-15T20:00:00Z"
                }
            ]
        }"#;

        let windows: Vec<UsageWindow> =
            parse_usage_payload(payload).expect("parse live-shaped unnamed limit payload");

        let weekly = windows
            .iter()
            .find(|w| w.label == "weekly")
            .expect("top-level usage summary maps to weekly");
        assert!(
            (weekly.used_percent - 7.0).abs() < 0.001,
            "weekly used_percent: got {}",
            weekly.used_percent
        );

        let h5 = windows
            .iter()
            .find(|w| w.label == "5h")
            .expect("unnamed limits[] item without duration/timeUnit must label as 5h");
        assert!(
            (h5.used_percent - 0.0).abs() < 0.001,
            "5h used_percent: got {}",
            h5.used_percent
        );
    }

    // ============================================================
    // Auth classification (RED): Settings badge must treat expired
    // access + refresh as configured (refreshable), and expired access
    // without refresh as not configured with a safe reason string.
    // ============================================================

    #[test]
    fn classify_kimi_auth_accepts_an_expired_access_token_with_refresh_token() {
        // Official token fields under $KIMI_CODE_HOME/credentials/: access_token,
        // refresh_token, expires_at (Unix seconds), scope, token_type, expires_in.
        let doc = json!({
            "access_token": "kimi-access-expired-but-refreshable",
            "refresh_token": "kimi-refresh-present",
            "expires_at": PAST_EXPIRES_AT,
            "scope": "openid profile",
            "token_type": "Bearer",
            "expires_in": 3600
        });
        let (configured, reason) = classify_auth_status(&doc);
        assert!(
            configured,
            "expired access + non-empty refresh must be configured (refreshable); reason={reason:?}"
        );
        assert!(reason.is_none(), "no reason needed when configured");
    }

    #[test]
    fn classify_kimi_auth_rejects_an_expired_access_token_without_refresh_token() {
        const ACCESS: &str = "kimi-access-expired-no-refresh-secret";
        let doc = json!({
            "access_token": ACCESS,
            "expires_at": PAST_EXPIRES_AT,
            "scope": "openid profile",
            "token_type": "Bearer",
            "expires_in": 3600
        });
        let (configured, reason) = classify_auth_status(&doc);
        assert!(
            !configured,
            "expired access without refresh must NOT be configured"
        );
        let reason = reason.expect("a reason is required when not configured");
        let reason_lc = reason.to_ascii_lowercase();
        assert!(
            reason_lc.contains("expir")
                || reason_lc.contains("re-auth")
                || reason_lc.contains("reauth"),
            "reason must explain expiry/re-auth, got: {reason:?}"
        );
        assert!(
            !reason.contains(ACCESS),
            "reason must not leak token text, got: {reason:?}"
        );
    }

    // ============================================================
    // Refresh token expiry (RED): zero expires_in must not produce
    // an immediately-expired credential after refresh.
    // ============================================================

    #[test]
    fn refresh_expiry_rejects_zero_expires_in() {
        let doc = json!({ "expires_in": 0 });
        let err =
            super::parse_refresh_expires_in(&doc).expect_err("expires_in: 0 must be rejected");
        assert!(
            err.to_ascii_lowercase().contains("expires_in"),
            "error must mention expires_in, got: {err:?}"
        );
    }
}
