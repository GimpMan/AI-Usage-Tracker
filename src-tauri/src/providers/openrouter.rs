//! OpenRouter prepaid-credits tracking.
//!
//! OpenRouter's model is different from the other providers: it's a **prepaid
//! credit balance** with optional per-key daily/weekly/monthly spend limits,
//! not a subscription quota with a fixed reset window. We surface:
//!
//! * **Bar** (when either source has displayable data): the normal key's
//!   `daily`/`weekly`/`monthly` limit window and/or the Management key's
//!   account balance. A normal key with no limit needs the Management key to
//!   produce an overlay segment.
//! * **Popup**: per-key metadata plus account-wide balance —
//!   `total_credits - total_usage` USD — when a Management key is configured.
//! * **Top-up detection**: when `total_credits` rises above the previously
//!   persisted baseline, we treat that as a top-up, snap the baseline to the
//!   new value, and surface "Top-up detected" in the snapshot's `level` field
//!   so the UI can flash it.
//!
//! Auth: the normal API key uses `GET https://openrouter.ai/api/v1/key` for
//! per-key windows and the separate Management key uses
//! `GET https://openrouter.ai/api/v1/credits` for account-wide balance.
//! See the [limits](https://openrouter.ai/docs/api/reference/limits),
//! [current-key](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key),
//! and [credits](https://openrouter.ai/docs/api/api-reference/credits/get-credits)
//! documentation, plus the repository design spec.

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::Deserialize;

use super::{
    classify_snapshot, Provider, ProviderFetch, ProviderHealth, UsageSnapshot, UsageWindow,
};
use crate::secrets::{AccountBalanceBaseline, Secrets, TopupBaseline};

const KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const PROVIDER_LABEL: &str = "OpenRouter";
const KEY_ID: &str = "openrouter";
const MANAGEMENT_KEY_ID: &str = "openrouter_management";

const LIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

// Soft-empty reasons (frontend may special-case by prefix later).
const REASON_NO_KEY: &str = "no api key configured";
const REASON_TRANSPORT: &str = "network error";
const REASON_DECODE: &str = "decode error";
const REASON_INVALID: &str = "invalid api key";

pub struct OpenrouterProvider;

#[async_trait]
impl Provider for OpenrouterProvider {
    fn id(&self) -> &'static str {
        KEY_ID
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, secrets: &Secrets) -> ProviderFetch {
        let normal_key = secrets.get(KEY_ID);
        let management_key = secrets.get(MANAGEMENT_KEY_ID);
        fetch_provider(
            normal_key.as_deref(),
            management_key.as_deref(),
            TopupBaseline::load(MANAGEMENT_KEY_ID),
            AccountBalanceBaseline::load(MANAGEMENT_KEY_ID),
            true,
        )
        .await
    }
}

// ============================================================
// Top-up baseline (persisted in app config so top-ups survive
// restarts and we don't reset the bar on every restart).
// ============================================================

/// Fetch the current account balance and make it the local budget baseline.
/// This changes only tracker state; it does not modify OpenRouter credits.
pub async fn rebase_account(api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .user_agent("ai-usage-tracker/0.1")
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let credits = fetch_credits_data(&client, api_key)
        .await
        .map_err(|error| error.reason)?;
    let (Some(total), Some(used)) = (credits.total_credits, credits.total_usage) else {
        return Err("no account balance returned".into());
    };
    let remaining = (total - used).max(0.0);
    AccountBalanceBaseline::save(
        MANAGEMENT_KEY_ID,
        AccountBalanceBaseline {
            balance: remaining,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        },
    )?;
    // Pin total_credits too so the next scheduled fetch doesn't treat the
    // current total as a top-up and auto-reset the rebase.
    TopupBaseline::save(
        MANAGEMENT_KEY_ID,
        TopupBaseline {
            total_credits: total,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        },
    )?;
    Ok(format!("Account balance rebased to ${remaining:.2}"))
}

/// Live probe used by the Settings → Test button. Validates the bearer key
/// against `/api/v1/key`, reports what we can see (key label, limit, lifetime
/// balance), and surfaces a short user-facing summary.
pub async fn test_key(api_key: &str) -> Result<String, String> {
    let result = fetch_provider(Some(api_key), None, None, None, false).await;
    let snap = result.snapshot.unwrap_or_else(|| {
        UsageSnapshot::unavailable(
            PROVIDER_LABEL,
            result.reason.unwrap_or_else(|| "no usable details".into()),
        )
    });
    match snap.unavailable_reason {
        Some(err) => Err(err),
        None => {
            let label = snap.level.unwrap_or_else(|| "?".into());
            if snap.windows.is_empty() {
                Ok(format!(
                    "OpenRouter [{label}] connected, no windows returned"
                ))
            } else {
                let window_summaries: Vec<String> = snap
                    .windows
                    .iter()
                    .map(|w| {
                        let left = (100.0 - w.used_percent).clamp(0.0, 100.0);
                        format!("{:.0}% {} left", left, super::short_window_label(&w.label))
                    })
                    .collect();
                Ok(format!(
                    "OpenRouter [{label}] — {}",
                    window_summaries.join(" · ")
                ))
            }
        }
    }
}

/// Live probe for the optional account-wide Management key. This deliberately
/// does not persist a top-up baseline because the field may contain an unsaved
/// draft key from Settings.
pub async fn test_management_key(api_key: &str) -> Result<String, String> {
    let result = fetch_provider(None, Some(api_key), None, None, false).await;
    let snap = result
        .snapshot
        .ok_or_else(|| result.reason.unwrap_or_else(|| "no usable details".into()))?;
    let balance = snap
        .windows
        .iter()
        .find(|window| window.label.starts_with("balance "))
        .map(|window| window.label.trim_start_matches("balance ").to_string())
        .unwrap_or_else(|| "unknown".into());
    Ok(format!("OpenRouter Management — {balance} remaining"))
}

// ============================================================
// Live fetch
// ============================================================

#[derive(Debug)]
struct SourceError {
    health: ProviderHealth,
    reason: String,
}

impl SourceError {
    fn new(health: ProviderHealth, reason: impl Into<String>) -> Self {
        Self {
            health,
            reason: reason.into(),
        }
    }
}

async fn fetch_provider(
    normal_key: Option<&str>,
    management_key: Option<&str>,
    prev_baseline: Option<TopupBaseline>,
    prev_account_baseline: Option<AccountBalanceBaseline>,
    persist_baseline: bool,
) -> ProviderFetch {
    if normal_key.is_none() && management_key.is_none() {
        return ProviderFetch::hard(ProviderHealth::MissingCredentials, REASON_NO_KEY);
    }

    let client = match reqwest::Client::builder()
        .timeout(LIVE_TIMEOUT)
        .user_agent("ai-usage-tracker/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => return ProviderFetch::transient(format!("client build: {e}")),
    };

    let key_result = match normal_key {
        Some(key) => Some(fetch_key_data(&client, key).await),
        None => None,
    };
    let credits_result = match management_key {
        Some(key) => Some(fetch_credits_data(&client, key).await),
        None => None,
    };

    let key_data = key_result.as_ref().and_then(|r| r.as_ref().ok());
    let credits_data = credits_result.as_ref().and_then(|r| r.as_ref().ok());
    let snapshot = if persist_baseline {
        build_snapshot_with_account_baseline(
            key_data,
            credits_data,
            prev_baseline,
            prev_account_baseline,
            true,
        )
    } else {
        build_snapshot_with_account_baseline(
            key_data,
            credits_data,
            prev_baseline,
            prev_account_baseline,
            false,
        )
    };
    if !snapshot.windows.is_empty() {
        return classify_snapshot(snapshot);
    }

    let mut errors = Vec::new();
    if let Some(Err(error)) = key_result {
        errors.push(error);
    }
    if let Some(Err(error)) = credits_result {
        errors.push(error);
    }
    let mut first_error: Option<SourceError> = None;
    for error in errors {
        if error.health == ProviderHealth::TransientFailure {
            return ProviderFetch::transient(error.reason);
        }
        if first_error.is_none() {
            first_error = Some(error);
        }
    }
    if let Some(error) = first_error {
        ProviderFetch::hard(error.health, error.reason)
    } else {
        ProviderFetch::hard(ProviderHealth::NoUsableDetails, "no usable usage details")
    }
}

async fn fetch_key_data(client: &reqwest::Client, api_key: &str) -> Result<KeyData, SourceError> {
    let response = client
        .get(KEY_URL)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            SourceError::new(
                ProviderHealth::TransientFailure,
                format!("{REASON_TRANSPORT}: key: {e}"),
            )
        })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(SourceError::new(
            ProviderHealth::InvalidCredentials,
            REASON_INVALID,
        ));
    }
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(SourceError::new(
            ProviderHealth::TransientFailure,
            format!("api key {status}"),
        ));
    }
    if !status.is_success() {
        return Err(SourceError::new(
            ProviderHealth::InvalidCredentials,
            format!("api key {status}"),
        ));
    }
    let body: KeyResponse = response.json().await.map_err(|e| {
        SourceError::new(
            ProviderHealth::TransientFailure,
            format!("{REASON_DECODE}: key: {e}"),
        )
    })?;
    body.data
        .ok_or_else(|| SourceError::new(ProviderHealth::NoUsableDetails, "empty key data"))
}

async fn fetch_credits_data(
    client: &reqwest::Client,
    management_key: &str,
) -> Result<CreditsData, SourceError> {
    let response = client
        .get(CREDITS_URL)
        .bearer_auth(management_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            SourceError::new(
                ProviderHealth::TransientFailure,
                format!("{REASON_TRANSPORT}: credits: {e}"),
            )
        })?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(SourceError::new(
            ProviderHealth::InvalidCredentials,
            "invalid management api key",
        ));
    }
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(SourceError::new(
            ProviderHealth::TransientFailure,
            format!("credits {status}"),
        ));
    }
    if !status.is_success() {
        return Err(SourceError::new(
            ProviderHealth::InvalidCredentials,
            format!("credits {status}"),
        ));
    }
    let body: CreditsResponse = response.json().await.map_err(|e| {
        SourceError::new(
            ProviderHealth::TransientFailure,
            format!("{REASON_DECODE}: credits: {e}"),
        )
    })?;
    body.data
        .ok_or_else(|| SourceError::new(ProviderHealth::NoUsableDetails, "empty credits data"))
}

#[cfg(test)]
fn build_snapshot(
    key: Option<&KeyData>,
    credits: Option<&CreditsData>,
    prev_baseline: Option<TopupBaseline>,
) -> UsageSnapshot {
    build_snapshot_with_account_baseline(key, credits, prev_baseline, None, true)
}

/// Infer the next reset instant for an OpenRouter per-key spend limit.
///
/// OpenRouter never returns a reset timestamp, but its docs define the cadence:
///   daily   = current UTC day          → resets at next 00:00 UTC
///   weekly  = current UTC week (Mon)   → resets at next Monday 00:00 UTC
///   monthly = current UTC month        → resets at 1st of next month 00:00 UTC
/// Any other `limit_reset` (e.g. `"total"` / lifetime) returns `None`.
fn infer_openrouter_reset(limit_reset: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    let midnight = |date: NaiveDate| date.and_hms_opt(0, 0, 0).expect("valid midnight");
    let reset = match limit_reset {
        "daily" => Utc.from_utc_datetime(&midnight(today + Duration::days(1))),
        "weekly" => {
            // num_days_from_monday(): Mon=0 .. Sun=6.
            let weekday = now.weekday().num_days_from_monday() as i64;
            let days_to_monday = if weekday == 0 { 7 } else { 7 - weekday };
            Utc.from_utc_datetime(&midnight(today + Duration::days(days_to_monday)))
        }
        "monthly" => {
            let (year, month) = if now.month() == 12 {
                (now.year() + 1, 1u32)
            } else {
                (now.year(), now.month() + 1)
            };
            let first = NaiveDate::from_ymd_opt(year, month, 1)?;
            Utc.from_utc_datetime(&midnight(first))
        }
        _ => return None,
    };
    Some(reset)
}

fn build_snapshot_with_account_baseline(
    key: Option<&KeyData>,
    credits: Option<&CreditsData>,
    prev_baseline: Option<TopupBaseline>,
    prev_account_baseline: Option<AccountBalanceBaseline>,
    persist_baseline: bool,
) -> UsageSnapshot {
    let mut windows: Vec<UsageWindow> = Vec::new();
    let mut level: Option<String> = None;
    let mut topup_note: Option<String> = None;

    // Per-key limit bar. A key with a reset cadence uses the matching period
    // usage. With `limit_reset: null`, OpenRouter treats the cap as lifetime
    // credit, so use the all-time `usage`/`limit_remaining` fields instead.
    if let Some(key) = key {
        let (label, used, limit) = match key.limit_reset.as_deref() {
            Some("daily") => (Some("daily"), key.usage_daily, key.limit),
            Some("weekly") => (Some("weekly"), key.usage_weekly, key.limit),
            Some("monthly") => (Some("monthly"), key.usage_monthly, key.limit),
            None => {
                let limit = key.limit;
                let used = key.usage.or_else(|| {
                    limit.and_then(|limit| key.limit_remaining.map(|remaining| limit - remaining))
                });
                (Some("total"), used, limit)
            }
            _ => (None, None, None),
        };
        if let (Some(label), Some(used), Some(limit)) = (label, used, limit) {
            if limit > 0.0 {
                let pct = ((used / limit) * 100.0).clamp(0.0, 100.0) as f32;
                let display_label = if label == "total" {
                    let remaining = key
                        .limit_remaining
                        .or_else(|| key.usage.map(|used| limit - used))
                        .unwrap_or(limit)
                        .clamp(0.0, limit);
                    format!("total ${remaining:.4}")
                } else {
                    label.to_string()
                };
                windows.push(UsageWindow {
                    label: display_label,
                    used_percent: pct,
                    reset_at: infer_openrouter_reset(label, Utc::now()),
                    bar_visible: true,
                    is_unlimited: false,
                    // Typed USD counters — the popup shows "$3.90 / $15.00
                    // used" plus a month-end projection for monthly windows.
                    used_absolute: Some(used),
                    limit_absolute: Some(limit),
                });

                // The lifetime-key response also contains the three useful
                // spend summaries shown by the OpenRouter dashboard. They
                // belong in the popup, not the collapsed bar.
                if label == "total" {
                    for (label, amount) in [
                        ("today", key.usage_daily),
                        ("this week", key.usage_weekly),
                        ("this month", key.usage_monthly),
                    ] {
                        if let Some(amount) = amount {
                            windows.push(UsageWindow {
                                label: format!("{label} ${amount:.4}"),
                                used_percent: 0.0,
                                reset_at: None,
                                bar_visible: false,
                                is_unlimited: false,
                                used_absolute: None,
                                limit_absolute: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Lifetime balance popup window — always present when we have both numbers.
    if let Some(credits) = credits {
        if let (Some(total), Some(used)) = (credits.total_credits, credits.total_usage) {
            let remaining = (total - used).max(0.0);
            let previous_total = prev_baseline
                .as_ref()
                .map(|baseline| baseline.total_credits);
            let previous_balance = prev_account_baseline
                .as_ref()
                .map(|baseline| baseline.balance);
            let total_increased = previous_total
                .map(|previous| total > previous + 0.005)
                .unwrap_or(false);
            // Only an actual top-up (total_credits rose) should reset the local
            // rebase baseline. A refund or small balance fluctuation must keep
            // the user-chosen baseline so spending since the rebase keeps counting.
            let effective_total = match (previous_balance, total_increased) {
                (Some(previous), false) => previous,
                (Some(_), true) => remaining,
                (None, _) => total,
            };
            let effective_used = (effective_total - remaining).max(0.0);
            // Show as a popup-only "balance" window. used_percent here is the
            // share of total_credits that's already been spent, so a fuller
            // account reads lower used_percent. That inverts vs. subscription
            // quotas — but the UI renders it the same way (a bar with the used
            // fraction filled). For OpenRouter the popup text will read e.g.
            // "balance — $12.40 remaining".
            let pct = if effective_total > 0.0 {
                ((effective_used / effective_total) * 100.0).clamp(0.0, 100.0) as f32
            } else {
                0.0
            };
            windows.push(UsageWindow {
                label: format!("balance ${:.2}", remaining),
                used_percent: pct,
                reset_at: None,
                bar_visible: true,
                is_unlimited: false,
                used_absolute: None,
                limit_absolute: None,
            });

            // Top-up detection: if `total_credits` increased above the persisted
            // baseline, treat that as a credit purchase and snap the baseline up.
            // The note flashes "Top-up detected" so the UI can highlight it.
            let new_baseline_total = match previous_total {
                Some(prev) if total > prev + 0.005 => {
                    topup_note = Some(format!("Top-up detected (+${:.2})", total - prev));
                    Some(total)
                }
                Some(prev) => Some(prev),
                None => {
                    // First fetch ever — establish the baseline, but don't
                    // claim it was a top-up (it could just be the user's first
                    // ever usage window).
                    Some(total)
                }
            };
            let new_balance_baseline =
                previous_balance.map(
                    |previous| {
                        if total_increased {
                            remaining
                        } else {
                            previous
                        }
                    },
                );
            if persist_baseline {
                let _ = TopupBaseline::save(
                    MANAGEMENT_KEY_ID,
                    TopupBaseline {
                        total_credits: new_baseline_total.unwrap_or(total),
                        saved_at: Utc::now(),
                        extra: serde_json::Map::new(),
                    },
                );
            }

            if persist_baseline {
                if let Some(balance) = new_balance_baseline {
                    let _ = AccountBalanceBaseline::save(
                        MANAGEMENT_KEY_ID,
                        AccountBalanceBaseline {
                            balance,
                            saved_at: Utc::now(),
                            extra: serde_json::Map::new(),
                        },
                    );
                }
            }

            if let Some(note) = topup_note {
                level = Some(note);
            }
        } else if let Some(prev) = prev_baseline {
            // No /credits data this tick but we still have a baseline — keep it.
            // (Defensive: shouldn't normally happen, but if /credits 500s we
            // don't want to lose the baseline.)
            if persist_baseline {
                let _ = TopupBaseline::save(MANAGEMENT_KEY_ID, prev);
            }
        }
    }

    // Surface key metadata for the popup: tier label and a short limit
    // summary so the user knows what kind of key they're tracking.
    let key_label = match key {
        Some(key) => match (&key.label, key.is_free_tier) {
            (Some(l), _) if !l.is_empty() => Some(l.clone()),
            (_, true) => Some("free tier".to_string()),
            _ => None,
        },
        None => None,
    };
    let limit_summary = key.and_then(|key| {
        key.limit.and_then(|l| {
            Some(match key.limit_reset.as_deref() {
                Some(reset) => format!("${l:.2} {reset} limit"),
                None => format!("${l:.2} lifetime limit"),
            })
        })
    });
    let combined_level = match (level, key_label, limit_summary) {
        (Some(note), Some(label), Some(limit)) => Some(format!("{note} · {label} ({limit})")),
        (Some(note), Some(label), None) => Some(format!("{note} · {label}")),
        (Some(note), None, Some(limit)) => Some(format!("{note} · {limit}")),
        (Some(note), None, None) => Some(note),
        (None, Some(label), Some(limit)) => Some(format!("{label} ({limit})")),
        (None, Some(label), None) => Some(label),
        (None, None, Some(limit)) => Some(limit),
        (None, None, None) if credits.is_some() => Some("account balance".to_string()),
        (None, None, None) => None,
    };

    UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: combined_level,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    }
}

// ============================================================
// Wire types — only the fields we read.
// ============================================================

#[derive(Deserialize)]
struct Envelope<T> {
    data: Option<T>,
}

type KeyResponse = Envelope<KeyData>;
type CreditsResponse = Envelope<CreditsData>;

#[derive(Deserialize)]
struct KeyData {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    is_free_tier: bool,
    /// USD spend cap on the key. `None` ⇒ unlimited key (no bar window).
    #[serde(default)]
    limit: Option<f64>,
    /// Remaining USD under the key cap. Present for lifetime limits and more
    /// precise than deriving the value from rounded usage.
    #[serde(default)]
    limit_remaining: Option<f64>,
    /// Which window the `limit` is enforced against. Drives which of
    /// `usage_daily` / `usage_weekly` / `usage_monthly` we surface on the bar.
    /// Per docs: `null | "daily" | "weekly" | "monthly"`.
    #[serde(default)]
    limit_reset: Option<String>,
    /// All-time USD spend under the key.
    #[serde(default)]
    usage: Option<f64>,
    #[serde(default)]
    usage_daily: Option<f64>,
    #[serde(default)]
    usage_weekly: Option<f64>,
    #[serde(default)]
    usage_monthly: Option<f64>,
}

#[derive(Deserialize)]
struct CreditsData {
    #[serde(default)]
    total_credits: Option<f64>,
    #[serde(default)]
    total_usage: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_with_limit(reset: &str, limit: f64, used: f64) -> KeyData {
        KeyData {
            label: Some("test-key".into()),
            is_free_tier: false,
            limit: Some(limit),
            limit_remaining: Some(limit - used),
            limit_reset: Some(reset.into()),
            usage: Some(used),
            usage_daily: Some(if reset == "daily" { used } else { 0.0 }),
            usage_weekly: Some(if reset == "weekly" { used } else { 0.0 }),
            usage_monthly: Some(if reset == "monthly" { used } else { 0.0 }),
        }
    }

    fn credits(total: f64, used: f64) -> CreditsData {
        CreditsData {
            total_credits: Some(total),
            total_usage: Some(used),
        }
    }

    fn dt(rfc3339: &str) -> DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn infer_reset_daily_is_next_midnight_utc() {
        // 2026-07-11 is a Saturday at 09:30 UTC.
        let now = dt("2026-07-11T09:30:00Z");
        let reset = infer_openrouter_reset("daily", now).unwrap();
        assert_eq!(reset, dt("2026-07-12T00:00:00Z"));
    }

    #[test]
    fn infer_reset_weekly_is_next_monday_midnight_utc() {
        // 2026-07-11 is a Saturday; next Monday is 2026-07-13.
        let now = dt("2026-07-11T09:30:00Z");
        let reset = infer_openrouter_reset("weekly", now).unwrap();
        assert_eq!(reset, dt("2026-07-13T00:00:00Z"));
    }

    #[test]
    fn infer_reset_weekly_on_monday_is_seven_days_later() {
        // 2026-07-13 is a Monday; reset is the FOLLOWING Monday (not today).
        let now = dt("2026-07-13T00:05:00Z");
        let reset = infer_openrouter_reset("weekly", now).unwrap();
        assert_eq!(reset, dt("2026-07-20T00:00:00Z"));
    }

    #[test]
    fn infer_reset_monthly_is_first_of_next_month() {
        let now = dt("2026-07-11T09:30:00Z");
        let reset = infer_openrouter_reset("monthly", now).unwrap();
        assert_eq!(reset, dt("2026-08-01T00:00:00Z"));
    }

    #[test]
    fn infer_reset_monthly_rolls_year_in_december() {
        let now = dt("2026-12-31T23:59:00Z");
        let reset = infer_openrouter_reset("monthly", now).unwrap();
        assert_eq!(reset, dt("2027-01-01T00:00:00Z"));
    }

    #[test]
    fn infer_reset_non_cadence_returns_none() {
        let now = dt("2026-07-11T09:30:00Z");
        assert_eq!(infer_openrouter_reset("total", now), None);
        assert_eq!(infer_openrouter_reset("other", now), None);
    }

    #[test]
    fn weekly_limit_produces_one_bar_window() {
        let key = key_with_limit("weekly", 100.0, 37.0);
        let credit_data = credits(200.0, 50.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        // Expect one per-key weekly window plus one balance window.
        let bar: Vec<&UsageWindow> = snap.windows.iter().filter(|w| w.bar_visible).collect();
        let balance: Vec<&UsageWindow> = snap
            .windows
            .iter()
            .filter(|w| w.label.starts_with("balance "))
            .collect();
        assert_eq!(bar.len(), 2, "weekly and balance bar windows expected");
        assert_eq!(bar[0].label, "weekly");
        assert!((bar[0].used_percent - 37.0).abs() < 0.01);
        assert!(
            bar[0].reset_at.is_some(),
            "weekly limit window should infer a reset instant"
        );
        assert_eq!(balance.len(), 1, "balance window expected");
    }

    /// The per-key window carries typed USD counters so the popup can render
    /// "$37.00 / $100.00 used" and a month-end dollar projection.
    #[test]
    fn key_window_carries_typed_usd_counters() {
        let key = key_with_limit("monthly", 100.0, 37.0);
        let credit_data = credits(200.0, 50.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        let monthly = snap
            .windows
            .iter()
            .find(|w| w.label == "monthly")
            .expect("monthly key window");
        assert_eq!(monthly.used_absolute, Some(37.0));
        assert_eq!(monthly.limit_absolute, Some(100.0));
    }

    #[test]
    fn no_limit_skips_bar_window_but_keeps_balance_popup() {
        let mut key = key_with_limit("weekly", 100.0, 0.0);
        key.limit = None;
        key.limit_reset = None;
        let credit_data = credits(50.0, 5.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        let bar: Vec<&UsageWindow> = snap.windows.iter().filter(|w| w.bar_visible).collect();
        let popup: Vec<&UsageWindow> = snap
            .windows
            .iter()
            .filter(|w| w.label.starts_with("balance "))
            .collect();
        assert_eq!(bar.len(), 1, "balance segment still shown");
        assert_eq!(popup.len(), 1, "balance still shown");
        assert_eq!(popup[0].label, "balance $45.00");
    }

    #[test]
    fn lifetime_key_limit_produces_total_and_period_spend_details() {
        let key = KeyData {
            label: Some("WJC_Hub".into()),
            is_free_tier: false,
            limit: Some(10.0),
            limit_remaining: Some(1.0605),
            limit_reset: None,
            usage: Some(8.9395),
            usage_daily: Some(0.0),
            usage_weekly: Some(0.0033),
            usage_monthly: Some(0.2985),
        };

        let snap = build_snapshot(Some(&key), None, None);
        let total = snap
            .windows
            .iter()
            .find(|window| window.label.starts_with("total "))
            .expect("lifetime total window");
        assert!(total.bar_visible);
        assert_eq!(total.label, "total $1.0605");
        assert!((total.used_percent - 89.395).abs() < 0.01);

        assert!(snap
            .windows
            .iter()
            .any(|window| window.label == "today $0.0000" && !window.bar_visible));
        assert!(snap
            .windows
            .iter()
            .any(|window| window.label == "this week $0.0033" && !window.bar_visible));
        assert!(snap
            .windows
            .iter()
            .any(|window| window.label == "this month $0.2985" && !window.bar_visible));
    }

    #[test]
    fn rebased_account_balance_starts_at_current_remaining_value() {
        let credit_data = credits(100.0, 82.17);
        let prev = AccountBalanceBaseline {
            balance: 17.83,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };

        let snap =
            build_snapshot_with_account_baseline(None, Some(&credit_data), None, Some(prev), false);
        let balance = snap
            .windows
            .iter()
            .find(|window| window.label.starts_with("balance "))
            .expect("account balance window");
        assert_eq!(balance.label, "balance $17.83");
        assert!(balance.used_percent.abs() < f32::EPSILON);
    }

    #[test]
    fn rebased_account_balance_counts_only_spend_since_rebase() {
        let credit_data = credits(100.0, 85.0);
        let prev = AccountBalanceBaseline {
            balance: 17.83,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };

        let snap =
            build_snapshot_with_account_baseline(None, Some(&credit_data), None, Some(prev), false);
        let balance = snap
            .windows
            .iter()
            .find(|window| window.label.starts_with("balance "))
            .expect("account balance window");
        assert_eq!(balance.label, "balance $15.00");
        assert!((balance.used_percent - 15.87).abs() < 0.01);
    }

    #[test]
    fn rebase_baseline_survives_refund_without_total_credits_rising() {
        // User rebased at $90.00. A later refund bumps remaining back to $95.00
        // while total_credits stayed at $100. The rebase baseline must NOT reset,
        // so the bar still counts spend relative to the original $90 rebase.
        let credit_data = credits(100.0, 5.0);
        let account_prev = AccountBalanceBaseline {
            balance: 90.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };
        let topup_prev = TopupBaseline {
            total_credits: 100.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };

        let snap = build_snapshot_with_account_baseline(
            None,
            Some(&credit_data),
            Some(topup_prev),
            Some(account_prev),
            false,
        );
        let balance = snap
            .windows
            .iter()
            .find(|window| window.label.starts_with("balance "))
            .expect("account balance window");
        assert_eq!(balance.label, "balance $95.00");
        // effective_total stays at the rebase baseline (90), effective_used = 0.
        assert!(balance.used_percent.abs() < f32::EPSILON);
    }

    #[test]
    fn rebase_baseline_resets_on_actual_topup() {
        // User rebased at $90.00. They then top up so total_credits rises from
        // $100 to $120 and remaining becomes $110. The baseline should reset to
        // the new remaining so tracking starts fresh after the top-up.
        let credit_data = credits(120.0, 10.0);
        let account_prev = AccountBalanceBaseline {
            balance: 90.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };
        let topup_prev = TopupBaseline {
            total_credits: 100.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };

        let snap = build_snapshot_with_account_baseline(
            None,
            Some(&credit_data),
            Some(topup_prev),
            Some(account_prev),
            false,
        );
        let balance = snap
            .windows
            .iter()
            .find(|window| window.label.starts_with("balance "))
            .expect("account balance window");
        assert_eq!(balance.label, "balance $110.00");
        assert!(
            snap.level
                .as_deref()
                .unwrap_or("")
                .contains("Top-up detected"),
            "expected top-up note, got {:?}",
            snap.level
        );
        // New baseline is the current remaining, so nothing spent since top-up.
        assert!(balance.used_percent.abs() < f32::EPSILON);
    }

    #[test]
    fn topup_detection_fires_when_total_credits_rises() {
        // First fetch establishes baseline.
        let key = key_with_limit("monthly", 100.0, 5.0);
        let prev = TopupBaseline {
            total_credits: 50.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };
        let credit_data = credits(100.0, 5.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), Some(prev));
        assert!(
            snap.level
                .as_deref()
                .map(|l| l.contains("Top-up detected"))
                .unwrap_or(false),
            "expected 'Top-up detected' in level, got {:?}",
            snap.level
        );
        assert!(snap.level.as_deref().unwrap().contains("+$50.00"));
    }

    #[test]
    fn no_topup_note_when_total_credits_steady_or_falling() {
        let key = key_with_limit("monthly", 100.0, 5.0);
        let prev = TopupBaseline {
            total_credits: 100.0,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        };
        let credit_data = credits(100.0, 20.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), Some(prev));
        let level = snap.level.unwrap_or_default();
        assert!(
            !level.contains("Top-up"),
            "no top-up note when credits unchanged; level={level:?}"
        );
    }

    #[test]
    fn first_fetch_does_not_trigger_topup_note() {
        // No prior baseline — even if the user just topped up $100 before the
        // first ever fetch, we shouldn't flash "Top-up detected" because we
        // have nothing to compare against.
        let key = key_with_limit("monthly", 100.0, 0.0);
        let credit_data = credits(100.0, 0.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        let level = snap.level.unwrap_or_default();
        assert!(!level.contains("Top-up"), "level={level:?}");
    }

    #[test]
    fn credits_only_path_skips_bar_when_limit_reset_unknown() {
        // limit_reset is a new field that older API responses might omit;
        // absence → no bar segment, but balance popup still renders.
        let key = KeyData {
            label: None,
            is_free_tier: true,
            limit: Some(10.0),
            limit_remaining: None,
            limit_reset: None,
            usage: None,
            usage_daily: None,
            usage_weekly: None,
            usage_monthly: None,
        };
        let credit_data = credits(10.0, 2.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        let bar: Vec<&UsageWindow> = snap.windows.iter().filter(|w| w.bar_visible).collect();
        let popup: Vec<&UsageWindow> = snap
            .windows
            .iter()
            .filter(|w| w.label.starts_with("balance "))
            .collect();
        assert_eq!(bar.len(), 1, "balance is the only bar segment");
        assert_eq!(popup.len(), 1);
        assert!(snap.level.as_deref().unwrap_or("").contains("free tier"));
    }

    #[test]
    fn parse_key_response_shape_against_real_openrouter_field_names() {
        // Real OpenRouter /api/v1/key payload shape — guards against silent
        // field-name drift (e.g. `limit_reset` → `resetLimit`).
        let payload = r#"{
            "data": {
                "label": "production",
                "is_free_tier": false,
                "is_management_key": false,
                "limit": 50,
                "limit_remaining": 38.5,
                "limit_reset": "weekly",
                "usage": 11.5,
                "usage_daily": 0.42,
                "usage_weekly": 11.5,
                "usage_monthly": 11.5,
                "byok_usage": 0,
                "include_byok_in_limit": false,
                "expires_at": null
            }
        }"#;
        let parsed: KeyResponse = serde_json::from_str(payload).expect("parse");
        let d = parsed.data.expect("data");
        assert_eq!(d.label.as_deref(), Some("production"));
        assert_eq!(d.limit_reset.as_deref(), Some("weekly"));
        assert_eq!(d.limit, Some(50.0));
        assert_eq!(d.limit_remaining, Some(38.5));
        assert_eq!(d.usage, Some(11.5));
        assert_eq!(d.usage_weekly, Some(11.5));
    }

    #[test]
    fn parse_credits_response_shape_against_real_openrouter_field_names() {
        let payload = r#"{
            "data": {
                "total_credits": 25.5,
                "total_usage": 3.25
            }
        }"#;
        let parsed: CreditsResponse = serde_json::from_str(payload).expect("parse");
        let d = parsed.data.expect("data");
        assert_eq!(d.total_credits, Some(25.5));
        assert_eq!(d.total_usage, Some(3.25));
    }

    #[test]
    fn free_tier_label_surfaces_when_label_is_missing() {
        // A free-tier key with no user-supplied label should still produce
        // a non-empty level string so the popup isn't blank.
        let key = KeyData {
            label: None,
            is_free_tier: true,
            limit: Some(10.0),
            limit_remaining: Some(9.0),
            limit_reset: Some("weekly".into()),
            usage: Some(1.0),
            usage_daily: Some(0.0),
            usage_weekly: Some(1.0),
            usage_monthly: Some(1.0),
        };
        let credit_data = credits(10.0, 1.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        assert_eq!(
            snap.level.as_deref(),
            Some("free tier ($10.00 weekly limit)")
        );
    }

    #[test]
    fn labelled_paid_key_surfaces_label_and_limit() {
        let key = KeyData {
            label: Some("my-key".into()),
            is_free_tier: false,
            limit: Some(100.0),
            limit_remaining: Some(58.0),
            limit_reset: Some("monthly".into()),
            usage: Some(42.0),
            usage_daily: Some(0.0),
            usage_weekly: Some(0.0),
            usage_monthly: Some(42.0),
        };
        let credit_data = credits(500.0, 42.0);
        let snap = build_snapshot(Some(&key), Some(&credit_data), None);
        assert_eq!(
            snap.level.as_deref(),
            Some("my-key ($100.00 monthly limit)")
        );
    }

    #[test]
    fn management_only_returns_balance_window() {
        let credits = CreditsData {
            total_credits: Some(100.0),
            total_usage: Some(12.5),
        };
        let snap = build_snapshot(None, Some(&credits), None);

        assert_eq!(snap.windows.len(), 1);
        assert!(snap.windows[0].bar_visible);
        assert_eq!(snap.windows[0].label, "balance $87.50");
    }

    #[test]
    fn normal_key_and_management_balance_are_combined() {
        let key = key_with_limit("weekly", 100.0, 37.0);
        let credits = CreditsData {
            total_credits: Some(200.0),
            total_usage: Some(50.0),
        };
        let snap = build_snapshot(Some(&key), Some(&credits), None);

        assert_eq!(snap.windows.len(), 2);
        assert!(snap
            .windows
            .iter()
            .any(|w| w.label == "weekly" && w.bar_visible));
        assert!(snap
            .windows
            .iter()
            .any(|w| w.label == "balance $150.00" && w.bar_visible));
    }

    #[test]
    fn normal_key_without_limit_is_unusable_without_balance() {
        let mut key = key_with_limit("weekly", 100.0, 0.0);
        key.limit = None;
        key.limit_reset = None;
        let snap = build_snapshot(Some(&key), None, None);

        assert!(snap.windows.is_empty());
    }
}
