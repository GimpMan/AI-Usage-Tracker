//! Cursor IDE / Agent usage (unofficial dashboard API).
//!
//! Auth (first match wins):
//! 1. Saved User API Key (`crsr_…`) exchanged for a session token
//! 2. Local Agent/CLI `auth.json` (`%APPDATA%\Cursor\auth.json`)
//! 3. Cursor IDE `state.vscdb` (`cursorAuth/accessToken`)
//!
//! Usage: Connect-RPC `GetCurrentPeriodUsage` + `GetPlanInfo` on
//! `api2.cursor.sh`. Individual plans expose two monthly pools that reset
//! with the billing cycle:
//! - **Cursor Models** (`autoPercentUsed`) — Composer / Grok / Auto
//! - **Other Models** (`apiPercentUsed`) — third-party frontier models
//! plus overall included usage (`totalPercentUsed`).
//!
//! These endpoints are not a public Cursor API and may change.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "Cursor";
const PROVIDER_ID: &str = "cursor";

const USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const PLAN_URL: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo";
const EXCHANGE_URL: &str = "https://api2.cursor.sh/auth/exchange_user_api_key";
const LIVE_TIMEOUT: Duration = Duration::from_secs(12);
const TOKEN_REFRESH_SKEW_SECS: i64 = 5 * 60;

const REASON_NO_AUTH: &str = "no auth found — sign in to Cursor or paste a User API Key";
const REASON_EXPIRED: &str = "session expired — sign in to Cursor";
const REASON_NETWORK: &str = "network error";
const REASON_DECODE: &str = "decode error";
const REASON_NO_DATA: &str = "no usage data yet";
const REASON_INVALID: &str = "invalid api key";

pub(crate) const REASON_NO_AUTH_STATUS: &str =
    "Sign in to Cursor on this PC, or paste a User API Key";

static TOKEN_CACHE: OnceLock<tokio::sync::Mutex<Option<CachedToken>>> = OnceLock::new();

struct CachedToken {
    key_fingerprint: String,
    access_token: String,
    expires_at: DateTime<Utc>,
}

pub struct CursorProvider;

#[async_trait]
impl Provider for CursorProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, secrets: &Secrets) -> ProviderFetch {
        match fetch_live(secrets.get(PROVIDER_ID).as_deref()).await {
            Ok(snap) => classify_snapshot(snap),
            Err(e) => classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, e)),
        }
    }
}

/// Settings → Test. `api_key` is a draft or saved `crsr_…` key; `None` uses
/// the local Cursor session on this machine.
pub async fn test_key(api_key: Option<&str>) -> Result<String, String> {
    let snap = fetch_live(api_key).await?;
    match snap.unavailable_reason {
        Some(err) => Err(err),
        None => {
            let summary: Vec<String> = snap
                .windows
                .iter()
                .map(|w| {
                    let left = (100.0 - w.used_percent).clamp(0.0, 100.0);
                    format!("{:.0}% {} left", left, super::short_window_label(&w.label))
                })
                .collect();
            let level = snap.level.unwrap_or_else(|| "?".into());
            if summary.is_empty() {
                Ok(format!("Cursor [{level}] connected, no windows returned"))
            } else {
                Ok(format!("Cursor [{level}] — {}", summary.join(" · ")))
            }
        }
    }
}

/// True when a local Cursor session file (or IDE DB) yields a non-empty token.
pub(crate) fn has_local_session() -> bool {
    load_local_access_token().is_ok()
}

async fn fetch_live(api_key: Option<&str>) -> Result<UsageSnapshot, String> {
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .user_agent("ai-usage-tracker/0.1")
        .build()
        .map_err(|e| format!("{REASON_NETWORK}: client: {e}"))?;

    let mut token = resolve_access_token(&client, api_key).await?;
    let mut usage = match post_connect_json(&client, USAGE_URL, &token).await {
        Ok(v) => v,
        Err(e) if is_auth_error(&e) => {
            token = refresh_access_token(&client, api_key).await?;
            post_connect_json(&client, USAGE_URL, &token).await?
        }
        Err(e) => return Err(e),
    };

    if usage_looks_empty(&usage) {
        return Err(REASON_NO_DATA.to_string());
    }

    let plan_name = match post_connect_json(&client, PLAN_URL, &token).await {
        Ok(plan) => plan_name_from_value(&plan),
        Err(e) if is_auth_error(&e) => None,
        Err(_) => None,
    };

    Ok(snapshot_from_usage(&mut usage, plan_name))
}

fn is_auth_error(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("401") || lower.contains("403") || lower.contains("session expired")
}

async fn resolve_access_token(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<String, String> {
    if let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        if !key.starts_with("crsr_") {
            return Err(REASON_INVALID.to_string());
        }
        return exchange_api_key(client, key).await;
    }
    load_local_access_token()
}

async fn refresh_access_token(
    client: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<String, String> {
    if let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        invalidate_cached_token();
        return exchange_api_key(client, key).await;
    }
    // Re-read local files in case Cursor rotated the session while we slept.
    load_local_access_token().map_err(|_| REASON_EXPIRED.to_string())
}

async fn exchange_api_key(client: &reqwest::Client, api_key: &str) -> Result<String, String> {
    let fingerprint = fingerprint_key(api_key);
    {
        let cache = TOKEN_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
        let guard = cache.lock().await;
        if let Some(cached) = guard.as_ref() {
            if cached.key_fingerprint == fingerprint
                && cached.expires_at > Utc::now() + chrono::Duration::seconds(TOKEN_REFRESH_SKEW_SECS)
            {
                return Ok(cached.access_token.clone());
            }
        }
    }

    let resp = client
        .post(EXCHANGE_URL)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(REASON_INVALID.to_string());
    }
    if !status.is_success() {
        return Err(format!("{REASON_NETWORK}: http {status}"));
    }
    let parsed: ExchangeResponse =
        serde_json::from_str(&body).map_err(|e| format!("{REASON_DECODE}: exchange: {e}"))?;
    let token = parsed
        .access_token
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{REASON_DECODE}: exchange: no accessToken"))?;
    let expires_at = jwt_expiry(&token).unwrap_or_else(|| Utc::now() + chrono::Duration::hours(1));

    let cache = TOKEN_CACHE.get_or_init(|| tokio::sync::Mutex::new(None));
    *cache.lock().await = Some(CachedToken {
        key_fingerprint: fingerprint,
        access_token: token.clone(),
        expires_at,
    });
    Ok(token)
}

fn invalidate_cached_token() {
    if let Some(cache) = TOKEN_CACHE.get() {
        if let Ok(mut guard) = cache.try_lock() {
            *guard = None;
        }
    }
}

fn fingerprint_key(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
struct ExchangeResponse {
    #[serde(default, alias = "accessToken")]
    access_token: Option<String>,
}

async fn post_connect_json(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<Value, String> {
    let resp = client
        .post(url)
        .bearer_auth(token)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))?;
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(REASON_EXPIRED.to_string());
    }
    if status.as_u16() == 429 {
        return Err("api 429 Too Many Requests".to_string());
    }
    if !status.is_success() {
        return Err(format!("{REASON_NETWORK}: http {status}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("{REASON_DECODE}: {e}"))
}

fn load_local_access_token() -> Result<String, String> {
    for path in auth_json_candidates() {
        if let Some(token) = read_auth_json_token(&path) {
            return Ok(token);
        }
    }
    if let Some(path) = vscdb_candidates().into_iter().find(|p| p.is_file()) {
        if let Some(token) = read_vscdb_access_token(&path) {
            return Ok(token);
        }
    }
    Err(REASON_NO_AUTH.to_string())
}

fn auth_json_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(PathBuf::from(appdata).join("Cursor").join("auth.json"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".cursor").join("auth.json"));
        out.push(home.join(".config").join("cursor").join("auth.json"));
        out.push(
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("auth.json"),
        );
    }
    out
}

fn vscdb_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        out.push(
            PathBuf::from(appdata)
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        );
    }
    if let Some(home) = dirs::home_dir() {
        out.push(
            home.join("Library")
                .join("Application Support")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        );
        out.push(
            home.join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
        );
    }
    out
}

fn read_auth_json_token(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    access_token_from_auth_json(&value)
}

fn access_token_from_auth_json(value: &Value) -> Option<String> {
    value
        .get("accessToken")
        .or_else(|| value.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Best-effort extract of `cursorAuth/accessToken` from VS Code's SQLite
/// `ItemTable` without pulling in rusqlite. Looks for a JWT near the key.
fn read_vscdb_access_token(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 512 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let key = b"cursorAuth/accessToken";
    let pos = bytes.windows(key.len()).position(|w| w == key)?;
    extract_jwt_after(&bytes[pos + key.len()..])
}

fn extract_jwt_after(bytes: &[u8]) -> Option<String> {
    let start = bytes.windows(3).position(|w| w == b"eyJ")?;
    let slice = &bytes[start..];
    let mut end = 0;
    for (i, b) in slice.iter().enumerate() {
        if b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_' || *b == b'.' {
            end = i + 1;
        } else {
            break;
        }
    }
    let token = std::str::from_utf8(&slice[..end]).ok()?.to_string();
    (token.matches('.').count() == 2 && token.len() > 20).then_some(token)
}

fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let payload = token.split('.').nth(1)?;
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(padded))
        .ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    let exp = value.get("exp")?.as_i64()?;
    Utc.timestamp_opt(exp, 0).single()
}

fn usage_looks_empty(usage: &Value) -> bool {
    usage.get("planUsage").or_else(|| usage.get("plan_usage")).is_none()
        && usage
            .get("planInfo")
            .or_else(|| usage.get("plan_info"))
            .is_none()
}

fn plan_name_from_value(value: &Value) -> Option<String> {
    value
        .get("planInfo")
        .or_else(|| value.get("plan_info"))
        .and_then(|p| p.get("planName").or_else(|| p.get("plan_name")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn snapshot_from_usage(usage: &mut Value, plan_name: Option<String>) -> UsageSnapshot {
    let parsed: CurrentPeriodUsage = match serde_json::from_value(usage.take()) {
        Ok(v) => v,
        Err(_) => {
            return UsageSnapshot::unavailable(PROVIDER_LABEL, REASON_DECODE);
        }
    };
    snapshot_from_parsed(parsed, plan_name)
}

fn snapshot_from_parsed(usage: CurrentPeriodUsage, plan_name: Option<String>) -> UsageSnapshot {
    let reset_at = parse_millis(usage.billing_cycle_end.as_ref());
    let pu = usage.plan_usage.unwrap_or_default();
    let limit_cents = finite_positive(pu.limit);

    let auto = finite_percent(pu.auto_percent_used);
    let api = finite_percent(pu.api_percent_used);
    let total = finite_percent(pu.total_percent_used);
    let has_split = auto.is_some() || api.is_some();

    let mut windows = Vec::new();
    if let Some(pct) = auto {
        windows.push(pool_window("cursor", pct, reset_at, limit_cents, true));
    }
    if let Some(pct) = api {
        windows.push(pool_window("api", pct, reset_at, limit_cents, true));
    }
    if let Some(pct) = total {
        windows.push(pool_window("total", pct, reset_at, limit_cents, !has_split));
    }

    if windows.is_empty() {
        return UsageSnapshot::unavailable(PROVIDER_LABEL, REASON_NO_DATA);
    }

    UsageSnapshot {
        provider: PROVIDER_LABEL.into(),
        level: plan_name,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    }
}

fn pool_window(
    label: &str,
    used_percent: f32,
    reset_at: Option<DateTime<Utc>>,
    limit_cents: Option<f64>,
    bar_visible: bool,
) -> UsageWindow {
    let (used_absolute, limit_absolute) = match limit_cents {
        Some(limit) if limit > 0.0 => {
            let dollars = limit / 100.0;
            (
                Some(dollars * (used_percent as f64) / 100.0),
                Some(dollars),
            )
        }
        _ => (None, None),
    };
    UsageWindow {
        label: label.into(),
        used_percent,
        reset_at,
        bar_visible,
        is_unlimited: false,
        used_absolute,
        limit_absolute,
    }
}

fn finite_percent(v: Option<f64>) -> Option<f32> {
    v.filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 100.0) as f32)
}

fn finite_positive(v: Option<f64>) -> Option<f64> {
    v.filter(|n| n.is_finite() && *n > 0.0)
}

fn parse_millis(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?;
    let ms = if let Some(n) = value.as_i64() {
        n
    } else if let Some(n) = value.as_f64() {
        n as i64
    } else if let Some(s) = value.as_str() {
        s.trim().parse::<i64>().ok()?
    } else {
        return None;
    };
    let ms = if ms.abs() < 1_000_000_000_000 {
        ms.saturating_mul(1000)
    } else {
        ms
    };
    Utc.timestamp_millis_opt(ms).single()
}

#[derive(Debug, Default, Deserialize)]
struct CurrentPeriodUsage {
    #[serde(default, alias = "billingCycleStart")]
    #[allow(dead_code)]
    billing_cycle_start: Option<Value>,
    #[serde(default, alias = "billingCycleEnd")]
    billing_cycle_end: Option<Value>,
    #[serde(default, alias = "planUsage")]
    plan_usage: Option<PlanUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct PlanUsage {
    #[serde(default, alias = "totalSpend")]
    #[allow(dead_code)]
    total_spend: Option<f64>,
    #[serde(default, alias = "includedSpend")]
    #[allow(dead_code)]
    included_spend: Option<f64>,
    #[serde(default)]
    limit: Option<f64>,
    #[serde(default)]
    #[allow(dead_code)]
    remaining: Option<f64>,
    #[serde(default, alias = "autoPercentUsed")]
    auto_percent_used: Option<f64>,
    #[serde(default, alias = "apiPercentUsed")]
    api_percent_used: Option<f64>,
    #[serde(default, alias = "totalPercentUsed")]
    total_percent_used: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LIVE_USAGE: &str = r#"{
        "billingCycleStart":"1787740735000",
        "billingCycleEnd":"1790419135000",
        "planUsage":{
            "totalSpend":7912,
            "includedSpend":7000,
            "bonusSpend":912,
            "limit":7000,
            "remainingBonus":false,
            "autoPercentUsed":5.3275,
            "apiPercentUsed":27.61818181818182,
            "totalPercentUsed":6.304382470119522
        }
    }"#;

    fn snap_from_json(raw: &str, plan: Option<&str>) -> UsageSnapshot {
        let mut value: Value = serde_json::from_str(raw).expect("json");
        snapshot_from_usage(&mut value, plan.map(str::to_string))
    }

    #[test]
    fn live_pro_plus_payload_splits_cursor_and_api_pools() {
        let snap = snap_from_json(LIVE_USAGE, Some("Pro+"));
        assert!(snap.unavailable_reason.is_none());
        assert_eq!(snap.level.as_deref(), Some("Pro+"));
        assert_eq!(snap.windows.len(), 3);

        assert_eq!(snap.windows[0].label, "cursor");
        assert!(snap.windows[0].bar_visible);
        assert!((snap.windows[0].used_percent - 5.3275).abs() < 0.001);

        assert_eq!(snap.windows[1].label, "api");
        assert!(snap.windows[1].bar_visible);
        assert!((snap.windows[1].used_percent - 27.61818).abs() < 0.001);

        // Overall included % is popup-only when the dual-pool split exists.
        assert_eq!(snap.windows[2].label, "total");
        assert!(!snap.windows[2].bar_visible);
        assert!((snap.windows[2].used_percent - 6.30438).abs() < 0.001);

        let reset = snap.windows[0].reset_at.expect("billingCycleEnd");
        assert_eq!(reset.timestamp_millis(), 1_790_419_135_000);

        // Dollar attribution is percent × included cap ($70 for Pro+).
        let api = &snap.windows[1];
        assert!((api.limit_absolute.unwrap() - 70.0).abs() < 0.01);
        assert!((api.used_absolute.unwrap() - 70.0 * 27.61818 / 100.0).abs() < 0.05);
    }

    #[test]
    fn total_only_payload_puts_total_on_the_bar() {
        let raw = r#"{
            "billingCycleEnd": 1790419135000,
            "planUsage": { "limit": 2000, "totalPercentUsed": 40.0 }
        }"#;
        let snap = snap_from_json(raw, Some("Pro"));
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "total");
        assert!(snap.windows[0].bar_visible);
        assert!((snap.windows[0].used_percent - 40.0).abs() < f32::EPSILON);
        assert!((snap.windows[0].limit_absolute.unwrap() - 20.0).abs() < 0.01);
        assert!((snap.windows[0].used_absolute.unwrap() - 8.0).abs() < 0.01);
    }

    #[test]
    fn snake_case_aliases_parse() {
        let raw = r#"{
            "billing_cycle_end": "1790419135000",
            "plan_usage": {
                "auto_percent_used": 10,
                "api_percent_used": 20,
                "total_percent_used": 12,
                "limit": 40000
            }
        }"#;
        let snap = snap_from_json(raw, None);
        assert_eq!(snap.windows[0].label, "cursor");
        assert_eq!(snap.windows[1].label, "api");
        assert!((snap.windows[1].limit_absolute.unwrap() - 400.0).abs() < 0.01);
    }

    #[test]
    fn missing_plan_usage_is_unusable() {
        let snap = snap_from_json("{}", None);
        assert_eq!(snap.unavailable_reason.as_deref(), Some(REASON_NO_DATA));
        assert!(snap.windows.is_empty());
    }

    #[test]
    fn percent_is_clamped() {
        let raw = r#"{
            "planUsage": { "autoPercentUsed": 140.0, "apiPercentUsed": -3.0, "limit": 1000 }
        }"#;
        let snap = snap_from_json(raw, None);
        assert_eq!(snap.windows[0].used_percent, 100.0);
        assert_eq!(snap.windows[1].used_percent, 0.0);
    }

    #[test]
    fn access_token_from_auth_json_accepts_camel_and_snake() {
        assert_eq!(
            access_token_from_auth_json(&json!({"accessToken":"  abc  "})).as_deref(),
            Some("abc")
        );
        assert_eq!(
            access_token_from_auth_json(&json!({"access_token":"xyz"})).as_deref(),
            Some("xyz")
        );
        assert!(access_token_from_auth_json(&json!({"accessToken":"  "})).is_none());
        assert!(access_token_from_auth_json(&json!({})).is_none());
    }

    #[test]
    fn exchange_response_accepts_camel_and_snake_access_token() {
        let camel: ExchangeResponse =
            serde_json::from_str(r#"{"accessToken":"tok-camel","refreshToken":"r"}"#)
                .expect("camelCase exchange json");
        assert_eq!(camel.access_token.as_deref(), Some("tok-camel"));

        let snake: ExchangeResponse =
            serde_json::from_str(r#"{"access_token":"tok-snake"}"#).expect("snake_case exchange json");
        assert_eq!(snake.access_token.as_deref(), Some("tok-snake"));
    }

    #[test]
    fn extract_jwt_after_key_payload() {
        let bytes = b"xxxxeyJhbGciOiJIUzI1NiJ9.e30.signature\x00trailing";
        let token = extract_jwt_after(bytes).expect("jwt");
        assert_eq!(token, "eyJhbGciOiJIUzI1NiJ9.e30.signature");
    }

    #[test]
    fn parse_millis_accepts_string_number_and_seconds() {
        let ms = parse_millis(Some(&json!("1790419135000"))).unwrap();
        assert_eq!(ms.timestamp_millis(), 1_790_419_135_000);
        let n = parse_millis(Some(&json!(1_790_419_135_000i64))).unwrap();
        assert_eq!(n, ms);
        let secs = parse_millis(Some(&json!(1_790_419_135i64))).unwrap();
        assert_eq!(secs.timestamp(), 1_790_419_135);
    }

    #[test]
    fn plan_name_from_nested_object() {
        assert_eq!(
            plan_name_from_value(&json!({"planInfo":{"planName":"Pro+"}})).as_deref(),
            Some("Pro+")
        );
        assert_eq!(
            plan_name_from_value(&json!({"plan_info":{"plan_name":"Ultra"}})).as_deref(),
            Some("Ultra")
        );
    }

    #[test]
    fn jwt_expiry_reads_exp_claim() {
        // {"exp": 1790419135} — no signature verification; we only need the payload.
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":1790419135}"#);
        let token = format!("eyJhbGciOiJub25lIn0.{payload}.x");
        let exp = jwt_expiry(&token).unwrap();
        assert_eq!(exp.timestamp(), 1_790_419_135);
    }
}
