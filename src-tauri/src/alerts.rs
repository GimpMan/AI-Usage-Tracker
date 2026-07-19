//! Quota-event detection between consecutive snapshots of one provider:
//! rate-limit window resets and edge-triggered threshold crossings (below the
//! even-pace red line, or exhausted). Detection is pure; dispatching emits the
//! frontend `quota-window-reset` event and fires OS notifications.

use chrono::{DateTime, Utc};
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

use crate::providers::{short_provider_name, UsageSnapshot, UsageWindow};
use crate::scheduler::USED_PERCENT_DROP_TOLERANCE;

/// Events detected between the previous and the incoming snapshot of one
/// provider. Both lists are empty for first-ever fetches and non-healthy
/// outcomes.
#[derive(Clone, Debug, Default)]
pub struct QuotaEvents {
    /// Labels of windows that reset: `reset_at` advanced and usage dropped.
    pub resets: Vec<String>,
    /// Edge-triggered threshold crossings.
    pub alerts: Vec<QuotaAlert>,
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

/// Diff two consecutive snapshots of one provider. `incoming` must be the
/// post-hold-last-good value so held readings don't raise phantom events.
pub fn detect_quota_events(
    prev: &UsageSnapshot,
    incoming: &UsageSnapshot,
    provider_id: &str,
) -> QuotaEvents {
    let mut events = QuotaEvents::default();
    for window in &incoming.windows {
        let Some(prev_window) = prev.windows.iter().find(|w| w.label == window.label) else {
            continue;
        };
        // Window reset: same label, advanced `reset_at`, and a usage drop
        // beyond the same tolerance `hold_last_good_used_percent` applies.
        if let (Some(prev_reset), Some(next_reset)) = (prev_window.reset_at, window.reset_at) {
            if prev_reset != next_reset
                && window.used_percent + USED_PERCENT_DROP_TOLERANCE < prev_window.used_percent
            {
                events.resets.push(window.label.clone());
            }
        }
        // Threshold alerts are limited to paced bar windows; OpenRouter is
        // exempt, mirroring the under-red-line guard in src/bar-summary.ts.
        if provider_id == "openrouter"
            || !window.bar_visible
            || window.is_unlimited
            || window.reset_at.is_none()
            || !(is_weekly_label(&window.label) || is_five_hour_label(&window.label))
        {
            continue;
        }
        let prev_remaining = (100.0 - prev_window.used_percent).clamp(0.0, 100.0);
        let next_remaining = (100.0 - window.used_percent).clamp(0.0, 100.0);
        if next_remaining <= 0.0 {
            if prev_remaining > 0.0 {
                events.alerts.push(QuotaAlert::Exhausted {
                    label: window.label.clone(),
                });
            }
            continue;
        }
        let next_below = below_red_line(window, incoming.fetched_at).unwrap_or(false);
        let prev_below = below_red_line(prev_window, prev.fetched_at).unwrap_or(false);
        if next_below && !prev_below {
            events.alerts.push(QuotaAlert::BelowRedLine {
                label: window.label.clone(),
            });
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

/// Emit `quota-window-reset` (fixed frontend contract) and fire one OS
/// notification per alert when notifications are enabled. Call after the
/// snapshot locks are released.
pub fn dispatch_quota_events(app: &tauri::AppHandle, provider_id: &str, events: &QuotaEvents) {
    if !events.resets.is_empty() {
        let _ = app.emit(
            "quota-window-reset",
            &serde_json::json!({ "provider": provider_id, "windows": events.resets }),
        );
    }
    if events.alerts.is_empty() || !crate::secrets::get_notifications_enabled() {
        return;
    }
    for alert in &events.alerts {
        let _ = app
            .notification()
            .builder()
            .title("AI Usage Tracker")
            .body(notification_body(provider_id, alert))
            .show();
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

    #[test]
    fn reset_detected_on_reset_at_change_with_usage_drop() {
        let now = Utc::now();
        let prev = snap_at(vec![win("weekly", 80.0, Some(now + Duration::days(2)))], now);
        let incoming = snap_at(
            vec![win("weekly", 5.0, Some(now + Duration::days(7)))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &incoming, "glm");
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
        assert!(detect_quota_events(&prev, &incoming, "glm").resets.is_empty());
        // New reset_at but drop within the 1.0 tolerance: not a reset.
        let incoming = snap_at(
            vec![win("weekly", 79.5, Some(now + Duration::days(7)))],
            now + Duration::minutes(1),
        );
        assert!(detect_quota_events(&prev, &incoming, "glm").resets.is_empty());
    }

    #[test]
    fn below_red_line_fires_only_on_the_crossing_edge() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        // Above the red line: 40% left with 3 days to go (below needs < ~32%).
        let prev_above = snap_at(vec![win("weekly", 60.0, Some(reset))], now);
        // Below the red line: 20% left with 3 days to go.
        let incoming_below = snap_at(
            vec![win("weekly", 80.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev_above, &incoming_below, "glm");
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::BelowRedLine {
                label: "weekly".into()
            }]
        );
        // Already below on both sides: no edge, no alert.
        let prev_below = snap_at(vec![win("weekly", 70.0, Some(reset))], now);
        let events = detect_quota_events(&prev_below, &incoming_below, "glm");
        assert!(events.alerts.is_empty());
        // Staying above: no alert either.
        let incoming_above = snap_at(
            vec![win("weekly", 65.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev_above, &incoming_above, "glm");
        assert!(events.alerts.is_empty());
    }

    #[test]
    fn exhausted_fires_on_the_zero_remaining_edge_only() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        let prev = snap_at(vec![win("weekly", 80.0, Some(reset))], now);
        let exhausted = snap_at(
            vec![win("weekly", 100.0, Some(reset))],
            now + Duration::minutes(1),
        );
        let events = detect_quota_events(&prev, &exhausted, "codex");
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::Exhausted {
                label: "weekly".into()
            }]
        );
        // Already exhausted: no re-fire.
        let events = detect_quota_events(&exhausted, &exhausted, "codex");
        assert!(events.alerts.is_empty());
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
        let events = detect_quota_events(&prev, &incoming, "minimax");
        assert_eq!(
            events.alerts,
            vec![QuotaAlert::BelowRedLine {
                label: "5h".into()
            }]
        );
    }

    #[test]
    fn skips_openrouter_unlimited_missing_reset_and_unclassified_labels() {
        let now = Utc::now();
        let reset = now + Duration::days(3);
        // Each pair crosses to 0% remaining, which would fire Exhausted if
        // the window were eligible.
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
        assert!(detect_quota_events(&prev, &incoming, "openrouter")
            .alerts
            .is_empty());
        // Unlimited windows never alert.
        let unlimited = UsageWindow {
            is_unlimited: true,
            used_absolute: None,
            limit_absolute: None,
            ..win("weekly", 0.0, Some(reset))
        };
        let (prev, incoming) = mk(unlimited);
        assert!(detect_quota_events(&prev, &incoming, "glm").alerts.is_empty());
        // No reset_at: no pace target, no alert.
        let (prev, incoming) = mk(win("weekly", 0.0, None));
        assert!(detect_quota_events(&prev, &incoming, "glm").alerts.is_empty());
        // daily / monthly labels don't classify as weekly/5h.
        let (prev, incoming) = mk(win("daily", 0.0, Some(reset)));
        assert!(detect_quota_events(&prev, &incoming, "glm").alerts.is_empty());
        let (prev, incoming) = mk(win("monthly", 0.0, Some(reset)));
        assert!(detect_quota_events(&prev, &incoming, "glm").alerts.is_empty());
        // Popup-only windows (bar_visible = false) stay silent.
        let hidden = UsageWindow {
            bar_visible: false,
            ..win("weekly", 0.0, Some(reset))
        };
        let (prev, incoming) = mk(hidden);
        assert!(detect_quota_events(&prev, &incoming, "glm").alerts.is_empty());
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
}
