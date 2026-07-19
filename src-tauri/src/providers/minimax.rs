use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "MiniMax Coding Plan";
const KEY_ID: &str = "minimax";

pub struct MinimaxProvider;

#[async_trait]
impl Provider for MinimaxProvider {
    fn id(&self) -> &'static str {
        "minimax"
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, secrets: &Secrets) -> ProviderFetch {
        let key = match secrets.get(KEY_ID) {
            Some(k) => k,
            None => {
                return classify_snapshot(UsageSnapshot::unavailable(
                    PROVIDER_LABEL,
                    "no api key configured",
                ))
            }
        };
        let region = crate::secrets::get_region(KEY_ID).unwrap_or_else(|| "overseas".to_string());
        classify_snapshot(fetch_quota(&key, &region).await)
    }
}

/// Live probe used by Settings → Test. Only hits the **selected** region
/// (overseas = minimax.io, china = minimaxi.com) — never probes both.
pub async fn test_key(api_key: &str, region: &str) -> Result<String, String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("no key provided".into());
    }
    let region = match region {
        "china" => "china",
        _ => "overseas",
    };
    let host = match region {
        "china" => "minimaxi.com",
        _ => "minimax.io",
    };
    let snap = fetch_quota(key, region).await;
    match snap.unavailable_reason {
        None => Ok(format!(
            "{} ({}) — {}",
            PROVIDER_LABEL,
            host,
            format_summary(&snap)
        )),
        Some(e) => Err(format!("{} ({}): {}", PROVIDER_LABEL, host, e)),
    }
}

/// Remaining-% parts only (host/label added by `test_key`).
fn format_summary(snap: &UsageSnapshot) -> String {
    let parts: Vec<String> = snap
        .windows
        .iter()
        .map(|w| {
            let left = (100.0 - w.used_percent).clamp(0.0, 100.0);
            format!("{:.0}% {} left", left, super::short_window_label(&w.label))
        })
        .collect();
    if parts.is_empty() {
        "connected, no windows returned".into()
    } else {
        parts.join(" · ")
    }
}

/// Per-region endpoint pair: (primary Token Plan, fallback Coding Plan).
/// Hosts per research §3:
///   overseas: www.minimax.io / platform.minimax.io
///   china:    www.minimaxi.com (both)
fn endpoints_for(region: &str) -> (&'static str, &'static str) {
    match region {
        "china" => (
            "https://www.minimaxi.com/v1/token_plan/remains",
            "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains",
        ),
        _ => (
            "https://www.minimax.io/v1/token_plan/remains",
            "https://platform.minimax.io/v1/api/openplatform/coding_plan/remains",
        ),
    }
}

async fn fetch_quota(api_key: &str, region: &str) -> UsageSnapshot {
    let key = api_key.trim();
    if key.is_empty() {
        return UsageSnapshot::unavailable(PROVIDER_LABEL, "no api key configured");
    }

    let (primary, fallback) = endpoints_for(region);

    // Try the Token Plan endpoint first; on auth/schema failure, fall through
    // to the Coding Plan endpoint (the key may be a Coding-Plan-only key).
    for url in [primary, fallback] {
        match fetch_one(key, url).await {
            Ok(snap) => return snap,
            Err(AttemptError::Auth(msg) | AttemptError::Definitive(msg)) => {
                return UsageSnapshot::unavailable(PROVIDER_LABEL, msg)
            }
            Err(AttemptError::Retry) => continue,
        }
    }

    UsageSnapshot::unavailable(
        PROVIDER_LABEL,
        "network error: login failed at all endpoints",
    )
}

enum AttemptError {
    /// Try the next endpoint.
    Retry,
    /// Server or HTTP response definitively rejected the credential.
    Auth(String),
    /// Server gave a definitive non-retryable answer.
    Definitive(String),
}

async fn fetch_one(api_key: &str, url: &str) -> Result<UsageSnapshot, AttemptError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("ai-usage-tracker")
        .build()
        .map_err(|_| AttemptError::Retry)?;

    let resp = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| AttemptError::Retry)?;

    // Real HTTP 401/403 is also possible per research — treat as auth failure.
    let resp = match resp.error_for_status() {
        Ok(resp) => resp,
        Err(error)
            if error
                .status()
                .is_some_and(|status| status.as_u16() == 401 || status.as_u16() == 403) =>
        {
            return Err(AttemptError::Auth("invalid api key".into()));
        }
        Err(_) => return Err(AttemptError::Retry),
    };

    let payload: ApiResponse = resp.json().await.map_err(|_| AttemptError::Retry)?;

    // Pitfall: status_code may be at base_resp.status_code OR the response root.
    let status_code = payload
        .status_code
        .or_else(|| payload.base_resp.as_ref().map(|b| b.status_code))
        .unwrap_or(-1);

    if status_code != 0 {
        // 1004 = "login fail" → the credential is not accepted.
        if status_code == 1004 {
            return Err(AttemptError::Auth("invalid api key".into()));
        }
        return Err(AttemptError::Retry);
    }

    // Pick the account-level "general" entry; fall back to the first entry if
    // the API didn't label any as "general".
    let remains = payload
        .model_remains
        .as_ref()
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.model_name.as_deref() == Some("general"))
                .or_else(|| arr.first())
        })
        .ok_or(AttemptError::Retry)?;

    let mut windows: Vec<UsageWindow> = Vec::new();

    // 5-hour window ("current_interval"). Pitfall: field is REMAINING percent,
    // so used% = 100 - remaining. Clamp to absorb schema drift.
    if let Some(remaining) = remains.current_interval_remaining_percent {
        windows.push(UsageWindow {
            label: "5h".into(),
            used_percent: pct_to_used(remaining),
            reset_at: parse_ms(remains.end_time).or_else(|| now_plus_ms(remains.remains_time)),
            bar_visible: true,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        });
    }

    // 7-day window ("current_weekly"). Same remaining-percent semantics as the
    // 5h window. Per the Token Plan docs (platform.minimax.io/docs/token-plan),
    // the included quota is governed by *both* the 5-hour rolling window and a
    // separate weekly window — "unused subscription quota does not carry over
    // to the next billing cycle." Earlier we treated weekly as absent; the
    // schema fields are now reliably populated, so we emit a "wk" bar.
    // `bar_visible: true` keeps it in the collapsed summary next to "5h" —
    // it is the binding constraint for heavy-week users, hiding it would
    // hide the most important reading of the week.
    if let Some(remaining) = remains.current_weekly_remaining_percent {
        windows.push(UsageWindow {
            label: "wk".into(),
            used_percent: pct_to_used(remaining),
            reset_at: parse_ms(remains.weekly_end_time)
                .or_else(|| now_plus_ms(remains.weekly_remains_time)),
            bar_visible: true,
            is_unlimited: remains.current_weekly_status == Some(3),
            used_absolute: None,
            limit_absolute: None,
        });
    }

    if windows.is_empty() {
        // Likely a Coding Plan response without percentages — let the caller
        // decide what to do. We've exhausted the schema, so treat as definitive.
        return Err(AttemptError::Definitive(
            "endpoint returned no percentage fields".into(),
        ));
    }

    Ok(UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: None, // MiniMax exposes no clean plan-tier string; model_name isn't useful here.
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    })
}

/// Parse a unix-ms timestamp (13-digit) into a UTC DateTime.
fn parse_ms(value: Option<i64>) -> Option<DateTime<Utc>> {
    let v = value?;
    let secs = if v > 100_000_000_000 {
        v as f64 / 1000.0
    } else {
        v as f64
    };
    Utc.timestamp_opt(secs as i64, 0).single()
}

/// Compute reset_at = now + relative_ms. Used when only `remains_time` is
/// available (no absolute `end_time`).
fn now_plus_ms(ms: Option<i64>) -> Option<DateTime<Utc>> {
    let ms = ms?;
    if ms <= 0 {
        return None;
    }
    Some(Utc::now() + chrono::Duration::milliseconds(ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ms_handles_ms_and_seconds() {
        let ms = 1_700_000_000_500_i64;
        let parsed = parse_ms(Some(ms)).expect("ms parses");
        assert_eq!(parsed.timestamp(), 1_700_000_000);

        let secs = 1_700_000_000_i64;
        let parsed = parse_ms(Some(secs)).expect("secs parses");
        assert_eq!(parsed.timestamp(), 1_700_000_000);

        assert!(parse_ms(None).is_none());
    }

    #[test]
    fn now_plus_ms_clamps_non_positive() {
        assert!(now_plus_ms(None).is_none());
        assert!(now_plus_ms(Some(0)).is_none());
        assert!(now_plus_ms(Some(-1)).is_none());

        let now = Utc::now();
        let parsed = now_plus_ms(Some(60_000)).expect("positive");
        let diff = parsed.signed_duration_since(now).num_milliseconds();
        assert!(diff >= 59_000 && diff <= 60_500);
    }

    #[test]
    fn pct_to_used_inverts_and_clamps() {
        // Remaining 100% → used 0%
        assert_eq!(pct_to_used(100.0), 0.0);
        // Remaining 0% → used 100%
        assert_eq!(pct_to_used(0.0), 100.0);
        // Mid-range linear
        assert_eq!(pct_to_used(73.5), 26.5);
        // Schema-drift safety: above 100 remaining → clamp to 0 used.
        assert_eq!(pct_to_used(105.0), 0.0);
        // Below 0 remaining → clamp to 100 used.
        assert_eq!(pct_to_used(-3.0), 100.0);
    }

    #[test]
    fn api_response_with_both_windows_parses_to_two_usage_windows() {
        // Locks in the dual-window shape: a payload exposing both 5h and weekly
        // percentages must produce two UsageWindow entries, in 5h-then-wk order,
        // both with used_percent derived from REMAINING and non-None reset_at.
        let payload = r#"{
            "base_resp": {"status_code": 0, "status_msg": "ok"},
            "status_code": 0,
            "model_remains": [
                {
                    "model_name": "general",
                    "current_interval_remaining_percent": 80.0,
                    "current_weekly_remaining_percent": 73.5,
                    "current_weekly_status": 3,
                    "end_time": 1700000000500,
                    "weekly_end_time": 1700604800000,
                    "remains_time": null,
                    "weekly_remains_time": null
                }
            ]
        }"#;

        let parsed: ApiResponse = serde_json::from_str(payload).expect("decode ok");
        let remains = parsed
            .model_remains
            .as_ref()
            .and_then(|arr| {
                arr.iter()
                    .find(|m| m.model_name.as_deref() == Some("general"))
            })
            .expect("general model_remains entry");

        // Build the same windows the live code would push, to keep the test
        // honest about ordering and field selection.
        let mut windows: Vec<UsageWindow> = Vec::new();
        if let Some(remaining) = remains.current_interval_remaining_percent {
            windows.push(UsageWindow {
                label: "5h".into(),
                used_percent: pct_to_used(remaining),
                reset_at: parse_ms(remains.end_time).or_else(|| now_plus_ms(remains.remains_time)),
                bar_visible: true,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });
        }
        if let Some(remaining) = remains.current_weekly_remaining_percent {
            windows.push(UsageWindow {
                label: "wk".into(),
                used_percent: pct_to_used(remaining),
                reset_at: parse_ms(remains.weekly_end_time)
                    .or_else(|| now_plus_ms(remains.weekly_remains_time)),
                bar_visible: true,
                is_unlimited: remains.current_weekly_status == Some(3),
                used_absolute: None,
                limit_absolute: None,
            });
        }

        assert_eq!(windows.len(), 2, "expected both 5h and wk windows");
        assert_eq!(windows[0].label, "5h");
        assert_eq!(windows[1].label, "wk");
        // 100 - 80 = 20 used, 100 - 73.5 = 26.5 used.
        assert!((windows[0].used_percent - 20.0).abs() < 1e-3);
        assert!((windows[1].used_percent - 26.5).abs() < 1e-3);
        assert!(windows[0].reset_at.is_some());
        assert!(windows[1].reset_at.is_some());
        // Weekly reset must be strictly later than 5h reset (a 7-day horizon).
        assert!(windows[1].reset_at.unwrap() > windows[0].reset_at.unwrap());
        assert!(windows[0].bar_visible);
        assert!(windows[1].bar_visible);
        assert!(windows[1].is_unlimited);
    }

    #[test]
    fn api_response_without_weekly_still_parses_5h_only() {
        // Backwards-compat: legacy payloads (or accounts that haven't been
        // migrated to the weekly quota yet) must continue to produce a single
        // 5h window. This is the regression guard that prevents a payload
        // schema gap from blanking the entire MiniMax segment.
        let payload = r#"{
            "base_resp": {"status_code": 0, "status_msg": "ok"},
            "model_remains": [
                {
                    "model_name": "general",
                    "current_interval_remaining_percent": 90.0,
                    "end_time": 1700000000000
                }
            ]
        }"#;

        let parsed: ApiResponse = serde_json::from_str(payload).expect("decode ok");
        let remains = parsed
            .model_remains
            .as_ref()
            .and_then(|arr| arr.first())
            .expect("one entry");

        let mut windows: Vec<UsageWindow> = Vec::new();
        if let Some(remaining) = remains.current_interval_remaining_percent {
            windows.push(UsageWindow {
                label: "5h".into(),
                used_percent: pct_to_used(remaining),
                reset_at: parse_ms(remains.end_time).or_else(|| now_plus_ms(remains.remains_time)),
                bar_visible: true,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });
        }
        if let Some(remaining) = remains.current_weekly_remaining_percent {
            windows.push(UsageWindow {
                label: "wk".into(),
                used_percent: pct_to_used(remaining),
                reset_at: parse_ms(remains.weekly_end_time)
                    .or_else(|| now_plus_ms(remains.weekly_remains_time)),
                bar_visible: true,
                is_unlimited: remains.current_weekly_status == Some(3),
                used_absolute: None,
                limit_absolute: None,
            });
        }

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "5h");
        assert!((windows[0].used_percent - 10.0).abs() < 1e-3);
    }

    #[test]
    fn format_summary_handles_5h_and_wk_labels() {
        // The Settings → Test button output path needs to render both labels.
        // Mirrors the short-label mapping in `format_summary`.
        let snap = UsageSnapshot {
            provider: PROVIDER_LABEL.to_string(),
            level: None,
            windows: vec![
                UsageWindow {
                    label: "5h".into(),
                    used_percent: 20.0,
                    reset_at: None,
                    bar_visible: true,
                    is_unlimited: false,
                    used_absolute: None,
                    limit_absolute: None,
                },
                UsageWindow {
                    label: "wk".into(),
                    used_percent: 26.5,
                    reset_at: None,
                    bar_visible: true,
                    is_unlimited: false,
                    used_absolute: None,
                    limit_absolute: None,
                },
            ],
            unavailable_reason: None,
            fetched_at: Utc::now(),
        };
        let summary = format_summary(&snap);
        // format_summary renders "{:.0}% {label} left" per window joined by
        // " · ". 100 - 20 = 80 remaining for the 5h window, 100 - 26.5 = 73.5
        // → rounded to 74% for the wk window.
        assert!(summary.contains("80% 5h left"), "summary was: {summary}");
        assert!(summary.contains("74% wk left"), "summary was: {summary}");
    }
}

// ============================================================
// Response shape — canonical Token Plan field names per the research
// fixture. Field drift is handled by `#[serde(default)]` so a missing
// field becomes None instead of a decode error.
// ============================================================

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    base_resp: Option<BaseResp>,
    #[serde(default)]
    status_code: Option<i32>,
    #[serde(default)]
    model_remains: Option<Vec<ModelRemains>>,
}

#[derive(Deserialize)]
struct BaseResp {
    status_code: i32,
    #[serde(default)]
    #[allow(dead_code)]
    status_msg: Option<String>,
}

#[derive(Deserialize, Default)]
struct ModelRemains {
    #[serde(default)]
    model_name: Option<String>,
    /// 5-hour window: percent REMAINING (not used). Canonical Token Plan name.
    #[serde(default)]
    current_interval_remaining_percent: Option<f64>,
    /// 7-day window: percent REMAINING. Emitted as the second "wk" bar.
    /// Per platform.minimax.io/docs/token-plan, Token Plan usage is bounded
    /// by *both* a 5-hour rolling window and a separate weekly window.
    #[serde(default)]
    current_weekly_remaining_percent: Option<f64>,
    /// 3 means the account has no weekly quota; 1 means an active quota.
    #[serde(default)]
    current_weekly_status: Option<i32>,
    /// Absolute unix-ms end of the 5h window.
    #[serde(default)]
    end_time: Option<i64>,
    /// Absolute unix-ms end of the weekly window.
    #[serde(default)]
    weekly_end_time: Option<i64>,
    /// Relative ms-until-reset (5h). Fallback when end_time is absent.
    #[serde(default)]
    remains_time: Option<i64>,
    /// Relative ms-until-reset (weekly). Fallback when weekly_end_time is absent.
    #[serde(default)]
    weekly_remains_time: Option<i64>,
}

/// Convert the API's REMAINING percent to the USED percent the bar plots.
/// `100 - remaining`; clamp absorbs minor over/under-shoots from schema drift.
fn pct_to_used(remaining: f64) -> f32 {
    (100.0 - remaining).clamp(0.0, 100.0) as f32
}
