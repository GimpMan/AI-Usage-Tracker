use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "Codex";

/// OpenAI Codex CLI usage, via the same `/wham/usage` endpoint the CLI uses
/// (ChatGPT session auth). A transient live failure (network, expired token,
/// upstream garbage) is surfaced through `apply_fetch_outcome`'s transient
/// path, which holds the last good in-memory snapshot rather than replacing it
/// with stale data — so the bar keeps the last sane reading across blips.
pub struct CodexProvider;

#[async_trait]
impl Provider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, _secrets: &Secrets) -> ProviderFetch {
        // Live/OAuth only. On any failure (no auth, expired token, network,
        // unexpected payload) return the unavailable snapshot; the scheduler's
        // transient path keeps the last good reading visible instead of
        // blanking the bar.
        match fetch_live().await {
            Ok(snap) => classify_snapshot(snap),
            Err(e) => classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, e)),
        }
    }
}

// ============================================================
// Short, bar-friendly reason strings (F7)
// ============================================================
//
// These are the only `unavailable_reason` values the backend ever produces.
// Frontend `SOFT_EMPTY_REASONS` mirrors them so the bar can pick the right
// styling (red error vs muted empty vs stale-with-windows).
const REASON_NO_AUTH: &str = "no Codex auth found";
const REASON_AUTH_MODE: &str = "auth mode not supported";
const REASON_EXPIRED: &str = "session expired — use Sign in";
const REASON_NETWORK: &str = "network error";
const REASON_DECODE: &str = "decode error";
const REASON_NO_RATE_LIMIT: &str = "no rate-limit data yet";

// ============================================================
// Live fetch — `/wham/usage` (ChatGPT session auth)
// ============================================================

const WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const LIVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct AuthFile {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    tokens: Option<AuthTokens>,
}

#[derive(Deserialize)]
struct AuthTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    plan_type: Option<String>,
    // `rate_limit` is a triple-Optional in the OpenAPI model (missing / null /
    // present). Flatten the two outer layers so `None` and `null` collapse
    // into the same error case.
    #[serde(default)]
    rate_limit: Option<Option<RateLimitStatusDetails>>,
}

#[derive(Deserialize)]
struct RateLimitStatusDetails {
    #[serde(default)]
    primary_window: Option<Option<RateLimitWindowSnapshot>>,
    #[serde(default)]
    secondary_window: Option<Option<RateLimitWindowSnapshot>>,
}

#[derive(Deserialize)]
struct RateLimitWindowSnapshot {
    // F6: f32 (not i32) so fractional values don't fail decode. Option so a
    // missing/null field deserializes to None (window dropped) instead of
    // silently becoming 0.0 — which would render as a false "100% remaining".
    used_percent: Option<f32>,
    #[serde(default)]
    limit_window_seconds: i32,
    // F2: Option<i64> (not i64) so a missing field doesn't materialize as 0
    // and later show up as 1970-01-01 in the popup.
    #[serde(default)]
    reset_at: Option<i64>,
}

async fn fetch_live() -> Result<UsageSnapshot, String> {
    let doc = load_auth_doc()?;
    let auth: AuthFile =
        serde_json::from_value(doc).map_err(|e| format!("{REASON_DECODE}: auth: {e}"))?;

    // F4: permit the live fetch whenever a non-empty access token is present,
    // except for explicit API-key modes where the WHAM endpoint shape differs.
    live_auth_allowed(auth.auth_mode.as_deref())?;
    let access_token = auth
        .tokens
        .as_ref()
        .and_then(|t| t.access_token.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| REASON_NO_AUTH.to_string())?
        .to_string();
    let account_id = auth
        .tokens
        .as_ref()
        .and_then(|t| t.account_id.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .user_agent("ai-usage-tracker/0.1")
        .build()
        .map_err(|e| format!("{REASON_NETWORK}: client: {e}"))?;

    let mut resp = send_usage_request(&client, &access_token, account_id.as_deref()).await?;
    let mut status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        // F10: rotate via `tokens.refresh_token` before declaring the session
        // dead — only a rejected/absent refresh grant ends in REASON_EXPIRED.
        let (new_token, new_account) = rotate_access_token(&client, &access_token).await?;
        let account = new_account.or(account_id);
        resp = send_usage_request(&client, &new_token, account.as_deref()).await?;
        status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(REASON_EXPIRED.to_string());
        }
    }
    if !status.is_success() {
        return Err(format!("api {status}"));
    }
    let body: UsageResponse = resp
        .json()
        .await
        .map_err(|e| format!("{REASON_DECODE}: {e}"))?;
    parse_usage_response(body)
}

async fn send_usage_request(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<reqwest::Response, String> {
    let mut req = client.get(WHAM_USAGE_URL).bearer_auth(access_token);
    if let Some(acc) = account_id {
        req = req.header("ChatGPT-Account-Id", acc);
    }
    req.send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: {e}"))
}

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

/// F10: rotate the ChatGPT session via `tokens.refresh_token` against the same
/// OAuth token endpoint the device sign-in exchanges at. On success the merged
/// doc is persisted back to Credential Manager so the next tick uses the fresh
/// token. Returns the new access token + stored account id.
async fn rotate_access_token(
    client: &reqwest::Client,
    observed_access_token: &str,
) -> Result<(String, Option<String>), String> {
    let lock = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;

    // Another fetch may have rotated while we waited on the lock — reuse its
    // token instead of burning a second refresh grant.
    let doc = load_auth_doc()?;
    let current_access = doc
        .pointer("/tokens/access_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(current) = current_access {
        if current != observed_access_token {
            let account = doc
                .pointer("/tokens/account_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            return Ok((current.to_string(), account));
        }
    }

    let refresh_token = doc
        .pointer("/tokens/refresh_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| REASON_EXPIRED.to_string())?;

    let resp = client
        .post(crate::oauth::codex::OAUTH_TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", crate::oauth::codex::CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: refresh: {e}"))?;
    let status = resp.status();
    let body: RefreshResponse = resp
        .json()
        .await
        .map_err(|e| format!("{REASON_DECODE}: refresh: {e}"))?;
    if !status.is_success() || body.error.is_some() {
        // The refresh grant itself was rejected — only a fresh Sign in helps.
        return Err(REASON_EXPIRED.to_string());
    }
    let new_access = body
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{REASON_DECODE}: refresh missing access_token"))?;

    let merged = merge_refresh_into_doc(
        &doc,
        new_access,
        body.refresh_token.as_deref(),
        body.id_token.as_deref(),
    );
    crate::secrets::oauth_set_json("codex", &merged)
        .map_err(|e| format!("store codex oauth: {e}"))?;
    let account = merged
        .pointer("/tokens/account_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((new_access.to_string(), account))
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Merge a refreshed token set into the stored auth doc: the new access token
/// always wins; refresh/id tokens are replaced only when the server returned
/// new ones. `last_refresh` is stamped; all unrelated fields survive.
fn merge_refresh_into_doc(
    doc: &Value,
    new_access: &str,
    new_refresh: Option<&str>,
    new_id: Option<&str>,
) -> Value {
    let mut root = doc.as_object().cloned().unwrap_or_default();
    let mut tokens = root
        .get("tokens")
        .and_then(|t| t.as_object())
        .cloned()
        .unwrap_or_default();
    tokens.insert(
        "access_token".into(),
        Value::String(new_access.to_string()),
    );
    if let Some(r) = new_refresh {
        tokens.insert("refresh_token".into(), Value::String(r.to_string()));
    }
    if let Some(i) = new_id {
        tokens.insert("id_token".into(), Value::String(i.to_string()));
    }
    root.insert("tokens".into(), Value::Object(tokens));
    root.insert(
        "last_refresh".into(),
        Value::String(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
    );
    Value::Object(root)
}

/// F4: decide whether the auth mode observed in `auth.json` allows the live
/// WHAM endpoint to be tried. Empty / `chatgpt` / anything containing
/// `chatgpt` are allowed (the actual token validity check happens later via
/// the 401 path). Explicit API-key modes are rejected — the WHAM endpoint
/// shape doesn't match.
fn live_auth_allowed(mode: Option<&str>) -> Result<(), String> {
    let mode = mode.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("");
    let mode_l = mode.to_ascii_lowercase();
    if matches!(
        mode_l.as_str(),
        "apikey" | "api_key" | "openai-api-key" | "openai_api_key"
    ) {
        return Err(format!("{REASON_AUTH_MODE}: '{mode}'"));
    }
    Ok(())
}

fn codex_home() -> Result<PathBuf, String> {
    super::cli_home_dir("CODEX_HOME", ".codex")
}

/// Legacy CLI path (one-time import only).
fn auth_path() -> Result<PathBuf, String> {
    Ok(codex_home()?.join("auth.json"))
}

/// App OAuth session from Credential Manager, with one-time import from CLI file.
pub(crate) fn load_auth_doc() -> Result<serde_json::Value, String> {
    let path = auth_path().ok();
    let doc = match path.as_ref() {
        Some(p) => crate::secrets::oauth_get_json_or_import_file("codex", p),
        None => crate::secrets::oauth_get_json("codex"),
    };
    doc.ok_or_else(|| REASON_NO_AUTH.to_string())
}

fn parse_usage_response(body: UsageResponse) -> Result<UsageSnapshot, String> {
    let details = body.rate_limit.flatten().ok_or(REASON_NO_RATE_LIMIT)?;

    let mut windows: Vec<UsageWindow> = Vec::new();
    if let Some(w) = details.primary_window.flatten() {
        if let Some(win) = window_from_snapshot("primary", &w) {
            windows.push(win);
        }
    }
    if let Some(w) = details.secondary_window.flatten() {
        if let Some(win) = window_from_snapshot("secondary", &w) {
            windows.push(win);
        }
    }
    if windows.is_empty() {
        return Err(REASON_NO_RATE_LIMIT.into());
    }

    Ok(UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: body.plan_type.map(format_plan_type),
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    })
}

fn window_from_snapshot(kind: &str, w: &RateLimitWindowSnapshot) -> Option<UsageWindow> {
    // A window without used_percent carries no usable reading — return None so
    // the caller drops it rather than rendering a false "100% remaining".
    let used = w.used_percent?;
    let minutes = if w.limit_window_seconds > 0 {
        Some((i64::from(w.limit_window_seconds) + 59) / 60)
    } else {
        None
    };
    Some(UsageWindow {
        label: window_label(kind, minutes),
        used_percent: clamp_percent(used),
        reset_at: parse_secs(w.reset_at),
        bar_visible: true,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

// ============================================================
// Settings-side sign-in classification (T11).
//
// Decides whether the app OAuth session (Credential Manager) holds a usable
// ChatGPT session: non-empty `tokens.access_token` and an auth_mode that the
// OAuth/WHAM path can use (anything except the explicit `apiKey` markers).
// Codex does not store `expires_at`, so any non-empty access token is treated
// as usable for the next fetch — a stale token shows up as a `REASON_EXPIRED`
// snapshot on the bar, not as a misleading green Settings badge.
// ============================================================
pub(crate) const REASON_NO_AUTH_STATUS: &str = "no Codex auth — use Sign in";
pub(crate) const REASON_NO_TOKEN_STATUS: &str = "Codex auth is empty — re-auth";
pub(crate) const REASON_AUTH_MODE_STATUS: &str =
    "Codex auth is in API-key mode — not usable for OAuth";

pub(crate) fn classify_auth_status(doc: &Value) -> (bool, Option<String>) {
    // Anything that is not JSON-shaped means we have no usable session.
    let obj = match doc.as_object() {
        Some(o) => o,
        None => return (false, Some(REASON_NO_AUTH_STATUS.to_string())),
    };

    let mode_raw = obj
        .get("auth_mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mode_lc = mode_raw.map(|m| m.to_ascii_lowercase());
    if let Some(ref m) = mode_lc {
        if matches!(
            m.as_str(),
            "apikey" | "api_key" | "openai-api-key" | "openai_api_key"
        ) {
            return (false, Some(REASON_AUTH_MODE_STATUS.to_string()));
        }
    }

    let access = doc
        .pointer("/tokens/access_token")
        .or_else(|| obj.get("access_token"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match access {
        Some(_) => (true, None),
        None => (false, Some(REASON_NO_TOKEN_STATUS.to_string())),
    }
}

/// F2: drop `reset_at` values <= 0. A missing field deserializes to `None` via
/// `#[serde(default)]` and short-circuits here; an explicit `0` (legacy
/// payload bug) is also rejected so the popup never shows epoch time as
/// "resetting".
fn parse_secs(value: Option<i64>) -> Option<DateTime<Utc>> {
    let v = value.filter(|&t| t > 0)?;
    Utc.timestamp_opt(v, 0).single()
}

/// Format Codex `plan_type` for the overlay label.
///
/// OpenAI's internal Pro tier identifiers map to user-facing allowance
/// multipliers; every other plan type keeps the historical first-character
/// capitalization.
fn format_plan_type(s: String) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "prolite" => "Pro x5".to_string(),
        "pro" => "Pro x20".to_string(),
        _ => capitalize(s),
    }
}

fn capitalize(s: String) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Clamp a percent value to `[0, 100]`. NaN / infinity collapse to 0 so a
/// malformed CLI payload cannot poison the bar's progress fill.
fn clamp_percent(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Derive a short, human label for a rate-limit window from its `window_minutes`.
/// Falls back to the historical defaults when the field is missing.
fn window_label(kind: &str, minutes: Option<i64>) -> String {
    // OpenAI may place the weekly quota in either the primary or secondary
    // slot. Its duration is stable, so normalize an exact seven-day window
    // before applying the position-specific label rules.
    if minutes == Some(60 * 24 * 7) {
        return "wk".into();
    }

    match (kind, minutes) {
        ("primary", Some(m)) if m % 60 == 0 => format!("{}h", m / 60),
        ("primary", Some(m)) => format!("{m}m"),
        ("primary", None) => "5h".into(),
        ("secondary", Some(m)) if m % (60 * 24) == 0 => format!("{}d", m / (60 * 24)),
        ("secondary", Some(m)) if m % 60 == 0 => format!("{}h", m / 60),
        ("secondary", Some(m)) => format!("{m}m"),
        ("secondary", None) => "wk".into(),
        _ => kind.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_plan_type_maps_prolite_to_pro_x5() {
        assert_eq!(format_plan_type("prolite".into()), "Pro x5");
        assert_eq!(format_plan_type(" ProLite ".into()), "Pro x5");
    }

    #[test]
    fn format_plan_type_maps_pro_to_pro_x20() {
        assert_eq!(format_plan_type("pro".into()), "Pro x20");
        assert_eq!(format_plan_type("PRO".into()), "Pro x20");
    }

    #[test]
    fn format_plan_type_capitalizes_other_tiers() {
        assert_eq!(format_plan_type("plus".into()), "Plus");
    }

    // ============================================================
    // F10: refresh-token rotation — doc merge semantics.
    // ============================================================

    #[test]
    fn merge_refresh_replaces_access_and_stamps_last_refresh() {
        let doc = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "id_token": "old-id",
                "account_id": "acc-1",
            },
        });
        let merged = merge_refresh_into_doc(&doc, "new-access", Some("new-refresh"), Some("new-id"));
        assert_eq!(merged["auth_mode"], "chatgpt", "unrelated fields survive");
        assert_eq!(merged["tokens"]["access_token"], "new-access");
        assert_eq!(merged["tokens"]["refresh_token"], "new-refresh");
        assert_eq!(merged["tokens"]["id_token"], "new-id");
        assert_eq!(merged["tokens"]["account_id"], "acc-1", "account id kept");
        assert!(
            merged["last_refresh"].as_str().is_some(),
            "last_refresh is stamped"
        );
    }

    #[test]
    fn merge_refresh_keeps_old_refresh_and_id_when_server_omits_them() {
        let doc = json!({
            "tokens": {
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "id_token": "old-id",
            },
        });
        let merged = merge_refresh_into_doc(&doc, "new-access", None, None);
        assert_eq!(merged["tokens"]["access_token"], "new-access");
        assert_eq!(
            merged["tokens"]["refresh_token"], "old-refresh",
            "a missing refresh_token in the response must not wipe the stored one"
        );
        assert_eq!(merged["tokens"]["id_token"], "old-id");
    }

    #[test]
    fn merge_refresh_handles_doc_without_tokens_object() {
        let doc = json!({ "auth_mode": "chatgpt" });
        let merged = merge_refresh_into_doc(&doc, "new-access", Some("r"), None);
        assert_eq!(merged["tokens"]["access_token"], "new-access");
        assert_eq!(merged["tokens"]["refresh_token"], "r");
    }

    #[test]
    fn parses_live_payload_with_both_windows() {
        // Mirrors the real `/wham/usage` response shape.
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 1,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 18000,
                    "reset_at": 1783303097
                },
                "secondary_window": {
                    "used_percent": 93,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 409285,
                    "reset_at": 1783694381
                }
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let snap = parse_usage_response(body).expect("parse ok");
        assert_eq!(snap.level.as_deref(), Some("Plus"));
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].label, "5h");
        assert!((snap.windows[0].used_percent - 1.0).abs() < 0.001);
        assert_eq!(snap.windows[1].label, "wk"); // 604800s / 60 = 10080 min
        assert!((snap.windows[1].used_percent - 93.0).abs() < 0.001);
    }

    #[test]
    fn parses_weekly_only_payload_when_weekly_is_primary() {
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 24,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 518400,
                    "reset_at": 1784488894
                },
                "secondary_window": null
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let snap = parse_usage_response(body).expect("parse ok");

        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "wk");
        assert!((snap.windows[0].used_percent - 24.0).abs() < 0.001);
        assert_eq!(
            snap.windows[0].reset_at,
            Utc.timestamp_opt(1_784_488_894, 0).single()
        );
    }

    #[test]
    fn live_payload_handles_null_rate_limit() {
        // The server returns `null` here when no rate-limit applies.
        let json = r#"{ "plan_type": "pro", "rate_limit": null }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let err = parse_usage_response(body).expect_err("expected error");
        assert!(err.contains("rate-limit data yet"));
    }

    #[test]
    fn live_payload_handles_missing_rate_limit_field() {
        let json = r#"{ "plan_type": "pro" }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let err = parse_usage_response(body).expect_err("expected error");
        assert!(err.contains("rate-limit data yet"));
    }

    #[test]
    fn live_payload_handles_null_windows() {
        // Some plans have no primary or secondary window.
        let json = r#"{
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": null,
                "secondary_window": {
                    "used_percent": 42,
                    "limit_window_seconds": 3600,
                    "reset_at": 123
                }
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let snap = parse_usage_response(body).expect("parse ok");
        assert_eq!(snap.windows.len(), 1);
        assert_eq!(snap.windows[0].label, "1h");
    }

    #[test]
    fn live_payload_accepts_fractional_used_percent() {
        // F6: f32 so fractional values like 1.5 deserialize instead of failing.
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 1.5,
                    "limit_window_seconds": 18000,
                    "reset_at": 1783303097
                },
                "secondary_window": null
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let snap = parse_usage_response(body).expect("parse ok");
        assert!((snap.windows[0].used_percent - 1.5).abs() < 0.001);
    }

    #[test]
    fn live_window_missing_used_percent_is_dropped_not_zero() {
        // Regression guard: if /wham/usage returns a window object WITHOUT
        // used_percent (observed intermittently in production), it must NOT
        // silently deserialize to 0% used (a false "100% remaining" reading).
        // Drop that window; siblings with real data are still shown.
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": { "limit_window_seconds": 18000, "reset_at": 1783303097 },
                "secondary_window": { "used_percent": 50, "limit_window_seconds": 604800, "reset_at": 1783694381 }
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let snap = parse_usage_response(body).expect("parse ok");
        assert_eq!(
            snap.windows.len(),
            1,
            "window missing used_percent must be dropped, not shown as 0% used"
        );
        assert_eq!(snap.windows[0].label, "wk");
        assert!((snap.windows[0].used_percent - 50.0).abs() < 0.001);
    }

    #[test]
    fn live_all_windows_missing_used_percent_is_unavailable() {
        // If every window lacks used_percent there is no usable reading —
        // surface "no rate-limit data yet" instead of a false-full bar.
        let json = r#"{
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": { "limit_window_seconds": 18000 },
                "secondary_window": { "limit_window_seconds": 604800 }
            }
        }"#;
        let body: UsageResponse = serde_json::from_str(json).expect("decode ok");
        let err = parse_usage_response(body).expect_err("expected unavailable");
        assert!(err.contains("rate-limit data yet"));
    }

    #[test]
    fn live_window_minutes_rounds_up_to_next_hour() {
        // F9: (3599 + 59) / 60 = 60 → "1h". (ceil-to-minute via +59, then hours
        // when divisible by 60).
        let w = RateLimitWindowSnapshot {
            used_percent: Some(10.0),
            limit_window_seconds: 3599,
            reset_at: None,
        };
        assert_eq!(window_from_snapshot("primary", &w).unwrap().label, "1h");
    }

    #[test]
    fn live_window_minutes_unaligned_falls_back_to_minutes_label() {
        // 2700s = 45m (not divisible by 60) → "45m".
        let w = RateLimitWindowSnapshot {
            used_percent: Some(10.0),
            limit_window_seconds: 2700,
            reset_at: None,
        };
        assert_eq!(window_from_snapshot("primary", &w).unwrap().label, "45m");
    }

    #[test]
    fn live_window_minutes_zero_falls_back_to_default_label() {
        let w = RateLimitWindowSnapshot {
            used_percent: Some(10.0),
            limit_window_seconds: 0,
            reset_at: None,
        };
        assert_eq!(window_from_snapshot("primary", &w).unwrap().label, "5h");
        assert_eq!(window_from_snapshot("secondary", &w).unwrap().label, "wk");
    }

    #[test]
    fn live_missing_reset_at_becomes_none_not_epoch() {
        // F2: a missing field deserializes to None, never to "1970-01-01".
        let w = RateLimitWindowSnapshot {
            used_percent: Some(10.0),
            limit_window_seconds: 18000,
            reset_at: None,
        };
        assert_eq!(window_from_snapshot("primary", &w).unwrap().reset_at, None);
    }

    #[test]
    fn live_zero_reset_at_is_filtered_out() {
        // F2: an explicit `0` (legacy bug) is treated the same as missing.
        let w = RateLimitWindowSnapshot {
            used_percent: Some(10.0),
            limit_window_seconds: 18000,
            reset_at: Some(0),
        };
        assert_eq!(window_from_snapshot("primary", &w).unwrap().reset_at, None);
    }

    #[test]
    fn parse_secs_rejects_zero_and_negative() {
        assert_eq!(parse_secs(None), None);
        assert_eq!(parse_secs(Some(0)), None);
        assert_eq!(parse_secs(Some(-1)), None);
        let positive = parse_secs(Some(1_700_000_000));
        assert!(positive.is_some());
    }

    #[test]
    fn label_drives_label_from_window_minutes() {
        // Without window_minutes we should still get the historical defaults.
        assert_eq!(window_label("primary", None), "5h");
        assert_eq!(window_label("secondary", None), "wk");
        // Explicit minute counts.
        assert_eq!(window_label("primary", Some(60)), "1h");
        assert_eq!(window_label("primary", Some(45)), "45m");
        assert_eq!(window_label("secondary", Some(60 * 24)), "1d");
        assert_eq!(window_label("secondary", Some(60 * 24 * 7)), "wk");
    }

    #[test]
    fn clamp_percent_handles_pathological_inputs() {
        assert_eq!(clamp_percent(50.0), 50.0);
        assert_eq!(clamp_percent(-10.0), 0.0);
        assert_eq!(clamp_percent(150.0), 100.0);
        assert_eq!(clamp_percent(f32::NAN), 0.0);
        assert_eq!(clamp_percent(f32::INFINITY), 0.0);
        assert_eq!(clamp_percent(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn live_auth_allowed_permits_empty_and_chatgpt() {
        // F4: missing/empty mode or `chatgpt` (any case, contains chatgpt) is
        // allowed; the actual token-validity check happens via 401 later.
        assert!(live_auth_allowed(None).is_ok());
        assert!(live_auth_allowed(Some("")).is_ok());
        assert!(live_auth_allowed(Some("chatgpt")).is_ok());
        assert!(live_auth_allowed(Some("ChatGPT")).is_ok());
        assert!(live_auth_allowed(Some("chatgptAuthTokens")).is_ok());
        assert!(live_auth_allowed(Some("  chatgpt  ")).is_ok());
    }

    #[test]
    fn live_auth_allowed_rejects_explicit_api_key_modes() {
        let err = live_auth_allowed(Some("apiKey")).expect_err("should reject");
        assert!(err.contains("auth mode"));
        assert!(live_auth_allowed(Some("API_KEY")).is_err());
        assert!(live_auth_allowed(Some("openai-api-key")).is_err());
        assert!(live_auth_allowed(Some("openai_api_key")).is_err());
    }

    #[test]
    fn live_auth_allowed_is_permissive_for_unknown_modes() {
        // F4: anything that isn't an explicit API-key marker is allowed; the
        // WHAM call will fail with 401 if the token isn't a valid session.
        assert!(live_auth_allowed(Some("weird-mode")).is_ok());
        assert!(live_auth_allowed(Some("oauth-foo")).is_ok());
    }

    #[test]
    fn codex_home_honors_codex_home_env_when_set() {
        // F5: $CODEX_HOME wins. The test process may have a real value set, so
        // we can't assert an absolute path — but we can assert it's non-empty
        // and is either the env value or the home-relative default.
        let resolved = codex_home().expect("codex_home");
        if let Ok(env_value) = std::env::var("CODEX_HOME") {
            let trimmed = env_value.trim();
            if !trimmed.is_empty() {
                assert_eq!(resolved, PathBuf::from(trimmed));
            }
        }
        assert!(!resolved.as_os_str().is_empty());
    }

    // ============================================================
    // T11: Settings-side sign-in classification.
    //
    // The badge must be green only when the auth.json contains a non-empty
    // ChatGPT session token in a non-API-key mode. Everything else
    // (missing, malformed, empty token, API-key mode) is reported as not
    // configured with a UI-readable reason.
    // ============================================================

    #[test]
    fn codex_classify_chatgpt_with_access_token_is_configured() {
        let doc = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "abc",
                "refresh_token": "xyz"
            }
        });
        let (ok, reason) = classify_auth_status(&doc);
        assert!(
            ok,
            "chatgpt mode + non-empty access token must be configured; reason={reason:?}"
        );
        assert!(reason.is_none(), "no reason needed when configured");
    }

    #[test]
    fn codex_classify_default_mode_with_access_token_is_configured() {
        // The CLI omits auth_mode in older auth.json payloads; the live fetch
        // already falls through `live_auth_allowed` and decides via the WHAM
        // 401 path. Treat unknown modes as potentially session-backed.
        let doc = json!({
            "tokens": {
                "access_token": "abc"
            }
        });
        let (ok, _) = classify_auth_status(&doc);
        assert!(ok, "tokens.access_token alone is a valid session");
    }

    #[test]
    fn codex_classify_empty_access_token_is_not_configured() {
        let doc = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "",
                "refresh_token": "xyz"
            }
        });
        let (ok, reason) = classify_auth_status(&doc);
        assert!(!ok, "blank access token must NOT be configured");
        assert!(reason.is_some(), "missing reason for empty access token");
    }

    #[test]
    fn codex_classify_whitespace_access_token_is_not_configured() {
        let doc = json!({
            "auth_mode": "chatgpt",
            "tokens": { "access_token": "   " }
        });
        let (ok, _) = classify_auth_status(&doc);
        assert!(!ok, "whitespace-only access token must NOT be configured");
    }

    #[test]
    fn codex_classify_missing_tokens_is_not_configured() {
        let doc = json!({
            "auth_mode": "chatgpt"
        });
        let (ok, reason) = classify_auth_status(&doc);
        assert!(!ok);
        assert!(reason.is_some());
    }

    #[test]
    fn codex_classify_api_key_mode_is_not_configured() {
        // API-key auth.json holds an OpenAI PAT, not a ChatGPT session — the
        // OAuth/WHAM path will refuse it. Settings should not pretend it is
        // signed in here.
        let doc = json!({
            "auth_mode": "apiKey",
            "OPENAI_API_KEY": "sk-test"
        });
        let (ok, reason) = classify_auth_status(&doc);
        assert!(!ok);
        let reason = reason.expect("a reason is required for apiKey mode");
        assert!(
            reason.contains("api") || reason.contains("API"),
            "reason must call out API-key mode, got: {reason:?}"
        );
    }
}
