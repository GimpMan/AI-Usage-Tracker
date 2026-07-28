//! Quota-event detection between consecutive snapshots of one provider:
//! rate-limit window resets and threshold alerts (below the even-pace red
//! line, or exhausted). Detection is pure; dispatching emits the frontend
//! `quota-window-reset` event, fires OS notifications, and reconciles the
//! persisted alert markers (`alert-state.json`) so each window period
//! notifies exactly once — even when the crossing happened while the app
//! was closed or before tracking started.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

use crate::providers::{short_provider_name, UsageSnapshot, UsageWindow};
use crate::scheduler::USED_PERCENT_DROP_TOLERANCE;

/// Events detected between the previous and the incoming snapshot of one
/// provider. All lists are empty for non-healthy outcomes.
#[derive(Clone, Debug, Default)]
pub struct QuotaEvents {
    /// Labels of windows that reset: `reset_at` advanced and usage dropped.
    pub resets: Vec<String>,
    /// Threshold alerts to notify: windows in an alert state whose marker
    /// key was not yet persisted.
    pub alerts: Vec<QuotaAlert>,
    /// Marker keys of every window currently in an alert state, whether or
    /// not it produced a notification. Dispatch reconciles the persisted
    /// marker set against this list.
    pub alert_keys: Vec<String>,
}

/// A threshold crossing on one rate-limit window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaAlert {
    /// Remaining quota crossed below the even-pace red line.
    BelowRedLine { label: String },
    /// Remaining quota reached 0%.
    Exhausted { label: String },
}

// Period lengths mirroring WEEK_DAYS / FIVE_HOUR_WINDOW_HOURS / DAY_MS /
// HOUR_MS in src/weekly-pace.ts.
const WEEK_DAYS: f64 = 7.0;
const FIVE_HOUR_WINDOW_HOURS: f64 = 5.0;
const DAY_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
const HOUR_MS: f64 = 60.0 * 60.0 * 1000.0;

/// Port of `isWeeklyWindow` (src/weekly-pace.ts): "weekly", "wk", "7d", or
/// labels starting with "7d" (e.g. "7d · resets Monday").
pub(crate) fn is_weekly_label(label: &str) -> bool {
    let normalized = label.trim().to_lowercase();
    normalized == "weekly" || normalized == "wk" || normalized.starts_with("7d")
}

/// Port of `isFiveHourWindow` (src/weekly-pace.ts): "5h" or labels starting
/// with "5h ·" (e.g. "5h · resets 14:00").
pub(crate) fn is_five_hour_label(label: &str) -> bool {
    let normalized = label.trim().to_lowercase();
    normalized == "5h" || normalized.starts_with("5h ·")
}

/// Whether the window is under its even-pace red line at `fetched_at` — the
/// same math the frontend applies in `calculateWeeklyPace` /
/// `calculateFiveHourPace` (dynamic sub-target, src/weekly-pace.ts). Returns
/// `None` when the window carries no `reset_at` or its label is neither
/// weekly nor 5h.
fn below_red_line(window: &UsageWindow, fetched_at: DateTime<Utc>) -> Option<bool> {
    if !window.used_percent.is_finite() {
        return None;
    }
    let reset_at = window.reset_at?;
    let (unit_ms, period) = if is_weekly_label(&window.label) {
        (DAY_MS, WEEK_DAYS)
    } else if is_five_hour_label(&window.label) {
        (HOUR_MS, FIVE_HOUR_WINDOW_HOURS)
    } else {
        return None;
    };
    let units_left = (reset_at - fetched_at).num_milliseconds() as f64 / unit_ms;
    if units_left <= 0.0 {
        // At/past reset there is no pace target — treated as not-below.
        return Some(false);
    }
    let remaining = (100.0 - window.used_percent as f64).clamp(0.0, 100.0);
    let target = (units_left / period * 100.0).clamp(0.0, 100.0);
    let sub_target = (target - remaining / units_left).clamp(0.0, 100.0);
    Some(remaining < sub_target)
}

/// Marker key identifying one alertable window period: the label plus the
/// current `reset_at`, so a window reset naturally re-arms its alerts.
fn alert_key(window: &UsageWindow) -> String {
    let reset = window
        .reset_at
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "none".into());
    format!("{}|{reset}", window.label)
}

/// Diff two consecutive snapshots of one provider. `incoming` must be the
/// post-hold-last-good value so held readings don't raise phantom events.
///
/// Alerts are state-based, not edge-triggered: any bar window currently
/// below its red pace line or exhausted produces an alert unless its marker
/// key is in `alerted` (already notified for this window period). This
/// catches crossings missed while the app was closed and windows already
/// red when tracking started. Exhaustion applies to every bar window —
/// including monthly and reset-at-less windows, which carry no pace
/// target — while the red-line check stays limited to weekly/5h windows
/// with a `reset_at`. OpenRouter is exempt, mirroring the under-red-line
/// guard in src/bar-summary.ts.
pub fn detect_quota_events(
    prev: &UsageSnapshot,
    incoming: &UsageSnapshot,
    provider_id: &str,
    alerted: &HashSet<String>,
) -> QuotaEvents {
    let mut events = QuotaEvents::default();
    for window in &incoming.windows {
        // Window reset: same label, advanced `reset_at`, and a usage drop
        // beyond the same tolerance `hold_last_good_used_percent` applies.
        if let Some(prev_window) = prev.windows.iter().find(|w| w.label == window.label) {
            if let (Some(prev_reset), Some(next_reset)) = (prev_window.reset_at, window.reset_at) {
                if prev_reset != next_reset
                    && window.used_percent + USED_PERCENT_DROP_TOLERANCE < prev_window.used_percent
                {
                    events.resets.push(window.label.clone());
                }
            }
        }
        // OpenRouter never alerts (mirrors the src/bar-summary.ts guard);
        // popup-only and unlimited windows stay silent.
        if provider_id == "openrouter" || !window.bar_visible || window.is_unlimited {
            continue;
        }
        let next_remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
        let alert = if next_remaining <= 0.0 {
            // Exhaustion needs no pace target and no `reset_at`.
            Some(QuotaAlert::Exhausted {
                label: window.label.clone(),
            })
        } else if window.reset_at.is_some()
            && (is_weekly_label(&window.label) || is_five_hour_label(&window.label))
            && below_red_line(window, incoming.fetched_at).unwrap_or(false)
        {
            Some(QuotaAlert::BelowRedLine {
                label: window.label.clone(),
            })
        } else {
            None
        };
        if let Some(alert) = alert {
            let key = alert_key(window);
            events.alert_keys.push(key.clone());
            if !alerted.contains(&key) {
                events.alerts.push(alert);
            }
        }
    }
    events
}

/// Notification body, e.g. `GLM weekly quota crossed below its red line` /
/// `Codex 5h quota exhausted` (short provider name + raw window label).
pub fn notification_body(provider_id: &str, alert: &QuotaAlert) -> String {
    let name = short_provider_name(provider_id);
    match alert {
        QuotaAlert::BelowRedLine { label } => {
            format!("{name} {label} quota crossed below its red line")
        }
        QuotaAlert::Exhausted { label } => format!("{name} {label} quota exhausted"),
    }
}

/// Path of the alert-marker store, next to `config.json` / `state.json`.
fn alerts_state_path() -> Option<PathBuf> {
    crate::secrets::config_dir()
        .ok()
        .map(|d| d.join("alert-state.json"))
}

/// Load the marker keys already notified for `provider_id`.
pub fn load_alerted(provider_id: &str) -> HashSet<String> {
    let Some(path) = alerts_state_path() else {
        return HashSet::new();
    };
    load_alerted_from(&path, provider_id)
}

fn load_alerted_from(path: &Path, provider_id: &str) -> HashSet<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<HashMap<String, HashSet<String>>>(&text).ok())
        .and_then(|mut map| map.remove(provider_id))
        .unwrap_or_default()
}

/// Replace the persisted marker set for `provider_id`. An empty set removes
/// the provider entry, so recovered windows re-arm their alerts.
pub fn save_alerted(provider_id: &str, keys: &HashSet<String>) {
    let Some(path) = alerts_state_path() else {
        return;
    };
    save_alerted_to(&path, provider_id, keys);
}

fn save_alerted_to(path: &Path, provider_id: &str, keys: &HashSet<String>) {
    let mut map: HashMap<String, HashSet<String>> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    if keys.is_empty() {
        map.remove(provider_id);
    } else {
        map.insert(provider_id.to_string(), keys.clone());
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, text);
    }
}

/// Emit `quota-window-reset` (fixed frontend contract), fire one OS
/// notification per alert when notifications are enabled, and reconcile the
/// persisted alert markers against the windows currently in an alert state.
/// When notifications are disabled no new markers are written, so enabling
/// the toggle later still notifies for windows that are red at that moment.
/// Call after the snapshot locks are released.
pub fn dispatch_quota_events(app: &tauri::AppHandle, provider_id: &str, events: &QuotaEvents) {
    if !events.resets.is_empty() {
        let _ = app.emit(
            "quota-window-reset",
            &serde_json::json!({ "provider": provider_id, "windows": events.resets }),
        );
    }
    let current: HashSet<String> = events.alert_keys.iter().cloned().collect();
    if !events.alerts.is_empty() && crate::secrets::get_notifications_enabled() {
        for alert in &events.alerts {
            let _ = app
                .notification()
                .builder()
                .title("AI Usage Tracker")
                .body(notification_body(provider_id, alert))
                .show();
        }
        save_alerted(provider_id, &current);
    } else {
        // Keep markers only for windows still in an alert state; a window
        // that recovered (or reset) re-arms its alerts.
        let kept: HashSet<String> = load_alerted(provider_id)
            .into_iter()
            .filter(|k| current.contains(k))
            .collect();
        save_alerted(provider_id, &kept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn win(label: &str, used_percent: f32, reset_at: Option<DateTime<Utc>>) -> UsageWindow {
        UsageWindow {
            label: label.into(),
            used_percent,
            reset_at,
            bar_visible: true,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        }
    }

    fn snap_at(windows: Vec<UsageWindow>, fetched_at: DateTime<Utc>) -> UsageSnapshot {
        UsageSnapshot {
            provider: "Test".into(),
            level: None,
            windows,
            unavailable_reason: None,
            fetched_at,
        }
    }

    fn no_alerts() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn reset_detected_on_reset_at_change_with_usage_drop() {
        let now = Utc::now();
        let prev = snap_at(vec![win("weekly", 80.0, Some(now + Duration::days(2)))], now);
        let incoming = snap_at(
            vec![win("weekly", 5.0, Some(now + Duration::days(7)))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &incoming, "glm", &no_alerts());
        assert_eq!(events.resets, vec!["weekly".to_string()]);
        assert!(events.alerts.is_empty());
    }

    #[test]
    fn no_reset_when_reset_at_unchanged_or_drop_within_tolerance() {
        let now = Utc::now();
        let reset = now + Duration::days(2);
        let prev = snap_at(vec![win("weekly", 80.0, Some(reset))], now);
        // Same reset_at: a drop is a bad reading, not a reset.
        let incoming = snap_at(
            vec![win("weekly", 5.0, Some(reset))],
            now + Duration::minutes(1),
        );
        assert!(
            detect_quota_events(&prev, &incoming, "glm", &no_alerts())
                .resets
                .is_empty()
        );
        // New reset_at but drop within the 1.0 tolerance: not a reset.
        let incoming = snap_at(
            vec![win("weekly", 79.5, Some(now + Duration::days(7)))],
            now + Duration::minutes(1),
        );
        assert!(
            detect_quota_events(&prev, &incoming, "glm", &no_alerts())
                .resets
                .is_empty()
        );
    }

    #[test]
    fn below_red_line_fires_on_crossing_and_when_already_below() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        // Above the red line: 40% left with 3 days to go (below needs < ~32%).
        let prev_above = snap_at(vec![win("weekly", 60.0, Some(reset))], now);
        // Below the red line: 20% left with 3 days to go.
        let incoming_below = snap_at(
            vec![win("weekly", 80.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev_above, &incoming_below, "glm", &no_alerts());
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::BelowRedLine {
                label: "weekly".into()
            }]
        );
        assert_eq!(events.alert_keys.len(), 1);
        // Already below on both sides still fires when never notified — the
        // crossing may have happened while the app was closed.
        let prev_below = snap_at(vec![win("weekly", 70.0, Some(reset))], now);
        let events = detect_quota_events(&prev_below, &incoming_below, "glm", &no_alerts());
        assert_eq!(events.alerts.len(), 1);
        // …but not when its marker was already persisted.
        let alerted: HashSet<String> = events.alert_keys.iter().cloned().collect();
        let events = detect_quota_events(&prev_below, &incoming_below, "glm", &alerted);
        assert!(events.alerts.is_empty());
        assert_eq!(events.alert_keys.len(), 1);
        // Staying above: no alert, no marker.
        let incoming_above = snap_at(
            vec![win("weekly", 65.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev_above, &incoming_above, "glm", &no_alerts());
        assert!(events.alerts.is_empty());
        assert!(events.alert_keys.is_empty());
    }

    #[test]
    fn exhausted_fires_until_marked() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        let prev = snap_at(vec![win("weekly", 80.0, Some(reset))], now);
        let exhausted = snap_at(
            vec![win("weekly", 100.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &exhausted, "codex", &no_alerts());
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::Exhausted {
                label: "weekly".into()
            }]
        );
        // Already exhausted on both sides: fires once when never notified,
        // then stays silent once the marker exists.
        let events = detect_quota_events(&exhausted, &exhausted, "codex", &no_alerts());
        assert_eq!(events.alerts.len(), 1);
        let alerted: HashSet<String> = events.alert_keys.iter().cloned().collect();
        let events = detect_quota_events(&exhausted, &exhausted, "codex", &alerted);
        assert!(events.alerts.is_empty());
    }

    #[test]
    fn exhausted_fires_without_reset_at_and_for_monthly_labels() {
        let now = Utc::now();
        // GLM-style windows carry no reset_at; running out must still alert.
        let prev = snap_at(vec![win("weekly", 80.0, None)], now);
        let exhausted = snap_at(vec![win("weekly", 100.0, None)], now + Duration::minutes(1));
        let events = detect_quota_events(&prev, &exhausted, "glm", &no_alerts());
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::Exhausted {
                label: "weekly".into()
            }]
        );
        // Monthly windows have no pace target, but hitting 0% left alerts.
        let prev = snap_at(vec![win("monthly", 74.0, Some(now + Duration::days(4)))], now);
        let exhausted = snap_at(
            vec![win("monthly", 100.0, Some(now + Duration::days(4)))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &exhausted, "grok", &no_alerts());
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::Exhausted {
                label: "monthly".into()
            }]
        );
        // Monthly windows never get a red-line alert (no pace math for them).
        let below = snap_at(
            vec![win("monthly", 95.0, Some(now + Duration::days(1)))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &below, "grok", &no_alerts());
        assert!(events.alerts.is_empty());
        assert!(events.alert_keys.is_empty());
    }

    #[test]
    fn five_hour_windows_use_a_five_hour_period() {
        let now = Utc::now();
        let reset = now + Duration::hours(2);
        // 2h left of 5h: target = 40, sub-target = 40 - remaining/2.
        // remaining 30 → sub 25 → above; remaining 10 → sub ~35 → below.
        let prev = snap_at(vec![win("5h", 70.0, Some(reset))], now);
        let incoming = snap_at(
            vec![win("5h", 90.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &incoming, "minimax", &no_alerts());
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::BelowRedLine {
                label: "5h".into()
            }]
        );
    }

    #[test]
    fn skips_openrouter_unlimited_and_hidden_windows() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        // Each pair sits at 0% remaining, which would fire Exhausted if the
        // window were eligible.
        let mk = |window: UsageWindow| {
            (
                snap_at(
                    vec![UsageWindow {
                        used_percent: 60.0,
                        ..window.clone()
                    }],
                    now,
                ),
                snap_at(
                    vec![UsageWindow {
                        used_percent: 100.0,
                        ..window
                    }],
                    now + Duration::minutes(1),
                ),
            )
        };
        // OpenRouter never alerts (mirrors the src/bar-summary.ts guard).
        let (prev, incoming) = mk(win("weekly", 0.0, Some(reset)));
        assert!(
            detect_quota_events(&prev, &incoming, "openrouter", &no_alerts())
                .alerts
                .is_empty()
        );
        // Unlimited windows never alert.
        let unlimited = UsageWindow {
            is_unlimited: true,
            used_absolute: None,
            limit_absolute: None,
            ..win("weekly", 0.0, Some(reset))
        };
        let (prev, incoming) = mk(unlimited);
        assert!(
            detect_quota_events(&prev, &incoming, "glm", &no_alerts())
                .alerts
                .is_empty()
        );
        // Popup-only windows (bar_visible = false) stay silent.
        let hidden = UsageWindow {
            bar_visible: false,
            ..win("weekly", 0.0, Some(reset))
        };
        let (prev, incoming) = mk(hidden);
        assert!(
            detect_quota_events(&prev, &incoming, "glm", &no_alerts())
                .alerts
                .is_empty()
        );
    }

    #[test]
    fn no_reset_at_means_no_red_line_alert() {
        let now = Utc::now();
        // No reset_at: no pace target, so BelowRedLine can never fire — but
        // the window stays eligible for Exhausted (covered above).
        let prev = snap_at(vec![win("weekly", 80.0, None)], now);
        let incoming = snap_at(vec![win("weekly", 95.0, None)], now + Duration::minutes(1));
        let events = detect_quota_events(&prev, &incoming, "glm", &no_alerts());
        assert!(events.alerts.is_empty());
        assert!(events.alert_keys.is_empty());
    }

    #[test]
    fn label_classification_matches_the_frontend() {
        assert!(is_weekly_label("weekly"));
        assert!(is_weekly_label("WK"));
        assert!(is_weekly_label("7d"));
        assert!(is_weekly_label("7d · resets Monday"));
        assert!(!is_weekly_label("5h"));
        assert!(!is_weekly_label("daily"));
        assert!(is_five_hour_label("5h"));
        assert!(is_five_hour_label("5h · resets 14:00"));
        assert!(!is_five_hour_label("5h-ish"));
        assert!(!is_five_hour_label("weekly"));
    }

    #[test]
    fn notification_body_uses_short_provider_name_and_raw_label() {
        assert_eq!(
            notification_body(
                "glm",
                &QuotaAlert::BelowRedLine {
                    label: "weekly".into()
                }
            ),
            "GLM weekly quota crossed below its red line"
        );
        assert_eq!(
            notification_body(
                "codex",
                &QuotaAlert::Exhausted {
                    label: "5h".into()
                }
            ),
            "Codex 5h quota exhausted"
        );
    }

    #[test]
    fn alert_state_roundtrips_and_clears_per_provider() {
        let dir = std::env::temp_dir().join(format!("ai-usage-alerts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("alert-state.json");

        // Missing file → empty set.
        assert!(load_alerted_from(&path, "glm").is_empty());

        let keys: HashSet<String> = ["weekly|none".to_string()].into_iter().collect();
        save_alerted_to(&path, "glm", &keys);
        assert_eq!(load_alerted_from(&path, "glm"), keys);
        // Other providers are untouched.
        assert!(load_alerted_from(&path, "codex").is_empty());

        // Saving for one provider preserves another's markers.
        let other: HashSet<String> = ["wk|2026-08-04T05:54:18+00:00".to_string()]
            .into_iter()
            .collect();
        save_alerted_to(&path, "codex", &other);
        assert_eq!(load_alerted_from(&path, "glm"), keys);
        assert_eq!(load_alerted_from(&path, "codex"), other);

        // Empty set removes the provider entry.
        save_alerted_to(&path, "glm", &HashSet::new());
        assert!(load_alerted_from(&path, "glm").is_empty());
        assert_eq!(load_alerted_from(&path, "codex"), other);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
