//! SuperGrok / Grok Build subscription usage (not xAI developer API credits).
//!
//! Auth: app OAuth session in Windows Credential Manager (`oauth_grok`).
//! Legacy `~/.grok/auth.json` is imported once if CM is empty; the app never
//! writes that CLI file.
//! Usage (same host the Grok Build CLI uses):
//! - `GET .../v1/billing` — calendar-month included dollar pool
//! - `GET .../v1/billing?format=credits` — unified weekly SuperGrok percent
//!   pool (Chat / Imagine / Build / …) plus extra-credit balance
//!
//! The card surfaces both windows when available (weekly + monthly), matching
//! multi-window providers like Kimi.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "Grok";
const PROVIDER_ID: &str = "grok";

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
/// Unified weekly SuperGrok pool + product breakdown (web Settings → Usage).
const BILLING_CREDITS_URL: &str =
    "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const SUBSCRIPTIONS_URL: &str = "https://grok.com/rest/subscriptions";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const LIVE_TIMEOUT: Duration = Duration::from_secs(12);

// Soft-empty reasons (frontend may special-case by prefix later).
const REASON_NO_AUTH: &str = "no Grok auth found";
const REASON_EXPIRED: &str = "session expired — run grok login";
const REASON_NETWORK: &str = "network error";
const REASON_DECODE: &str = "decode error";
const REASON_NO_DATA: &str = "no usage data yet";

static REFRESH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub struct GrokProvider;

#[async_trait]
impl Provider for GrokProvider {
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
// Auth file
// ============================================================

#[derive(Deserialize)]
struct AuthEntry {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
}

struct AuthSession {
    entry_key: String,
    access_token: String,
    refresh_token: Option<String>,
    client_id: String,
    expires_at: Option<DateTime<Utc>>,
}

impl AuthSession {
    fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .map(|expires| expires <= now)
            .unwrap_or(false)
    }
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Clone, Copy)]
enum RefreshReason {
    Expired,
    Rejected,
}

fn grok_home() -> Result<PathBuf, String> {
    super::cli_home_dir("GROK_HOME", ".grok")
}

/// Legacy CLI path (one-time import only).
fn auth_path() -> Result<PathBuf, String> {
    Ok(grok_home()?.join("auth.json"))
}

fn read_auth_map() -> Result<Map<String, Value>, String> {
    let path = auth_path().ok();
    let value = match path.as_ref() {
        Some(p) => crate::secrets::oauth_get_json_or_import_file("grok", p),
        None => crate::secrets::oauth_get_json("grok"),
    }
    .ok_or_else(|| REASON_NO_AUTH.to_string())?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{REASON_DECODE}: auth: not an object"))
}

/// Pick the best OIDC SuperGrok / Build session from the app OAuth blob.
fn load_auth_session() -> Result<AuthSession, String> {
    select_auth_session(read_auth_map()?)
}

fn select_auth_session(map: Map<String, Value>) -> Result<AuthSession, String> {
    // Prefer auth.x.ai OIDC entries (Grok Build / SuperGrok).
    let mut candidates: Vec<(i32, AuthSession)> = Vec::new();
    for (k, v) in map {
        let entry: AuthEntry = match serde_json::from_value(v) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let token = entry
            .key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let Some(token) = token else {
            continue;
        };
        let mut score = 0;
        if k.starts_with("https://auth.x.ai::") {
            score += 100;
        }
        if entry
            .auth_mode
            .as_deref()
            .map(|m| m.eq_ignore_ascii_case("oidc"))
            .unwrap_or(false)
        {
            score += 50;
        }
        let client_id = entry
            .oidc_client_id
            .filter(|s| !s.trim().is_empty())
            .or_else(|| k.strip_prefix("https://auth.x.ai::").map(str::to_string))
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
        let expires_at = entry
            .expires_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc));
        candidates.push((
            score,
            AuthSession {
                entry_key: k,
                access_token: token,
                refresh_token: entry.refresh_token.filter(|s| !s.trim().is_empty()),
                client_id,
                expires_at,
            },
        ));
    }
    if candidates.is_empty() {
        return Err(REASON_NO_AUTH.into());
    }

    candidates.sort_by(|(a, _), (b, _)| b.cmp(a));
    Ok(candidates.remove(0).1)
}

fn update_auth_map(
    map: &mut Map<String, Value>,
    entry_key: &str,
    refreshed: &RefreshResponse,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let entry = map
        .get_mut(entry_key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("{REASON_DECODE}: auth entry disappeared"))?;
    entry.insert("key".into(), Value::String(refreshed.access_token.clone()));
    if let Some(refresh_token) = refreshed.refresh_token.as_ref() {
        entry.insert("refresh_token".into(), Value::String(refresh_token.clone()));
    }
    let expires_at = now + chrono::Duration::seconds(refreshed.expires_in.unwrap_or(21_600) as i64);
    entry.insert(
        "expires_at".into(),
        Value::String(expires_at.to_rfc3339_opts(SecondsFormat::Nanos, true)),
    );
    Ok(())
}

fn persist_refresh(entry_key: &str, refreshed: &RefreshResponse) -> Result<(), String> {
    let mut map = read_auth_map()?;
    update_auth_map(&mut map, entry_key, refreshed, Utc::now())?;
    crate::secrets::oauth_set_json("grok", &Value::Object(map))
        .map_err(|e| format!("write Grok auth: {e}"))
}

async fn refresh_access_token(
    client: &reqwest::Client,
    session: &AuthSession,
) -> Result<String, String> {
    let refresh_token = session.refresh_token.as_deref().ok_or(REASON_EXPIRED)?;
    let response = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", session.client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: refresh: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("{REASON_NETWORK}: refresh body: {e}"))?;
    if !status.is_success() {
        let error: Option<OAuthErrorResponse> = serde_json::from_str(&body).ok();
        let detail = error
            .and_then(|e| e.error_description.or(e.error))
            .unwrap_or_else(|| status.to_string());
        return Err(format!("{REASON_EXPIRED}: {detail}"));
    }
    let refreshed: RefreshResponse =
        serde_json::from_str(&body).map_err(|e| format!("{REASON_DECODE}: refresh: {e}"))?;
    if refreshed.access_token.trim().is_empty() {
        return Err(format!("{REASON_DECODE}: refresh missing access token"));
    }
    persist_refresh(&session.entry_key, &refreshed)?;
    Ok(refreshed.access_token)
}

fn should_refresh(
    session: &AuthSession,
    observed_access_token: &str,
    reason: RefreshReason,
    now: DateTime<Utc>,
) -> bool {
    if session.access_token != observed_access_token {
        return false;
    }
    match reason {
        RefreshReason::Expired => session.is_expired(now),
        RefreshReason::Rejected => true,
    }
}

async fn refresh_current_token(
    client: &reqwest::Client,
    observed_access_token: &str,
    reason: RefreshReason,
) -> Result<String, String> {
    let lock = REFRESH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = lock.lock().await;
    let current = load_auth_session()?;
    if should_refresh(&current, observed_access_token, reason, Utc::now()) {
        refresh_access_token(client, &current).await
    } else {
        Ok(current.access_token)
    }
}

async fn load_bearer(client: &reqwest::Client) -> Result<String, String> {
    let session = load_auth_session()?;
    if session.is_expired(Utc::now()) {
        refresh_current_token(client, &session.access_token, RefreshReason::Expired).await
    } else {
        Ok(session.access_token)
    }
}

// ============================================================
// Billing response
// ============================================================

#[derive(Deserialize)]
struct BillingResponse {
    #[serde(default)]
    config: Option<BillingConfig>,
}

/// Shared shape for both default billing and `?format=credits`.
/// Unknown fields are ignored so either payload can deserialize here.
#[derive(Debug, Deserialize)]
struct BillingConfig {
    // --- monthly dollar-pool shape (`GET /v1/billing`) ---
    #[serde(default, rename = "monthlyLimit")]
    monthly_limit: Option<MoneyVal>,
    #[serde(default)]
    used: Option<MoneyVal>,
    #[serde(default, rename = "onDemandCap")]
    on_demand_cap: Option<MoneyVal>,
    #[serde(default, rename = "onDemandUsed")]
    on_demand_used: Option<MoneyVal>,
    #[serde(default, rename = "billingPeriodEnd")]
    billing_period_end: Option<String>,

    // --- weekly SuperGrok shape (`GET /v1/billing?format=credits`) ---
    #[serde(default, rename = "currentPeriod")]
    current_period: Option<CurrentPeriod>,
    #[serde(default, rename = "creditUsagePercent")]
    credit_usage_percent: Option<f64>,
    #[serde(default, rename = "prepaidBalance")]
    prepaid_balance: Option<MoneyVal>,
    /// Per-product breakdown (Chat/Imagine/Build) riding the credits payload.
    #[serde(default, rename = "productUsage")]
    product_usage: Option<Vec<ProductUsage>>,
}

/// One entry of `productUsage[]`: the product's own usage percent within the
/// weekly SuperGrok pool.
#[derive(Debug, Deserialize)]
struct ProductUsage {
    #[serde(default)]
    product: Option<String>,
    #[serde(default, rename = "usagePercent")]
    usage_percent: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CurrentPeriod {
    #[serde(default, rename = "type")]
    period_type: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MoneyVal {
    #[serde(default)]
    val: Option<serde_json::Value>,
}

impl MoneyVal {
    fn as_f64(&self) -> Option<f64> {
        let v = self.val.as_ref()?;
        if let Some(n) = v.as_f64() {
            return Some(n);
        }
        if let Some(s) = v.as_str() {
            return s.parse().ok();
        }
        if let Some(i) = v.as_i64() {
            return Some(i as f64);
        }
        None
    }
}

#[derive(Deserialize)]
struct SubscriptionsResponse {
    #[serde(default)]
    subscriptions: Vec<Subscription>,
}

#[derive(Deserialize)]
struct Subscription {
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn fetch_live() -> Result<UsageSnapshot, String> {
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .user_agent("ai-usage-tracker/0.1")
        .build()
        .map_err(|e| format!("{REASON_NETWORK}: client: {e}"))?;

    let mut token = load_bearer(&client).await?;

    let (mut monthly_resp, mut credits_resp, mut sub_resp) =
        send_usage_requests(&client, &token).await;
    if auth_rejected(&monthly_resp) || auth_rejected(&credits_resp) {
        token = refresh_current_token(&client, &token, RefreshReason::Rejected).await?;
        (monthly_resp, credits_resp, sub_resp) = send_usage_requests(&client, &token).await;
    }

    // Either payload can stand alone. Prefer both; only hard-fail auth expiry.
    let monthly_result = decode_billing_response(monthly_resp, "monthly").await;
    let credits_result = decode_billing_response(credits_resp, "credits").await;
    if matches!(&monthly_result, Err(e) if e.as_str() == REASON_EXPIRED)
        || matches!(&credits_result, Err(e) if e.as_str() == REASON_EXPIRED)
    {
        return Err(REASON_EXPIRED.into());
    }
    let monthly_err = monthly_result.as_ref().err().cloned();
    let credits_err = credits_result.as_ref().err().cloned();
    let monthly_body = monthly_result.unwrap_or(BillingResponse { config: None });
    let credits_body = credits_result.ok();
    if monthly_body.config.is_none()
        && credits_body
            .as_ref()
            .and_then(|b| b.config.as_ref())
            .is_none()
    {
        return Err(monthly_err
            .or(credits_err)
            .unwrap_or_else(|| REASON_NO_DATA.into()));
    }

    parse_billing(monthly_body, credits_body, sub_resp).await
}

fn auth_rejected(resp: &Result<reqwest::Response, reqwest::Error>) -> bool {
    resp.as_ref()
        .map(|r| matches!(r.status().as_u16(), 401 | 403))
        .unwrap_or(false)
}

async fn decode_billing_response(
    resp: Result<reqwest::Response, reqwest::Error>,
    label: &str,
) -> Result<BillingResponse, String> {
    let resp = resp.map_err(|e| format!("{REASON_NETWORK}: {label}: {e}"))?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(REASON_EXPIRED.into());
    }
    if !status.is_success() {
        return Err(format!("api {label} {status}"));
    }
    resp.json()
        .await
        .map_err(|e| format!("{REASON_DECODE}: {label}: {e}"))
}

async fn send_usage_requests(
    client: &reqwest::Client,
    token: &str,
) -> (
    Result<reqwest::Response, reqwest::Error>,
    Result<reqwest::Response, reqwest::Error>,
    Result<reqwest::Response, reqwest::Error>,
) {
    // Monthly pool + weekly SuperGrok credits + tier, in parallel.
    let monthly_fut = client
        .get(BILLING_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send();
    let credits_fut = client
        .get(BILLING_CREDITS_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send();
    let sub_fut = client
        .get(SUBSCRIPTIONS_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send();

    tokio::join!(monthly_fut, credits_fut, sub_fut)
}

async fn parse_billing(
    monthly_body: BillingResponse,
    credits_body: Option<BillingResponse>,
    sub_resp: Result<reqwest::Response, reqwest::Error>,
) -> Result<UsageSnapshot, String> {
    let monthly_cfg = monthly_body.config;
    let credits_cfg = credits_body.and_then(|b| b.config);

    let windows = build_windows(monthly_cfg.as_ref(), credits_cfg.as_ref())?;

    let tier = match sub_resp {
        Ok(r) if r.status().is_success() => parse_subscription(r).await,
        _ => None,
    };

    Ok(UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: tier,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    })
}

/// Build display windows from the two billing payload shapes.
///
/// Order matches multi-window cards (e.g. Kimi): shorter window first.
/// - `weekly` from `?format=credits` (`creditUsagePercent` + period end)
/// - per-product rows from `productUsage[]` (popup-only, bar-hidden)
/// - `monthly` from default billing (`used` / `monthlyLimit`)
/// - optional popup-only credits balance when on-demand cap or prepaid > 0
fn build_windows(
    monthly: Option<&BillingConfig>,
    credits: Option<&BillingConfig>,
) -> Result<Vec<UsageWindow>, String> {
    let mut windows: Vec<UsageWindow> = Vec::new();

    if let Some(w) = weekly_window(credits) {
        windows.push(w);
    }
    windows.extend(product_windows(credits));
    if let Some(w) = monthly_window(monthly) {
        windows.push(w);
    }
    if let Some(w) = credits_extra_window(credits.or(monthly)) {
        windows.push(w);
    }

    if windows.iter().any(|w| w.bar_visible) {
        Ok(windows)
    } else {
        Err(REASON_NO_DATA.into())
    }
}

fn weekly_window(credits: Option<&BillingConfig>) -> Option<UsageWindow> {
    let cfg = credits?;
    let used_percent = cfg.credit_usage_percent? as f32;
    // Accept weekly periods and the bare percent when period type is absent
    // (some proxies omit type but still return a weekly end date).
    let period = cfg.current_period.as_ref();
    let is_weekly = period
        .and_then(|p| p.period_type.as_deref())
        .map(|t| t.to_ascii_uppercase().contains("WEEK"))
        .unwrap_or(true);
    if !is_weekly {
        return None;
    }
    let reset_at = credits_reset_at(cfg);

    Some(UsageWindow {
        label: "weekly".into(),
        used_percent: used_percent.clamp(0.0, 100.0),
        reset_at,
        bar_visible: true,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

/// Reset shared by the weekly pool and its per-product rows.
fn credits_reset_at(cfg: &BillingConfig) -> Option<DateTime<Utc>> {
    cfg.current_period
        .as_ref()
        .and_then(|p| parse_rfc3339(p.end.as_deref()))
        .or_else(|| parse_rfc3339(cfg.billing_period_end.as_deref()))
}

/// Popup-only per-product rows (Chat/Imagine/Build) from `productUsage[]`.
/// Bar-hidden: they break the weekly pool down, they are not separate quotas
/// the collapsed bar should track.
fn product_windows(credits: Option<&BillingConfig>) -> Vec<UsageWindow> {
    let cfg = match credits {
        Some(c) => c,
        None => return Vec::new(),
    };
    let reset_at = credits_reset_at(cfg);
    cfg.product_usage
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| {
            let product = p.product.as_deref()?;
            let used_percent = p.usage_percent? as f32;
            Some(UsageWindow {
                label: product_label(product),
                used_percent: used_percent.clamp(0.0, 100.0),
                reset_at,
                bar_visible: false,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            })
        })
        .collect()
}

/// "GrokBuild" → "Build"; unknown names pass through unchanged.
fn product_label(raw: &str) -> String {
    match raw.strip_prefix("Grok") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => raw.to_string(),
    }
}

fn monthly_window(monthly: Option<&BillingConfig>) -> Option<UsageWindow> {
    let cfg = monthly?;
    let monthly_limit = cfg
        .monthly_limit
        .as_ref()
        .and_then(|m| m.as_f64())
        .filter(|n| *n > 0.0)?;
    let used = cfg
        .used
        .as_ref()
        .and_then(|m| m.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    // Real calendar-month included pool — never invent a weekly allowance by
    // quartering this limit (that falsely pinned the bar at 100% after 25%).
    let used_percent = ((used / monthly_limit) * 100.0).clamp(0.0, 100.0) as f32;
    let reset_at = parse_rfc3339(cfg.billing_period_end.as_deref());

    Some(UsageWindow {
        label: "monthly".into(),
        used_percent,
        reset_at,
        bar_visible: true,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

/// Popup-only extra-credits row when the account has a positive on-demand cap
/// or prepaid balance (web "Extra Usage Credits").
fn credits_extra_window(cfg: Option<&BillingConfig>) -> Option<UsageWindow> {
    let cfg = cfg?;
    let cap = cfg.on_demand_cap.as_ref().and_then(|m| m.as_f64()).unwrap_or(0.0);
    let prepaid = cfg
        .prepaid_balance
        .as_ref()
        .and_then(|m| m.as_f64())
        .unwrap_or(0.0);
    if cap <= 0.0 && prepaid <= 0.0 {
        return None;
    }

    let used = cfg
        .on_demand_used
        .as_ref()
        .and_then(|m| m.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    let used_percent = if cap > 0.0 {
        ((used / cap) * 100.0).clamp(0.0, 100.0) as f32
    } else {
        0.0
    };
    // Prefer remaining prepaid balance for the label when present; else cap.
    let dollars = if prepaid > 0.0 { prepaid } else { cap } / 100.0;

    Some(UsageWindow {
        label: format!("credits ${dollars:.2}"),
        used_percent,
        reset_at: None,
        bar_visible: false,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

async fn parse_subscription(resp: reqwest::Response) -> Option<String> {
    let body: SubscriptionsResponse = resp.json().await.ok()?;
    // Exact match: "SUBSCRIPTION_STATUS_INACTIVE" contains "ACTIVE" as a
    // substring, and the expired entry is listed first — a substring check
    // shadows the live subscription with the stale one.
    let active = body
        .subscriptions
        .into_iter()
        .find(|s| s.status.as_deref() == Some("SUBSCRIPTION_STATUS_ACTIVE"))?;

    active.tier.as_deref().map(pretty_tier)
}

fn parse_rfc3339(s: Option<&str>) -> Option<DateTime<Utc>> {
    let s = s?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn pretty_tier(raw: &str) -> String {
    // SUBSCRIPTION_TIER_GROK_PRO → SuperGrok
    let t = raw.trim();
    if t.contains("HEAVY") {
        return "SuperGrok Heavy".into();
    }
    if t.contains("PRO") || t.contains("SUPER") {
        return "SuperGrok".into();
    }
    if t.contains("LITE") {
        return "SuperGrok Lite".into();
    }
    t.trim_start_matches("SUBSCRIPTION_TIER_").replace('_', " ")
}

// ============================================================
// Settings-side sign-in classification (T11).
//
// Decides whether the app OAuth session holds credentials we can use today:
// either an unexpired access token, or an expired one paired with a refresh
// token we can rotate. Returns `(configured, reason_for_settings)`. Reasons
// are short, user-facing strings — never include the token or any other secret.
// ============================================================
pub(crate) const REASON_NO_AUTH_STATUS: &str = "no Grok auth — use Sign in";
pub(crate) const REASON_EXPIRED_STATUS: &str =
    "session expired and no refresh token — use Sign in";
pub(crate) const REASON_NO_USABLE_TOKEN: &str = "Grok auth is empty — re-auth";

pub(crate) fn classify_auth_status(map: &Map<String, Value>) -> (bool, Option<String>) {
    let now = Utc::now();
    let mut has_any = false;
    let mut has_unexpired = false;
    let mut has_refreshable = false;
    let mut has_expired_only = false;

    for (_k, v) in map {
        let entry: AuthEntry = match serde_json::from_value(v.clone()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let token_present = entry
            .key
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let refresh_present = entry
            .refresh_token
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !token_present && !refresh_present {
            continue;
        }
        has_any = true;

        let expired = entry
            .expires_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc) <= now)
            .unwrap_or(false);

        if token_present && !expired {
            has_unexpired = true;
        } else if token_present && expired && refresh_present {
            has_refreshable = true;
        } else if token_present && expired {
            has_expired_only = true;
        } else if !token_present && refresh_present {
            // No access token yet, but we can mint one with the refresh.
            has_refreshable = true;
        }
    }

    if has_unexpired || has_refreshable {
        return (true, None);
    }
    let reason = if !has_any {
        REASON_NO_AUTH_STATUS
    } else if has_expired_only {
        REASON_EXPIRED_STATUS
    } else {
        REASON_NO_USABLE_TOKEN
    };
    (false, Some(reason.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expired_oidc_entry_with_refresh_token_can_be_refreshed() {
        let raw = json!({
            "https://auth.x.ai::client-id": {
                "key": "expired-access",
                "auth_mode": "oidc",
                "expires_at": "2020-01-01T00:00:00Z",
                "refresh_token": "refresh-me",
                "oidc_client_id": "client-id"
            }
        });

        let session = select_auth_session(raw.as_object().unwrap().clone()).expect("session");

        assert_eq!(session.access_token, "expired-access");
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-me"));
        assert_eq!(session.client_id, "client-id");
        assert!(session.is_expired(Utc::now()));
    }

    #[test]
    fn refreshed_credentials_replace_rotated_tokens_and_preserve_other_entries() {
        let mut map = json!({
            "https://auth.x.ai::client-id": {
                "key": "old-access",
                "auth_mode": "oidc",
                "refresh_token": "old-refresh",
                "custom": "preserve-me"
            },
            "api.x.ai": { "key": "unrelated-api-key" }
        })
        .as_object()
        .unwrap()
        .clone();
        let refreshed = RefreshResponse {
            access_token: "new-access".into(),
            refresh_token: Some("new-refresh".into()),
            expires_in: Some(21_600),
        };

        update_auth_map(
            &mut map,
            "https://auth.x.ai::client-id",
            &refreshed,
            Utc::now(),
        )
        .expect("update");

        let oidc = map["https://auth.x.ai::client-id"].as_object().unwrap();
        assert_eq!(oidc["key"], "new-access");
        assert_eq!(oidc["refresh_token"], "new-refresh");
        assert_eq!(oidc["custom"], "preserve-me");
        assert_eq!(map["api.x.ai"]["key"], "unrelated-api-key");
        assert!(oidc.get("expires_at").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn refresh_decision_ignores_a_token_already_rotated_by_another_fetch() {
        let session = AuthSession {
            entry_key: "oidc".into(),
            access_token: "new-access".into(),
            refresh_token: Some("new-refresh".into()),
            client_id: "client-id".into(),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };

        assert!(!should_refresh(
            &session,
            "old-access",
            RefreshReason::Rejected,
            Utc::now(),
        ));
        assert!(should_refresh(
            &session,
            "new-access",
            RefreshReason::Rejected,
            Utc::now(),
        ));
        assert!(!should_refresh(
            &session,
            "new-access",
            RefreshReason::Expired,
            Utc::now(),
        ));
    }

    #[tokio::test]
    #[ignore = "requires a local Grok login and network access"]
    async fn live_fetch_refreshes_local_session_and_returns_usage() {
        let (first, second) = tokio::join!(fetch_live(), fetch_live());

        for snapshot in [first, second] {
            let snapshot = snapshot.expect("live Grok usage");
            assert!(!snapshot.windows.is_empty());
            assert!(snapshot.unavailable_reason.is_none());
        }
    }

    // ============================================================
    // Dual-window billing: weekly SuperGrok (?format=credits) + monthly pool.
    // Fixtures mirror live payloads captured 2026-07-16.
    // ============================================================

    fn monthly_cfg_json() -> BillingConfig {
        serde_json::from_value(json!({
            "monthlyLimit": { "val": 15000 },
            "used": { "val": 3898 },
            "onDemandCap": { "val": 0 },
            "billingPeriodStart": "2026-07-01T00:00:00+00:00",
            "billingPeriodEnd": "2026-08-01T00:00:00+00:00"
        }))
        .expect("monthly fixture")
    }

    fn credits_weekly_cfg_json() -> BillingConfig {
        serde_json::from_value(json!({
            "currentPeriod": {
                "type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-07-16T15:31:37.506066+00:00",
                "end": "2026-07-23T15:31:37.506066+00:00"
            },
            "creditUsagePercent": 1.0,
            "onDemandCap": { "val": 0 },
            "onDemandUsed": { "val": 0 },
            "productUsage": [{ "product": "GrokBuild", "usagePercent": 1.0 }],
            "isUnifiedBillingUser": true,
            "prepaidBalance": { "val": 0 },
            "billingPeriodStart": "2026-07-16T15:31:37.506066+00:00",
            "billingPeriodEnd": "2026-07-23T15:31:37.506066+00:00"
        }))
        .expect("credits weekly fixture")
    }

    #[test]
    fn build_windows_surfaces_weekly_and_monthly_like_kimi() {
        let monthly = monthly_cfg_json();
        let credits = credits_weekly_cfg_json();
        let windows = build_windows(Some(&monthly), Some(&credits)).expect("windows");

        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["weekly", "Build", "monthly"],
            "weekly first, per-product rows, then monthly"
        );

        let weekly = &windows[0];
        assert!((weekly.used_percent - 1.0).abs() < 0.001);
        assert!(weekly.bar_visible);
        assert_eq!(
            weekly.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-23T15:31:37.506066+00:00")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );

        let mo = &windows[2];
        // 3898 / 15000 ≈ 25.986…%
        assert!((mo.used_percent - (3898.0 / 15000.0 * 100.0) as f32).abs() < 0.01);
        assert!(mo.bar_visible);
        assert_eq!(
            mo.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-08-01T00:00:00+00:00")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
    }

    /// `productUsage[]` becomes popup-only rows: "GrokBuild" → "Build",
    /// bar-hidden, reset inherited from the weekly period.
    #[test]
    fn build_windows_surfaces_product_usage_as_popup_only_rows() {
        let credits = credits_weekly_cfg_json();
        let windows = build_windows(None, Some(&credits)).expect("windows");

        let build = windows
            .iter()
            .find(|w| w.label == "Build")
            .expect("GrokBuild maps to a Build row");
        assert!((build.used_percent - 1.0).abs() < 0.001);
        assert!(
            !build.bar_visible,
            "product rows stay out of the collapsed bar"
        );
        assert_eq!(
            build.reset_at,
            Some(
                DateTime::parse_from_rfc3339("2026-07-23T15:31:37.506066+00:00")
                    .unwrap()
                    .with_timezone(&Utc)
            ),
            "product rows inherit the weekly period reset"
        );
    }

    #[test]
    fn build_windows_monthly_only_when_credits_missing() {
        let monthly = monthly_cfg_json();
        let windows = build_windows(Some(&monthly), None).expect("monthly only");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "monthly");
        assert!(windows[0].bar_visible);
    }

    #[test]
    fn build_windows_weekly_only_when_monthly_pool_absent() {
        let credits = credits_weekly_cfg_json();
        let windows = build_windows(None, Some(&credits)).expect("weekly only");
        assert_eq!(windows.len(), 2, "weekly + its Build product row");
        assert_eq!(windows[0].label, "weekly");
        assert!((windows[0].used_percent - 1.0).abs() < 0.001);
    }

    #[test]
    fn build_windows_rejects_empty_payloads() {
        let err = build_windows(None, None).expect_err("no data");
        assert_eq!(err, REASON_NO_DATA);
    }

    #[test]
    fn build_windows_surfaces_on_demand_credits_popup_only() {
        let mut monthly = monthly_cfg_json();
        monthly.on_demand_cap = Some(MoneyVal {
            val: Some(json!(2500)),
        });
        monthly.on_demand_used = Some(MoneyVal {
            val: Some(json!(500)),
        });
        let windows = build_windows(Some(&monthly), None).expect("with credits");
        let credits = windows
            .iter()
            .find(|w| w.label.starts_with("credits"))
            .expect("credits row");
        assert!(!credits.bar_visible, "extra credits stay popup-only");
        assert_eq!(credits.label, "credits $25.00");
        assert!((credits.used_percent - 20.0).abs() < 0.001);
    }

    #[test]
    fn monthly_window_never_quarters_limit_into_synthetic_weekly() {
        // Historical bug: used/(monthlyLimit/4) hit 100% at 25% real monthly use.
        let monthly = monthly_cfg_json(); // 3898/15000 ≈ 26%
        let w = monthly_window(Some(&monthly)).expect("monthly");
        assert!(w.used_percent < 30.0, "must use full monthly limit, got {}", w.used_percent);
        assert_eq!(w.label, "monthly");
    }

    // ============================================================
    // T11: Settings-side sign-in classification.
    //
    // The Settings badge must say "Signed in" only when the local auth file
    // actually carries credentials we can use *today*: either an unexpired
    // access token, or an expired one paired with a refresh token we can rotate.
    // Every other shape (missing, malformed, expired-no-refresh, empty entries,
    // entries without a key) is reported as not configured with a specific
    // reason the UI can show next to the red NOT SIGNED IN badge.
    //
    // These tests pin `classify_auth_status` to those invariants. They run
    // against the pure JSON input — no disk or env access — so they are
    // deterministic and never touch a real ~/.grok.
    // ============================================================

    fn future_rfc3339(secs: i64) -> String {
        (Utc::now() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
    }

    fn past_rfc3339(secs: i64) -> String {
        (Utc::now() - chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
    }

    #[test]
    fn classify_missing_auth_reports_not_configured() {
        let map = serde_json::Map::<String, Value>::new();
        let (ok, reason) = classify_auth_status(&map);
        assert!(!ok);
        assert!(reason.is_some(), "missing auth must carry a reason");
    }

    #[test]
    fn classify_empty_entries_reports_not_configured() {
        let raw = json!({
            "https://auth.x.ai::cid": { "key": "" }
        });
        let (ok, reason) = classify_auth_status(raw.as_object().unwrap());
        assert!(!ok);
        assert!(reason.is_some(), "blank key must carry a reason");
    }

    #[test]
    fn classify_whitespace_only_key_is_treated_as_empty() {
        let raw = json!({
            "https://auth.x.ai::cid": { "key": "   \n  " }
        });
        let (ok, _) = classify_auth_status(raw.as_object().unwrap());
        assert!(!ok, "whitespace-only key must not count as signed in");
    }

    #[test]
    fn classify_unexpired_access_token_is_configured() {
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "live-access",
                "auth_mode": "oidc",
                "expires_at": future_rfc3339(3600)
            }
        });
        let (ok, reason) = classify_auth_status(raw.as_object().unwrap());
        assert!(
            ok,
            "unexpired access token must be configured; reason={reason:?}"
        );
        assert!(reason.is_none(), "no reason needed when configured");
    }

    #[test]
    fn classify_expired_with_refresh_token_is_still_configured() {
        // The auth file holds a refresh token, so the next fetch can rotate
        // the access token without forcing the user through `grok login` again.
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "old-access",
                "auth_mode": "oidc",
                "expires_at": past_rfc3339(3600),
                "refresh_token": "refresh-me"
            }
        });
        let (ok, reason) = classify_auth_status(raw.as_object().unwrap());
        assert!(
            ok,
            "expired access token + non-empty refresh must be configured; reason={reason:?}"
        );
        assert!(reason.is_none(), "no reason needed when configured");
    }

    #[test]
    fn classify_expired_without_refresh_reports_not_configured() {
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "old-access",
                "auth_mode": "oidc",
                "expires_at": past_rfc3339(3600)
            }
        });
        let (ok, reason) = classify_auth_status(raw.as_object().unwrap());
        assert!(
            !ok,
            "expired access token without refresh must NOT be configured"
        );
        let reason = reason.expect("a reason is required when not configured");
        assert!(
            reason.contains("expired") || reason.contains("re-auth") || reason.contains("login"),
            "reason must explain expiry, got: {reason:?}"
        );
    }

    #[test]
    fn classify_no_expiry_with_refresh_token_is_configured() {
        // CLI profiles that omit expires_at still write a refresh token; those
        // are refreshable and must render as green signed in.
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "access",
                "auth_mode": "oidc",
                "refresh_token": "refresh-me"
            }
        });
        let (ok, _) = classify_auth_status(raw.as_object().unwrap());
        assert!(ok, "no expires_at + non-empty refresh must be configured");
    }

    #[test]
    fn classify_no_expiry_no_refresh_is_configured() {
        // No expires_at, no refresh_token: the file has an access token and
        // nothing tells us it's stale. The next live fetch will hit 401 if
        // the server disagrees and surface that as a stale/expired snapshot.
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "access",
                "auth_mode": "oidc"
            }
        });
        let (ok, _) = classify_auth_status(raw.as_object().unwrap());
        assert!(
            ok,
            "no expires_at + no refresh must still be configured (file is non-empty)"
        );
    }

    #[test]
    fn classify_prefers_refreshable_entry_over_expired_only_entry() {
        let raw = json!({
            "https://auth.x.ai::stale": {
                "key": "old",
                "expires_at": past_rfc3339(3600)
            },
            "https://auth.x.ai::fresh": {
                "key": "new",
                "expires_at": future_rfc3339(3600),
                "refresh_token": "refresh"
            }
        });
        let (ok, _) = classify_auth_status(raw.as_object().unwrap());
        assert!(ok, "at least one usable entry must surface as configured");
    }

    #[test]
    fn classify_rejects_invalid_expiry_timestamp_as_usuable_when_refresh_present() {
        // A garbage expires_at string is not parseable. The refresh token
        // still makes the session usable.
        let raw = json!({
            "https://auth.x.ai::cid": {
                "key": "access",
                "refresh_token": "refresh-me",
                "expires_at": "not-a-date"
            }
        });
        let (ok, _) = classify_auth_status(raw.as_object().unwrap());
        assert!(
            ok,
            "unparseable expires_at + present refresh must be configured"
        );
    }

    #[test]
    fn classify_rejects_entirely_unusable_map() {
        // The file exists, all entries have empty keys, so the credential
        // set is effectively empty.
        let raw = json!({
            "x": { "key": "" },
            "y": { "key": "", "auth_mode": "oidc" }
        });
        let (ok, reason) = classify_auth_status(raw.as_object().unwrap());
        assert!(!ok);
        assert!(reason.is_some());
    }
}
