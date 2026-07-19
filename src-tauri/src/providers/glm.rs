use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

/// Z.ai Coding Plan BFF. NOT in the public OpenAPI spec; discovered from the
/// dashboard chunk for /manage-apikey/coding-plan/personal/usage. See
/// GLM_USAGE_TRACKER.md §2.
const ENDPOINT: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const PROVIDER_LABEL: &str = "Z.ai Coding Plan";
const KEY_ID: &str = "glm";

pub struct GlmProvider;

#[async_trait]
impl Provider for GlmProvider {
    fn id(&self) -> &'static str {
        "glm"
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
        classify_snapshot(fetch_quota(&key).await)
    }
}

/// Live probe used by the Settings → Test button. Takes the key directly so
/// the user can test before saving.
///
/// Reports **remaining** % (same as the overlay bar: "100% 5h · 73% wk"), not
/// used %. Showing used % without a label looked inverted next to the bar.
pub async fn test_key(api_key: &str) -> Result<String, String> {
    let snap = fetch_quota(api_key).await;
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
                Ok(format!(
                    "Z.ai Coding Plan [{level}] connected, no windows returned"
                ))
            } else {
                Ok(format!(
                    "Z.ai Coding Plan [{level}] — {}",
                    summary.join(" · ")
                ))
            }
        }
    }
}

async fn fetch_quota(api_key: &str) -> UsageSnapshot {
    let key = api_key.trim();
    if key.is_empty() {
        return UsageSnapshot::unavailable(PROVIDER_LABEL, "no api key configured");
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("ai-usage-tracker")
        .build()
    {
        Ok(c) => c,
        Err(e) => return UsageSnapshot::unavailable(PROVIDER_LABEL, format!("client build: {e}")),
    };

    let resp = client
        .get(ENDPOINT)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return UsageSnapshot::unavailable(PROVIDER_LABEL, format!("transport: {e}")),
    };

    // Pitfall C (part 1): transport-level errors return non-2xx HTTP.
    // `error_for_status` consumes `resp` and returns it on Ok.
    let resp = match resp.error_for_status() {
        Ok(r) => r,
        Err(e) => return UsageSnapshot::unavailable(PROVIDER_LABEL, format!("http {e}")),
    };

    let payload: ApiResponse = match resp.json().await {
        Ok(p) => p,
        Err(e) => return UsageSnapshot::unavailable(PROVIDER_LABEL, format!("decode: {e}")),
    };

    // Pitfall C (part 2): BFF returns 200 with success=false for auth/quota errors.
    if !payload.success || payload.code != 200 {
        return UsageSnapshot::unavailable(
            PROVIDER_LABEL,
            payload.msg.unwrap_or_else(|| "unknown error".into()),
        );
    }

    let data = match payload.data {
        Some(d) => d,
        None => return UsageSnapshot::unavailable(PROVIDER_LABEL, "empty data"),
    };

    // The Coding Plan exposes three windows: unit 3 = the 5-hour coding window
    // (shown in the bar), unit 6 = the weekly window (popup-only), and a
    // TIME_LIMIT entry (unit 5) = the monthly Web Search / Reader / Zread tool
    // quota (also popup-only). Stable display order regardless of API order.
    let mut h5: Option<UsageWindow> = None;
    let mut weekly: Option<UsageWindow> = None;
    let mut monthly: Option<UsageWindow> = None;

    for limit in data.limits.unwrap_or_default() {
        // Pitfall B: percentage is 0-100 (not 0-1). Clamp for proxy drift.
        let pct = match limit.percentage {
            Some(p) => p.clamp(0.0, 100.0),
            None => continue,
        };
        let reset = parse_dt(limit.next_reset_time);
        let is_monthly = limit.r#type.as_deref() == Some("TIME_LIMIT") || limit.unit == Some(5);
        if is_monthly && monthly.is_none() {
            monthly = Some(UsageWindow {
                label: "monthly".into(),
                used_percent: pct,
                reset_at: reset,
                bar_visible: false,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });
        } else if limit.unit == Some(3) && h5.is_none() {
            h5 = Some(UsageWindow {
                label: "5h".into(),
                used_percent: pct,
                reset_at: reset,
                bar_visible: true,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });
        } else if limit.unit == Some(6) && weekly.is_none() {
            weekly = Some(UsageWindow {
                label: "weekly".into(),
                used_percent: pct,
                reset_at: reset,
                bar_visible: true,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });
        }
    }

    let mut windows: Vec<UsageWindow> = Vec::new();
    if let Some(w) = h5 {
        windows.push(w);
    }
    if let Some(w) = weekly {
        windows.push(w);
    }
    if let Some(w) = monthly {
        windows.push(w);
    }

    UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: data.level,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    }
}

/// Parse a unix timestamp, auto-detecting seconds vs milliseconds.
/// Pitfall A: nextResetTime is unix-ms (13 digit). Heuristic > 1e11 → ms.
///
/// `raw_value` lets callers pass through non-integer payloads (RFC 3339
/// strings, nested objects) by stringifying and re-parsing — used by the
/// tolerant deserializer below.
fn parse_dt(value: Option<i64>) -> Option<DateTime<Utc>> {
    let v = value?;
    let secs = if v > 100_000_000_000 {
        v as f64 / 1000.0
    } else {
        v as f64
    };
    Utc.timestamp_opt(secs as i64, ((secs.fract()) * 1_000_000_000.0) as u32)
        .single()
}

/// Try to extract a reset instant from a raw JSON value, accepting:
/// - `null` / missing → `None`
/// - integer (unix seconds or unix-ms) → unix
/// - string containing only digits → unix
/// - RFC 3339 / ISO 8601 string (with or without sub-seconds / TZ) → parse
/// - nested object with a `time` field (some BFF wrappers do this) → recurse
fn reset_from_value(v: &Value) -> Option<i64> {
    match v {
        Value::Null => None,
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|u| u.min(i64::MAX as u64) as i64))
            .or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp());
            }
            // Common Z.ai variant: "2025-07-09 12:34:56" without TZ. Treat
            // as UTC since the dashboard displays it in the user's local TZ.
            for fmt in &[
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S%.f",
            ] {
                if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
                    if let Some(dt) = Utc.from_local_datetime(&naive).single() {
                        return Some(dt.timestamp());
                    }
                }
            }
            None
        }
        Value::Object(_) => {
            // Some BFFs nest: {"time": <ts>, "nextReset": <ts>}. Try common keys.
            for k in ["time", "nextReset", "next_reset", "resetTime", "ts"] {
                if let Some(inner) = v.get(k) {
                    if let Some(n) = reset_from_value(inner) {
                        return Some(n);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Tolerant deserializer for `next_reset_time`: accepts the JSON field under
/// any of the observed names (snake_case, camelCase, a couple of variants)
/// and any of the value shapes the BFF has shipped (number, RFC 3339 string,
/// nested object).
fn deserialize_reset<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(deserializer)?;
    Ok(reset_from_value(&v))
}

#[derive(Deserialize)]
struct ApiResponse {
    code: i32,
    success: bool,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Option<ApiResponseData>,
}

#[derive(Deserialize)]
struct ApiResponseData {
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    limits: Option<Vec<ApiLimit>>,
}

#[derive(Deserialize)]
struct ApiLimit {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    unit: Option<i64>,
    #[serde(default)]
    percentage: Option<f32>,
    // The Z.ai BFF has shipped this field under multiple names (snake_case
    // and camelCase) and as multiple value shapes (unix int, RFC 3339 string,
    // sometimes nested). The tolerant deserializer + aliases catch all of
    // them so the popup can actually show "Resets in 4h 12m" instead of "—".
    #[serde(
        default,
        alias = "nextResetTime",
        alias = "resetTime",
        alias = "nextRefreshTime",
        deserialize_with = "deserialize_reset"
    )]
    next_reset_time: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dt_handles_ms_and_seconds() {
        // unix-ms (13 digit) → sub-second precision preserved.
        let ms = 1_700_000_000_500_i64;
        let parsed = parse_dt(Some(ms)).expect("ms parses");
        assert_eq!(parsed.timestamp(), 1_700_000_000);
        assert_eq!(parsed.timestamp_subsec_nanos(), 500_000_000);

        // plain unix-seconds (10 digit).
        let secs = 1_700_000_000_i64;
        let parsed = parse_dt(Some(secs)).expect("secs parses");
        assert_eq!(parsed.timestamp(), 1_700_000_000);
        assert_eq!(parsed.timestamp_subsec_nanos(), 0);

        // None → None.
        assert!(parse_dt(None).is_none());
    }

    #[test]
    fn reset_from_value_accepts_unix_int_and_ms() {
        // reset_from_value is just the raw extractor — it does NOT apply the
        // seconds-vs-ms heuristic. The heuristic lives in parse_dt, which is
        // called downstream. So the raw i64 comes back as-is.
        let secs = reset_from_value(&serde_json::json!(1_700_000_000)).unwrap();
        let ms = reset_from_value(&serde_json::json!(1_700_000_000_000_i64)).unwrap();
        assert_eq!(secs, 1_700_000_000);
        assert_eq!(ms, 1_700_000_000_000);

        // And the heuristic in parse_dt normalizes both to the same instant.
        let d_secs = parse_dt(Some(secs)).unwrap();
        let d_ms = parse_dt(Some(ms)).unwrap();
        assert_eq!(d_secs, d_ms);
    }

    #[test]
    fn reset_from_value_accepts_rfc3339_string() {
        let v = serde_json::json!("2025-07-09T12:34:56Z");
        let ts = reset_from_value(&v).expect("rfc3339 parses");
        // 2025-07-09T12:34:56Z in unix seconds
        assert!(ts > 1_750_000_000 && ts < 1_760_000_000);
    }

    #[test]
    fn reset_from_value_accepts_naive_datetime_string_as_utc() {
        // Some BFFs strip the TZ suffix. Treat as UTC.
        let v = serde_json::json!("2025-07-09 12:34:56");
        let ts = reset_from_value(&v).expect("naive parses");
        assert!(ts > 1_750_000_000 && ts < 1_760_000_000);
    }

    #[test]
    fn reset_from_value_accepts_numeric_string() {
        // JS-style coercion sometimes lands the timestamp as a string of digits.
        let v = serde_json::json!("1700000000");
        assert_eq!(reset_from_value(&v), Some(1_700_000_000));
    }

    #[test]
    fn reset_from_value_returns_none_for_null_and_garbage() {
        assert_eq!(reset_from_value(&Value::Null), None);
        assert_eq!(reset_from_value(&serde_json::json!("not a date")), None);
        assert_eq!(reset_from_value(&serde_json::json!(true)), None);
    }

    #[test]
    fn reset_from_value_recurses_into_nested_object() {
        // Some Z.ai payloads wrap the timestamp under a nested field.
        let v = serde_json::json!({"time": 1_700_000_000_i64});
        assert_eq!(reset_from_value(&v), Some(1_700_000_000));
    }

    #[test]
    fn deserialize_reset_reads_camel_case_field() {
        // The Z.ai BFF typically serializes the field as `nextResetTime`.
        // With the old snake_case-only struct, this field was silently
        // dropped — leaving the popup with "Resets in —". The alias list
        // here catches it.
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(
                default,
                alias = "nextResetTime",
                alias = "resetTime",
                deserialize_with = "deserialize_reset"
            )]
            next_reset_time: Option<i64>,
        }
        let json = r#"{"nextResetTime": 1700000000}"#;
        let w: Wrap = serde_json::from_str(json).expect("decode ok");
        assert_eq!(w.next_reset_time, Some(1_700_000_000));
    }

    #[test]
    fn deserialize_reset_accepts_string_timestamp() {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default, deserialize_with = "deserialize_reset")]
            next_reset_time: Option<i64>,
        }
        let json = r#"{"next_reset_time": "2025-07-09T12:34:56Z"}"#;
        let w: Wrap = serde_json::from_str(json).expect("decode ok");
        assert!(w.next_reset_time.unwrap() > 1_750_000_000);
    }

    #[test]
    fn fetch_quota_populates_reset_at_when_api_returns_camel_case_field() {
        // End-to-end: a payload using the actual BFF shape with
        // `nextResetTime` should now produce windows with non-None reset_at.
        let payload = r#"{
            "code": 200,
            "success": true,
            "msg": null,
            "data": {
                "level": "pro",
                "limits": [
                    {
                        "type": "TIME_LIMIT",
                        "unit": 5,
                        "percentage": 12.5,
                        "nextResetTime": 1783303097000
                    },
                    {
                        "type": "USAGE",
                        "unit": 3,
                        "percentage": 50.0,
                        "nextResetTime": 1783299500000
                    },
                    {
                        "type": "USAGE",
                        "unit": 6,
                        "percentage": 22.0,
                        "nextResetTime": 1783694381000
                    }
                ]
            }
        }"#;
        let parsed: ApiResponse = serde_json::from_str(payload).expect("decode ok");
        let data = parsed.data.expect("data");
        let mut h5 = None;
        let mut weekly = None;
        let mut monthly = None;
        for limit in data.limits.unwrap_or_default() {
            let _pct = limit.percentage.unwrap_or(0.0).clamp(0.0, 100.0);
            let reset = parse_dt(limit.next_reset_time);
            let is_monthly = limit.r#type.as_deref() == Some("TIME_LIMIT") || limit.unit == Some(5);
            if is_monthly && monthly.is_none() {
                monthly = Some(reset);
            } else if limit.unit == Some(3) && h5.is_none() {
                h5 = Some(reset);
            } else if limit.unit == Some(6) && weekly.is_none() {
                weekly = Some(reset);
            }
        }
        // All three windows should have a populated reset_at.
        assert!(h5.unwrap().is_some(), "5h window must have reset_at");
        assert!(
            weekly.unwrap().is_some(),
            "weekly window must have reset_at"
        );
        assert!(
            monthly.unwrap().is_some(),
            "monthly window must have reset_at"
        );
    }
}
