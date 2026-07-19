//! Live tray tooltip: a one-line summary of every provider's remaining
//! quota, rewritten whenever fresh snapshots land.

use std::collections::HashMap;

use crate::providers::{short_provider_name, short_window_label, UsageSnapshot, PROVIDER_IDS};

/// Windows silently drops tooltips longer than 127 chars, so over-long
/// summaries are compacted (first window per provider), then truncated.
const MAX_TOOLTIP_CHARS: usize = 127;

/// Build the tooltip text, e.g. `GLM 60% wk · 80% 5h | Codex 17% wk`.
///
/// Providers follow the canonical [`PROVIDER_IDS`] order; ids without a
/// snapshot are skipped. A snapshot with an `unavailable_reason` renders as
/// `NAME stale`; unlimited windows render as `∞`.
pub fn format_tray_tooltip(snaps: &HashMap<String, UsageSnapshot>) -> String {
    let full = build_tooltip(snaps, false);
    if full.chars().count() <= MAX_TOOLTIP_CHARS {
        return full;
    }
    // Over the Windows cap: retry with only the first window per provider.
    let compact = build_tooltip(snaps, true);
    if compact.chars().count() <= MAX_TOOLTIP_CHARS {
        return compact;
    }
    // Still over: char-safe truncate (never byte-slice — labels are UTF-8).
    let truncated: String = compact.chars().take(MAX_TOOLTIP_CHARS - 1).collect();
    format!("{truncated}…")
}

/// Rewrite the tray icon tooltip from the current snapshots. No-op until the
/// tray icon exists.
pub fn update_tray_tooltip(app: &tauri::AppHandle, snaps: &HashMap<String, UsageSnapshot>) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format_tray_tooltip(snaps)));
    }
}

fn build_tooltip(snaps: &HashMap<String, UsageSnapshot>, first_window_only: bool) -> String {
    let mut chunks: Vec<String> = Vec::new();
    for id in PROVIDER_IDS {
        let Some(snapshot) = snaps.get(*id) else {
            continue;
        };
        let name = short_provider_name(id);
        if snapshot.unavailable_reason.is_some() {
            chunks.push(format!("{name} stale"));
            continue;
        }
        let windows: Vec<String> = snapshot
            .windows
            .iter()
            .filter(|w| w.bar_visible)
            .take(if first_window_only { 1 } else { usize::MAX })
            .map(|w| {
                if w.is_unlimited {
                    "∞".to_string()
                } else {
                    let remaining = (100.0 - w.used_percent).round().clamp(0.0, 100.0) as i32;
                    format!("{remaining}% {}", short_window_label(&w.label))
                }
            })
            .collect();
        if windows.is_empty() {
            chunks.push(name);
        } else {
            let joined = windows.join(" · ");
            chunks.push(format!("{name} {joined}"));
        }
    }
    chunks.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UsageWindow;
    use chrono::Utc;

    fn win(label: &str, used_percent: f32) -> UsageWindow {
        UsageWindow {
            label: label.into(),
            used_percent,
            reset_at: None,
            bar_visible: true,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        }
    }

    fn snap(windows: Vec<UsageWindow>) -> UsageSnapshot {
        UsageSnapshot {
            provider: "Test".into(),
            level: None,
            windows,
            unavailable_reason: None,
            fetched_at: Utc::now(),
        }
    }

    fn snaps_of(entries: &[(&str, UsageSnapshot)]) -> HashMap<String, UsageSnapshot> {
        entries
            .iter()
            .map(|(id, snap)| (id.to_string(), snap.clone()))
            .collect()
    }

    #[test]
    fn orders_providers_by_canonical_order_and_joins_windows() {
        // Insertion order is the reverse of PROVIDER_IDS on purpose.
        let snaps = snaps_of(&[
            ("codex", snap(vec![win("weekly", 40.0)])),
            ("glm", snap(vec![win("weekly", 40.0), win("5h", 20.0)])),
        ]);
        assert_eq!(
            format_tray_tooltip(&snaps),
            "GLM 60% wk · 80% 5h | Codex 60% wk"
        );
    }

    #[test]
    fn renders_unlimited_windows_as_infinity() {
        let snaps = snaps_of(&[(
            "minimax",
            snap(vec![UsageWindow {
                is_unlimited: true,
                used_absolute: None,
                limit_absolute: None,
                ..win("weekly", 0.0)
            }]),
        )]);
        assert_eq!(format_tray_tooltip(&snaps), "MiniMax ∞");
    }

    #[test]
    fn marks_unavailable_providers_stale() {
        let mut snapshot = snap(vec![win("weekly", 40.0)]);
        snapshot.unavailable_reason = Some("network error".into());
        let snaps = snaps_of(&[("grok", snapshot)]);
        assert_eq!(format_tray_tooltip(&snaps), "Grok stale");
    }

    #[test]
    fn provider_without_visible_windows_renders_name_only() {
        let snaps = snaps_of(&[(
            "kimi",
            snap(vec![UsageWindow {
                bar_visible: false,
                ..win("monthly", 40.0)
            }]),
        )]);
        assert_eq!(format_tray_tooltip(&snaps), "Kimi");
    }

    #[test]
    fn ids_without_snapshots_are_skipped() {
        let snaps = snaps_of(&[("not-a-provider", snap(vec![win("weekly", 40.0)]))]);
        assert_eq!(format_tray_tooltip(&snaps), "");
    }

    #[test]
    fn falls_back_to_first_window_per_provider_when_over_the_cap() {
        let snaps: HashMap<String, UsageSnapshot> = PROVIDER_IDS
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    snap(vec![
                        win("weekly", 40.0),
                        win("5h", 40.0),
                        win("daily", 40.0),
                    ]),
                )
            })
            .collect();
        let tooltip = format_tray_tooltip(&snaps);
        assert_eq!(
            tooltip,
            "GLM 60% wk | MiniMax 60% wk | Codex 60% wk | Claude 60% wk | Grok 60% wk | Kimi 60% wk | OpenRouter 60% wk"
        );
        assert!(tooltip.chars().count() <= MAX_TOOLTIP_CHARS);
    }

    #[test]
    fn truncates_char_safely_when_still_over_the_cap() {
        // Long pass-through labels keep even the one-window form over 127.
        let snaps: HashMap<String, UsageSnapshot> = PROVIDER_IDS
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    snap(vec![win("super-long-custom-window-label", 40.0)]),
                )
            })
            .collect();
        let tooltip = format_tray_tooltip(&snaps);
        assert_eq!(tooltip.chars().count(), MAX_TOOLTIP_CHARS);
        assert!(tooltip.ends_with('…'));
        let compact = build_tooltip(&snaps, true);
        assert!(compact.starts_with(tooltip.trim_end_matches('…')));
    }
}
