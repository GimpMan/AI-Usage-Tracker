use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "Claude Code";
const PROVIDER_ID: &str = "claude";

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const LIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Contains "session expired" so `classify_snapshot` treats it as invalid credentials.
const REASON_EXPIRED: &str = "session expired — sign in to Claude again";
/// Contains "no auth" so `classify_snapshot` treats it as missing credentials.
const REASON_NO_AUTH: &str = "no auth found — sign in to Claude";

pub struct ClaudeProvider;

#[async_trait]
impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    /// Live 5h / weekly plan limits from the app's own OAuth session — the
    /// same endpoint the Claude CLI's `/usage` reads. Token refresh and plan
    /// detection happen in `oauth::claude::ensure_session` before each cycle.
    async fn fetch(&self, _secrets: &Secrets) -> ProviderFetch {
        let Some(session) = stored_session() else {
            return classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, REASON_NO_AUTH));
        };

        match fetch_live_windows(&session.token).await {
            Ok(windows) => {
                log::info!(
                    "claude usage: {}",
                    windows
                        .iter()
                        .map(|w| format!("{}={:.0}%", w.label, w.used_percent))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                classify_snapshot(UsageSnapshot {
                    provider: PROVIDER_LABEL.to_string(),
                    level: session.plan,
                    windows,
                    unavailable_reason: None,
                    fetched_at: Utc::now(),
                })
            }
            Err(reason) => {
                log::warn!("claude usage: fetch failed: {reason}");
                classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, reason))
            }
        }
    }
}

struct Session {
    token: String,
    /// Popup subtitle, e.g. "Max 5x" or "Pro".
    plan: Option<String>,
}

/// The app's own Claude session (Credential Manager).
fn stored_session() -> Option<Session> {
    let blob = crate::secrets::oauth_get_json("claude")?;
    let oauth = blob.get("claudeAiOauth")?;
    let token = oauth
        .get("accessToken")
        .and_then(|t| t.as_str())
        .filter(|t| !t.is_empty())?
        .to_string();
    let plan = plan_label(
        oauth.get("subscriptionType").and_then(|s| s.as_str()),
        oauth.get("rateLimitTier").and_then(|s| s.as_str()),
    );
    Some(Session { token, plan })
}

/// "max" + "default_claude_max_5x" → "Max 5x"; "pro" → "Pro".
fn plan_label(subscription: Option<&str>, tier: Option<&str>) -> Option<String> {
    let sub = subscription.filter(|s| !s.is_empty())?;
    let mut name = sub.to_string();
    if let Some(first) = name.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    let multiplier = tier
        .and_then(|t| t.rsplit('_').next())
        .filter(|m| m.len() > 1 && m.ends_with('x') && m[..m.len() - 1].chars().all(|c| c.is_ascii_digit()));
    Some(match multiplier {
        Some(m) => format!("{name} {m}"),
        None => name,
    })
}

async fn fetch_live_windows(token: &str) -> Result<Vec<UsageWindow>, String> {
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .build()
        .map_err(|e| format!("network error: client: {e}"))?;
    let resp = client
        .get(USAGE_URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;

    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(REASON_EXPIRED.into());
    }
    if !status.is_success() {
        return Err(format!("claude usage http {status}"));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("claude usage decode: {e}"))?;
    let windows = parse_live_usage(&body);
    if windows.is_empty() {
        return Err("claude usage decode: no 5h/weekly windows in response".into());
    }
    Ok(windows)
}

/// Optional per-model / per-surface weekly caps. Plans that have one return an
/// object for the key; plans without return null and nothing is shown.
const OPTIONAL_WEEKLY: [(&str, &str); 4] = [
    ("seven_day_sonnet", "Sonnet"),
    ("seven_day_opus", "Opus"),
    ("seven_day_cowork", "Cowork"),
    ("seven_day_oauth_apps", "OAuth Apps"),
];

/// Turn the usage response into windows, showing only what the account has:
///
/// * `five_hour` / `seven_day` → "5h" / "weekly" bar windows (`utilization` is
///   percent used 0–100; `resets_at` is null until the window has started).
/// * `seven_day_sonnet` etc. → popup-only "weekly <Model>" windows, when non-null.
/// * `extra_usage` → popup-only "this month $used / $cap" credits row, only
///   when enabled or already spent.
/// * `seven_day_breakdown` → popup-only "mix …" text row (share of weekly use).
fn parse_live_usage(v: &serde_json::Value) -> Vec<UsageWindow> {
    let mut out = Vec::new();
    for (key, label) in [("five_hour", "5h"), ("seven_day", "weekly")] {
        if let Some(w) = live_window(v, key, label.to_string(), true) {
            out.push(w);
        }
    }
    // No primary windows means the payload is not what we expect; let the
    // caller report a decode error instead of showing only extras.
    if out.is_empty() {
        return out;
    }
    for (key, name) in OPTIONAL_WEEKLY {
        if let Some(w) = live_window(v, key, format!("weekly {name}"), false) {
            out.push(w);
        }
    }
    out.extend(parse_extra_usage(v));
    out.extend(parse_weekly_mix(v));
    out
}

fn live_window(
    v: &serde_json::Value,
    key: &str,
    label: String,
    bar_visible: bool,
) -> Option<UsageWindow> {
    let obj = v.get(key).filter(|o| o.is_object())?;
    let util = obj.get("utilization").and_then(|u| u.as_f64())?;
    let reset_at = obj
        .get("resets_at")
        .and_then(|r| r.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));
    Some(UsageWindow {
        label,
        used_percent: util.clamp(0.0, 100.0) as f32,
        reset_at,
        bar_visible,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

/// Pay-as-you-go overflow credits. Amounts are minor units (`decimal_places`).
fn parse_extra_usage(v: &serde_json::Value) -> Option<UsageWindow> {
    let e = v.get("extra_usage").filter(|o| o.is_object())?;
    let enabled = e.get("is_enabled").and_then(|b| b.as_bool()).unwrap_or(false);
    let used_minor = e.get("used_credits").and_then(|n| n.as_f64()).unwrap_or(0.0);
    if !enabled && used_minor <= 0.0 {
        return None;
    }
    let scale = 10f64.powi(e.get("decimal_places").and_then(|n| n.as_i64()).unwrap_or(2) as i32);
    let used = used_minor / scale;
    let limit = e
        .get("monthly_limit")
        .and_then(|n| n.as_f64())
        .filter(|l| *l > 0.0)
        .map(|l| l / scale);
    let percent = e
        .get("utilization")
        .and_then(|n| n.as_f64())
        .or_else(|| limit.map(|l| used / l * 100.0))
        .unwrap_or(0.0);
    let label = match limit {
        Some(l) => format!("this month ${used:.2} / ${l:.2}"),
        None => format!("this month ${used:.2}"),
    };
    Some(UsageWindow {
        label,
        used_percent: percent.clamp(0.0, 100.0) as f32,
        reset_at: None,
        bar_visible: false,
        is_unlimited: false,
        used_absolute: Some(used),
        limit_absolute: limit,
    })
}

/// Which surfaces consumed the weekly pool ("Claude Code 40% · Chats 60%").
/// A share, not a limit, so it is carried as text in the label.
fn parse_weekly_mix(v: &serde_json::Value) -> Option<UsageWindow> {
    let rows = v.pointer("/seven_day_breakdown/rows")?.as_array()?;
    let parts: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            let name = r.get("display_name")?.as_str()?;
            let pct = r.get("percent")?.as_f64()?;
            (pct > 0.0).then(|| format!("{name} {pct:.0}%"))
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(UsageWindow {
        label: format!("mix {}", parts.join(" · ")),
        used_percent: 0.0,
        reset_at: None,
        bar_visible: false,
        is_unlimited: false,
        used_absolute: None,
        limit_absolute: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderHealth;

    #[test]
    fn live_usage_maps_five_hour_and_weekly_to_bar_windows() {
        let v = serde_json::json!({
            "five_hour": { "utilization": 37.0, "resets_at": "2026-09-30T20:00:00+00:00" },
            "seven_day": { "utilization": 12, "resets_at": "2026-10-04T09:00:00.123456+00:00" },
            "seven_day_opus": null
        });
        let w = parse_live_usage(&v);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label, "5h");
        assert!((w[0].used_percent - 37.0).abs() < 0.001);
        assert!(w[0].bar_visible && w[0].reset_at.is_some());
        assert_eq!(w[1].label, "weekly");
        assert!((w[1].used_percent - 12.0).abs() < 0.001);
        assert!(w[1].reset_at.is_some());
    }

    #[test]
    fn live_usage_keeps_window_without_reset_time_and_clamps() {
        let v = serde_json::json!({
            "five_hour": { "utilization": 140.0, "resets_at": null }
        });
        let w = parse_live_usage(&v);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].used_percent, 100.0);
        assert!(w[0].reset_at.is_none());
    }

    #[test]
    fn live_usage_ignores_unknown_or_malformed_shapes() {
        assert!(parse_live_usage(&serde_json::json!({})).is_empty());
        assert!(parse_live_usage(&serde_json::json!({ "five_hour": null })).is_empty());
        assert!(
            parse_live_usage(&serde_json::json!({ "five_hour": { "utilization": "x" } }))
                .is_empty()
        );
    }

    /// Shape captured from a real Max account (nulls for plans without the feature).
    fn real_max_response() -> serde_json::Value {
        serde_json::json!({
            "extra_usage": {
                "currency": "USD", "decimal_places": 2, "is_enabled": false,
                "monthly_limit": 7500, "used_credits": 0.0, "utilization": 0.0,
                "disabled_reason": "out_of_credits"
            },
            "five_hour": { "utilization": 43.0, "resets_at": "2026-09-30T18:59:59.782168+00:00" },
            "seven_day": { "utilization": 4.0, "resets_at": "2026-10-06T11:59:59.782188+00:00" },
            "seven_day_breakdown": { "rows": [
                { "display_name": "Claude Code", "key": "claude_code", "percent": 40 },
                { "display_name": "Chats", "key": "chat", "percent": 60 },
                { "display_name": "Cowork", "key": "cowork", "percent": 0 },
                { "display_name": "Other", "key": "other", "percent": 0 }
            ]},
            "seven_day_opus": null, "seven_day_sonnet": null,
            "seven_day_cowork": null, "seven_day_oauth_apps": null
        })
    }

    #[test]
    fn real_account_shows_bars_and_mix_but_hides_unavailable_extras() {
        let w = parse_live_usage(&real_max_response());
        let labels: Vec<&str> = w.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5h", "weekly", "mix Claude Code 40% · Chats 60%"]);
        assert!(w[0].bar_visible && w[1].bar_visible);
        assert!(!w[2].bar_visible, "mix is popup-only");
    }

    #[test]
    fn per_model_weekly_limits_appear_automatically_when_returned() {
        let mut v = real_max_response();
        v["seven_day_sonnet"] =
            serde_json::json!({ "utilization": 22.0, "resets_at": "2026-10-06T11:59:59+00:00" });
        let w = parse_live_usage(&v);
        let sonnet = w.iter().find(|w| w.label == "weekly Sonnet").expect("sonnet row");
        assert!(!sonnet.bar_visible);
        assert!((sonnet.used_percent - 22.0).abs() < 0.001);
        assert!(sonnet.reset_at.is_some());
        assert!(w.iter().all(|w| w.label != "weekly Opus"));
    }

    #[test]
    fn extra_usage_credits_appear_once_enabled_or_spent() {
        let mut v = real_max_response();
        v["extra_usage"]["is_enabled"] = serde_json::json!(true);
        v["extra_usage"]["used_credits"] = serde_json::json!(1250.0);
        v["extra_usage"]["utilization"] = serde_json::json!(16.7);
        let w = parse_live_usage(&v);
        let credits = w.iter().find(|w| w.label.starts_with("this month")).expect("credits row");
        assert_eq!(credits.label, "this month $12.50 / $75.00");
        assert!(!credits.bar_visible);
        assert_eq!(credits.used_absolute, Some(12.5));
        assert_eq!(credits.limit_absolute, Some(75.0));
    }

    #[test]
    fn extras_alone_do_not_count_as_a_usable_payload() {
        let v = serde_json::json!({
            "seven_day_sonnet": { "utilization": 5.0, "resets_at": null }
        });
        assert!(parse_live_usage(&v).is_empty());
    }

    #[test]
    fn plan_label_combines_plan_and_multiplier() {
        assert_eq!(
            plan_label(Some("max"), Some("default_claude_max_5x")).as_deref(),
            Some("Max 5x")
        );
        assert_eq!(
            plan_label(Some("max"), Some("default_claude_max_20x")).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(plan_label(Some("pro"), Some("default_claude_ai")).as_deref(), Some("Pro"));
        assert_eq!(plan_label(Some("pro"), None).as_deref(), Some("Pro"));
        assert_eq!(plan_label(None, Some("default_claude_max_5x")), None);
    }

    #[test]
    fn expired_reason_classifies_as_invalid_credentials() {
        let out = classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, REASON_EXPIRED));
        assert_eq!(out.health, ProviderHealth::InvalidCredentials);
    }

    #[test]
    fn no_auth_reason_classifies_as_missing_credentials() {
        let out = classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, REASON_NO_AUTH));
        assert_eq!(out.health, ProviderHealth::MissingCredentials);
    }

    #[test]
    fn live_http_error_classifies_as_transient() {
        let out = classify_snapshot(UsageSnapshot::unavailable(
            PROVIDER_LABEL,
            "claude usage http 429 Too Many Requests",
        ));
        assert_eq!(out.health, ProviderHealth::TransientFailure);
    }
}
