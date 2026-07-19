//! Burn history: one point per qualifying window per healthy fetch, kept for
//! 7 days, and the bucketed per-bucket burn series the popup renders as bars.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::alerts::{is_five_hour_label, is_weekly_label};
use crate::providers::{UsageSnapshot, PROVIDER_IDS};

/// One recorded reading of a window's used-percent (post last-good hold).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub ts: i64,
    pub used_percent: f32,
}

/// provider id -> window label -> points, oldest first.
pub type HistoryMap = Arc<RwLock<HashMap<String, HashMap<String, Vec<HistoryPoint>>>>>;

const RETENTION_SECS: i64 = 7 * 24 * 3600; // 7 days
const MAX_POINTS_PER_WINDOW: usize = 30_000; // safety valve
pub const BUCKET_COUNT: usize = 60;

#[derive(Clone, Debug, Serialize)]
pub struct BurnBucket {
    pub t: i64,
    pub burn: f32,
    pub reset: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct WindowBurn {
    pub label: String,
    pub buckets: Vec<BurnBucket>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderBurnHistory {
    pub id: String,
    pub provider: String,
    pub windows: Vec<WindowBurn>,
}

/// Append the snapshot's qualifying windows to the buffer. `snapshot` must be
/// the post-hold value of a Healthy fetch. Skips a point whose ts equals the
/// last stored ts (double-apply guard), and points older than retention.
pub fn record_snapshot(
    buffer: &mut HashMap<String, HashMap<String, Vec<HistoryPoint>>>,
    provider_id: &str,
    snapshot: &UsageSnapshot,
) {
    let cutoff = snapshot.fetched_at.timestamp() - RETENTION_SECS;
    for w in &snapshot.windows {
        if !w.bar_visible || w.is_unlimited {
            continue;
        }
        if !is_weekly_label(&w.label) && !is_five_hour_label(&w.label) {
            continue;
        }
        if !w.used_percent.is_finite() {
            continue;
        }
        let points = buffer
            .entry(provider_id.to_string())
            .or_default()
            .entry(w.label.clone())
            .or_default();
        let ts = snapshot.fetched_at.timestamp();
        if points.last().is_some_and(|p| p.ts >= ts) {
            continue; // same tick / out-of-order
        }
        points.push(HistoryPoint {
            ts,
            used_percent: w.used_percent,
        });
        points.retain(|p| p.ts >= cutoff);
        if points.len() > MAX_POINTS_PER_WINDOW {
            let drop = points.len() - MAX_POINTS_PER_WINDOW;
            points.drain(0..drop);
        }
    }
    // Prune empty provider / label maps so a recovered empty window doesn't
    // leave a tombstone behind that confuses `burn_history`.
    buffer.retain(|_, labels| {
        labels.retain(|_, points| !points.is_empty());
        !labels.is_empty()
    });
}

fn history_path() -> Result<std::path::PathBuf, String> {
    crate::secrets::config_dir()
        .map(|d| d.join("history.json"))
        .map_err(|e| e.to_string())
}

/// Rehydrate the in-memory buffer from `history.json`. Missing or corrupt
/// files leave the buffer empty (the next Healthy fetch will start it
/// fresh). Prunes anything older than the 7-day retention window while
/// loading.
pub async fn load_history(map: &HistoryMap) {
    let parsed: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = match history_path() {
        Ok(path) => tokio::fs::read_to_string(path)
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    };
    let now = Utc::now().timestamp();
    let cutoff = now - RETENTION_SECS;
    let mut pruned: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
    for (provider_id, labels) in parsed {
        let mut kept_labels: HashMap<String, Vec<HistoryPoint>> = HashMap::new();
        for (label, points) in labels {
            let trimmed: Vec<HistoryPoint> =
                points.into_iter().filter(|p| p.ts >= cutoff).collect();
            if !trimmed.is_empty() {
                kept_labels.insert(label, trimmed);
            }
        }
        if !kept_labels.is_empty() {
            pruned.insert(provider_id, kept_labels);
        }
    }
    let mut guard = map.write().await;
    *guard = pruned;
}

/// Persist the buffer to `history.json`. Compact JSON (no pretty printing) —
/// at 30k points per window and 7 windows, this file is on the megabyte
/// order. Errors are swallowed because history is non-critical.
pub async fn save_history(map: &HistoryMap) {
    let Ok(path) = history_path() else {
        return;
    };
    let snapshot: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = {
        let guard = map.read().await;
        guard.clone()
    };
    let Ok(text) = serde_json::to_string(&snapshot) else {
        return;
    };
    let _ = tokio::fs::write(path, text).await;
}

/// Bin `points` into BUCKET_COUNT buckets on a FIXED grid anchored to the Unix
/// epoch (NOT to `now`). Bucket i covers
/// `[i*bucket_secs, (i+1)*bucket_secs)`. The 60 buckets displayed are the ones
/// spanning `[now_bucket-59, now_bucket]` where
/// `now_bucket = floor(now_ts / bucket_secs)`.
///
/// Why fixed-grid (not `now`-anchored): with `now`-anchored buckets the grid
/// slid a little on every refresh, so a delta (a pair of consecutive points)
/// near a boundary flipped between buckets as `now` advanced — making the
/// rendered values drift across the whole chart between polls. On a fixed
/// grid, a point always lands in the same historical bucket; the chart only
/// changes when (a) a new delta lands in the still-filling last bucket, or
/// (b) `now` crosses into the next bucket index and every bar shifts left by
/// one (the intended "scroll" behavior).
///
/// Bucket burn = sum of positive consecutive deltas whose later point falls
/// in that bucket. A negative delta means the window reset: flag that bucket
/// `reset`, add 0.
pub fn burn_buckets(
    points: &[HistoryPoint],
    period_secs: i64,
    now: DateTime<Utc>,
) -> Vec<BurnBucket> {
    let now_ts = now.timestamp();
    let bucket_secs = period_secs / BUCKET_COUNT as i64;
    // Absolute bucket index of `now` and of the first visible bucket. Both are
    // stable as `now` moves within a single bucket; they advance by exactly 1
    // when `now` crosses a bucket boundary, scrolling the chart one step left.
    let now_bucket = now_ts.div_euclid(bucket_secs);
    let first_bucket = now_bucket - (BUCKET_COUNT as i64 - 1);
    let mut buckets: Vec<BurnBucket> = (0..BUCKET_COUNT)
        .map(|i| BurnBucket {
            t: (first_bucket + i as i64) * bucket_secs,
            burn: 0.0,
            reset: false,
        })
        .collect();
    let mut sorted: Vec<&HistoryPoint> = points.iter().collect();
    sorted.sort_by_key(|p| p.ts);
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        // Absolute, `now`-invariant bucket index of the later point.
        let bucket_idx = b.ts.div_euclid(bucket_secs);
        if bucket_idx < first_bucket || bucket_idx > now_bucket {
            continue;
        }
        let idx = (bucket_idx - first_bucket) as usize;
        let delta = b.used_percent - a.used_percent;
        if delta < 0.0 {
            buckets[idx].reset = true;
        } else {
            buckets[idx].burn += delta;
        }
    }
    buckets
}

/// One entry per provider with a live snapshot, keyed by provider id; windows
/// follow the snapshot's own order. Weekly gets a 7d range, 5h a 5h range.
pub async fn burn_history(
    snaps: &crate::scheduler::SnapshotMap,
    history: &HistoryMap,
) -> Vec<ProviderBurnHistory> {
    let now = Utc::now();
    let snaps = snaps.read().await;
    let history = history.read().await;
    let mut out = Vec::new();
    for id in PROVIDER_IDS {
        let Some(snap) = snaps.get(*id) else {
            continue;
        };
        let windows = snap
            .windows
            .iter()
            .filter(|w| w.bar_visible && !w.is_unlimited)
            .filter(|w| is_weekly_label(&w.label) || is_five_hour_label(&w.label))
            .map(|w| {
                let period = if is_weekly_label(&w.label) {
                    7 * 24 * 3600
                } else {
                    5 * 3600
                };
                let empty = Vec::new();
                let points = history
                    .get(*id)
                    .and_then(|m| m.get(&w.label))
                    .unwrap_or(&empty);
                WindowBurn {
                    label: w.label.clone(),
                    buckets: burn_buckets(points, period, now),
                }
            })
            .collect();
        out.push(ProviderBurnHistory {
            id: id.to_string(),
            provider: snap.provider.clone(),
            windows,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UsageWindow;
    use chrono::{Duration, TimeZone};

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

    fn unlimited(label: &str, used_percent: f32) -> UsageWindow {
        UsageWindow {
            label: label.into(),
            used_percent,
            reset_at: None,
            bar_visible: true,
            is_unlimited: true,
            used_absolute: None,
            limit_absolute: None,
        }
    }

    fn hidden(label: &str, used_percent: f32) -> UsageWindow {
        UsageWindow {
            label: label.into(),
            used_percent,
            reset_at: None,
            bar_visible: false,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        }
    }

    fn snap_at(
        provider: &str,
        windows: Vec<UsageWindow>,
        fetched_at: DateTime<Utc>,
    ) -> UsageSnapshot {
        UsageSnapshot {
            provider: provider.into(),
            level: None,
            windows,
            unavailable_reason: None,
            fetched_at,
        }
    }

    // ----- record_snapshot -----

    #[test]
    fn record_snapshot_appends_qualifying_windows_in_order() {
        let mut buffer: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
        let base = Utc.with_ymd_and_hms(2025, 7, 1, 0, 0, 0).unwrap();
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at("Z.ai", vec![win("weekly", 10.0)], base),
        );
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at(
                "Z.ai",
                vec![win("weekly", 20.0)],
                base + Duration::minutes(1),
            ),
        );
        let pts = &buffer["glm"]["weekly"];
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].used_percent, 10.0);
        assert_eq!(pts[1].used_percent, 20.0);
    }

    #[test]
    fn record_snapshot_skips_duplicate_or_out_of_order_timestamps() {
        let mut buffer: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
        let t = Utc.with_ymd_and_hms(2025, 7, 1, 0, 0, 0).unwrap();
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at("Z.ai", vec![win("weekly", 10.0)], t),
        );
        // Same tick: ignored.
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at("Z.ai", vec![win("weekly", 11.0)], t),
        );
        assert_eq!(buffer["glm"]["weekly"].len(), 1);
        assert_eq!(buffer["glm"]["weekly"][0].used_percent, 10.0);
        // Older tick: also ignored.
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at("Z.ai", vec![win("weekly", 9.0)], t - Duration::seconds(1)),
        );
        assert_eq!(buffer["glm"]["weekly"].len(), 1);
    }

    #[test]
    fn record_snapshot_skips_unlimited_hidden_and_unclassified_windows() {
        let mut buffer: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 1, 0, 0, 0).unwrap();
        record_snapshot(
            &mut buffer,
            "minimax",
            &snap_at(
                "MiniMax",
                vec![
                    unlimited("weekly", 50.0),
                    hidden("weekly", 50.0),
                    win("daily", 50.0),
                    win("monthly", 50.0),
                    win("5h", 30.0),
                ],
                now,
            ),
        );
        assert!(!buffer.contains_key("minimax") || !buffer["minimax"].contains_key("daily"));
        assert!(!buffer.contains_key("minimax") || !buffer["minimax"].contains_key("monthly"));
        // Only the qualifying 5h window survives.
        assert_eq!(buffer["minimax"]["5h"].len(), 1);
    }

    #[test]
    fn record_snapshot_prunes_points_older_than_retention() {
        let mut buffer: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
        let now = Utc.with_ymd_and_hms(2025, 7, 10, 0, 0, 0).unwrap();
        // Seed a point well outside the 7-day window.
        buffer.insert(
            "glm".into(),
            HashMap::from([(
                "weekly".into(),
                vec![HistoryPoint {
                    ts: now.timestamp() - 8 * 24 * 3600,
                    used_percent: 99.0,
                }],
            )]),
        );
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at("Z.ai", vec![win("weekly", 50.0)], now),
        );
        let pts = &buffer["glm"]["weekly"];
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].used_percent, 50.0);
    }

    #[test]
    fn record_snapshot_caps_total_points_per_window() {
        let mut buffer: HashMap<String, HashMap<String, Vec<HistoryPoint>>> = HashMap::new();
        // Pre-load the cap.
        let base = 1_000_000_i64;
        let mut pre: Vec<HistoryPoint> = Vec::new();
        for i in 0..MAX_POINTS_PER_WINDOW + 50 {
            pre.push(HistoryPoint {
                ts: base + i as i64,
                used_percent: i as f32,
            });
        }
        buffer.insert("glm".into(), HashMap::from([("weekly".into(), pre)]));
        // Append one more inside retention.
        let next_ts = base + (MAX_POINTS_PER_WINDOW + 50) as i64 + 1;
        record_snapshot(
            &mut buffer,
            "glm",
            &snap_at(
                "Z.ai",
                vec![win("weekly", 99.9)],
                Utc.timestamp_opt(next_ts, 0).unwrap(),
            ),
        );
        let pts = &buffer["glm"]["weekly"];
        assert_eq!(pts.len(), MAX_POINTS_PER_WINDOW);
        // First kept point is the original oldest-survivor (drained).
        assert_eq!(pts[0].ts, base + 51);
        assert_eq!(pts.last().unwrap().used_percent, 99.9);
    }

    // ----- burn_buckets -----

    fn p(ts: i64, used_percent: f32) -> HistoryPoint {
        HistoryPoint { ts, used_percent }
    }

    #[test]
    fn burn_buckets_returns_sixty_evenly_spaced_empty_buckets_for_empty_input() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let period = 7 * 24 * 3600;
        let bucket_secs = period / BUCKET_COUNT as i64;
        let buckets = burn_buckets(&[], period, now);
        assert_eq!(buckets.len(), BUCKET_COUNT);
        // Fixed grid: bucket i left edge = (now_bucket - 59 + i) * bucket_secs.
        let now_bucket = now.timestamp().div_euclid(bucket_secs);
        let first_bucket = now_bucket - (BUCKET_COUNT as i64 - 1);
        for (i, b) in buckets.iter().enumerate() {
            assert_eq!(b.t, (first_bucket + i as i64) * bucket_secs);
            assert_eq!(b.burn, 0.0);
            assert!(!b.reset);
        }
    }

    #[test]
    fn burn_buckets_sums_positive_deltas_into_their_bucket() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let period: i64 = 3600; // 1h -> 60s buckets, easy math
        let bucket_secs = period / BUCKET_COUNT as i64; // 60s
        // Two consecutive points inside one bucket: +0.5 burn lands there.
        // Place both points well inside a single bucket so the absolute
        // bucket index is unambiguous.
        let now_bucket = now.timestamp().div_euclid(bucket_secs);
        let target_bucket = now_bucket - 5; // a few buckets back from `now`
        let bucket_left = target_bucket * bucket_secs;
        let t0 = bucket_left + 10;
        let t1 = t0 + 5;
        let t2 = t0 + 10;
        let points = vec![p(t0, 10.0), p(t1, 10.5), p(t2, 11.0)];
        let buckets = burn_buckets(&points, period, now);
        let idx = (target_bucket - (now_bucket - (BUCKET_COUNT as i64 - 1))) as usize;
        // Two deltas, each +0.5, both later-points in the same bucket.
        assert!((buckets[idx].burn - 1.0).abs() < 1e-4, "got {}", buckets[idx].burn);
        for (i, b) in buckets.iter().enumerate() {
            if i != idx {
                assert_eq!(b.burn, 0.0, "bucket {i} should be empty");
                assert!(!b.reset);
            }
        }
    }

    #[test]
    fn burn_buckets_flags_reset_on_negative_delta_and_adds_no_burn() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let period: i64 = 3600;
        let bucket_secs = period / BUCKET_COUNT as i64;
        let now_bucket = now.timestamp().div_euclid(bucket_secs);
        let target_bucket = now_bucket - 3;
        let bucket_left = target_bucket * bucket_secs;
        // Pre-reset at 80%, post-reset at 5% — both in the same bucket.
        let points = vec![p(bucket_left + 5, 80.0), p(bucket_left + 10, 5.0)];
        let buckets = burn_buckets(&points, period, now);
        let idx = (target_bucket - (now_bucket - (BUCKET_COUNT as i64 - 1))) as usize;
        assert!(buckets[idx].reset, "reset bucket must be flagged");
        assert_eq!(buckets[idx].burn, 0.0);
    }

    #[test]
    fn burn_buckets_ignores_deltas_whose_later_point_is_outside_the_window() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let period: i64 = 3600;
        let bucket_secs = period / BUCKET_COUNT as i64;
        let now_bucket = now.timestamp().div_euclid(bucket_secs);
        // `b` is in a bucket AFTER now_bucket → outside the visible window.
        let a_ts = now.timestamp() - 5;
        let b_ts = (now_bucket + 5) * bucket_secs + 3;
        let points = vec![p(a_ts, 10.0), p(b_ts, 99.0)];
        let buckets = burn_buckets(&points, period, now);
        // No bucket should carry burn from this pair.
        for b in &buckets {
            assert_eq!(b.burn, 0.0);
            assert!(!b.reset);
        }
    }

    #[test]
    fn burn_buckets_seeds_first_in_range_delta_from_a_point_before_the_window() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let period: i64 = 3600;
        let bucket_secs = period / BUCKET_COUNT as i64;
        let now_bucket = now.timestamp().div_euclid(bucket_secs);
        let first_visible = now_bucket - (BUCKET_COUNT as i64 - 1);
        let first_left = first_visible * bucket_secs;
        // `a` is in the bucket just before the first visible one; `b` lands at
        // the very start of the first visible bucket. The +3 delta must be
        // attributed to the first visible bucket (delta follows b).
        let a_ts = first_left - 3;
        let b_ts = first_left + 2;
        let points = vec![p(a_ts, 10.0), p(b_ts, 13.0)];
        let buckets = burn_buckets(&points, period, now);
        assert!((buckets[0].burn - 3.0).abs() < 1e-4, "got {}", buckets[0].burn);
    }

    /// Stability contract (the bug this guards against): re-sampling `now`
    /// WITHOUT crossing a bucket boundary must leave every historical bucket
    /// byte-identical. Previously, `now`-anchored bucket boundaries slid a
    /// little on every refresh, re-attributing deltas near boundaries and
    /// making the rendered values drift between polls even when no new fetch
    /// happened.
    #[test]
    fn burn_buckets_is_stable_when_now_advances_within_a_bucket() {
        let period: i64 = 5 * 3600; // 5h window, 300s buckets
        let bucket_secs = period / BUCKET_COUNT as i64;
        // Build ~30 points spanning the window so several buckets are filled.
        let base_now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let now_bucket = base_now.timestamp().div_euclid(bucket_secs);
        let first_visible = now_bucket - (BUCKET_COUNT as i64 - 1);
        let mut points = Vec::new();
        let mut value = 10.0_f32;
        for i in 0..30 {
            // Land each point a few seconds into a distinct bucket.
            let ts = (first_visible + i) * bucket_secs + 7;
            points.push(p(ts, value));
            value += 0.7;
        }
        // Sample `now` at three offsets strictly inside the SAME current bucket.
        // bucket_secs is 300; offsets 50/120/250 are all < 300 and share now_bucket.
        let a = burn_buckets(&points, period, DateTime::from_timestamp(base_now.timestamp() + 50, 0).unwrap());
        let b = burn_buckets(&points, period, DateTime::from_timestamp(base_now.timestamp() + 120, 0).unwrap());
        let c = burn_buckets(&points, period, DateTime::from_timestamp(base_now.timestamp() + 250, 0).unwrap());
        assert_eq!(a.len(), BUCKET_COUNT);
        assert_eq!(b.len(), BUCKET_COUNT);
        assert_eq!(c.len(), BUCKET_COUNT);
        for i in 0..BUCKET_COUNT {
            assert_eq!(a[i].t, b[i].t, "bucket {i} left edge must be stable");
            assert_eq!(a[i].t, c[i].t, "bucket {i} left edge must be stable");
            assert!((a[i].burn - b[i].burn).abs() < 1e-6, "bucket {i} burn drifted between polls");
            assert!((a[i].burn - c[i].burn).abs() < 1e-6, "bucket {i} burn drifted between polls");
            assert_eq!(a[i].reset, b[i].reset, "bucket {i} reset flag drifted");
            assert_eq!(a[i].reset, c[i].reset, "bucket {i} reset flag drifted");
        }
    }

    /// And the complementary contract: when `now` crosses INTO the next bucket
    /// index, every bar shifts left by exactly one slot — the intended scroll.
    #[test]
    fn burn_buckets_scrolls_left_by_one_when_now_crosses_a_boundary() {
        let period: i64 = 5 * 3600;
        let bucket_secs = period / BUCKET_COUNT as i64;
        let base_now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let now_bucket = base_now.timestamp().div_euclid(bucket_secs);
        let first_visible = now_bucket - (BUCKET_COUNT as i64 - 1);
        // Fill one specific historical bucket with a recognizable burn.
        let target = first_visible + 10;
        let ts = target * bucket_secs + 4;
        let points = vec![p(ts - 1, 10.0), p(ts, 14.0)]; // +4 burn in `target`
        let before = burn_buckets(&points, period, base_now);
        // Advance `now` into the NEXT bucket index.
        let next_now_ts = (now_bucket + 1) * bucket_secs + 3;
        let after = burn_buckets(&points, period, DateTime::from_timestamp(next_now_ts, 0).unwrap());
        // Locate the +4 burn before and after; it must have moved one slot left.
        let find_burn = |bs: &[BurnBucket]| -> usize {
            bs.iter()
                .position(|b| (b.burn - 4.0).abs() < 1e-4)
                .expect("the +4 burn must be present")
        };
        let before_idx = find_burn(&before);
        let after_idx = find_burn(&after);
        assert_eq!(
            after_idx,
            before_idx.saturating_sub(1),
            "crossing a boundary must shift the burn one slot to the left"
        );
    }

    // ----- burn_history -----

    #[tokio::test]
    async fn burn_history_returns_one_entry_per_provider_with_a_snapshot() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let glm_snap = snap_at(
            "Z.ai Coding Plan",
            vec![win("weekly", 42.0), win("5h", 33.0)],
            now,
        );
        let codex_snap = snap_at("OpenAI Codex CLI", vec![win("weekly", 11.0)], now);
        let snaps: crate::scheduler::SnapshotMap = Arc::new(RwLock::new(HashMap::from([
            ("glm".to_string(), glm_snap),
            ("codex".to_string(), codex_snap),
        ])));
        let history: HistoryMap = Arc::new(RwLock::new(HashMap::new()));
        let out = burn_history(&snaps, &history).await;
        // Canonical order (PROVIDER_IDS) means glm, minimax, codex, ...
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "glm");
        assert_eq!(out[0].provider, "Z.ai Coding Plan");
        assert_eq!(out[0].windows.len(), 2);
        assert_eq!(out[0].windows[0].label, "weekly");
        assert_eq!(out[0].windows[0].buckets.len(), BUCKET_COUNT);
        assert_eq!(out[0].windows[1].label, "5h");
        assert_eq!(out[0].windows[1].buckets.len(), BUCKET_COUNT);
        assert_eq!(out[1].id, "codex");
        assert_eq!(out[1].provider, "OpenAI Codex CLI");
        assert_eq!(out[1].windows.len(), 1);
        assert_eq!(out[1].windows[0].label, "weekly");
        assert_eq!(out[1].windows[0].buckets.len(), BUCKET_COUNT);
    }

    #[tokio::test]
    async fn burn_history_skips_providers_without_a_snapshot() {
        let snaps: crate::scheduler::SnapshotMap = Arc::new(RwLock::new(HashMap::new()));
        let history: HistoryMap = Arc::new(RwLock::new(HashMap::new()));
        let out = burn_history(&snaps, &history).await;
        assert!(out.is_empty(), "no snapshot => no entries");
    }

    #[tokio::test]
    async fn burn_history_uses_seven_days_for_weekly_and_five_hours_for_5h() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let glm_snap = snap_at("Z.ai", vec![win("weekly", 42.0), win("5h", 33.0)], now);
        let snaps: crate::scheduler::SnapshotMap =
            Arc::new(RwLock::new(HashMap::from([("glm".to_string(), glm_snap)])));
        // A single isolated point carries no delta, so all buckets are empty —
        // we only assert the grid spacing/period here.
        let history: HistoryMap = Arc::new(RwLock::new(HashMap::from([(
            "glm".to_string(),
            HashMap::from([(
                "weekly".to_string(),
                vec![HistoryPoint {
                    ts: now.timestamp() - 3 * 24 * 3600,
                    used_percent: 30.0,
                }],
            )]),
        )])));
        let out = burn_history(&snaps, &history).await;
        // burn_history() anchors the grid to Utc::now() (real time), not the
        // test's `now`, so the fixture point falls outside the visible window.
        // Assert only the grid geometry: 60 buckets, evenly spaced at the
        // weekly (7d/60) and 5h (5h/60) cadences.
        let weekly = &out[0].windows[0].buckets;
        let weekly_bucket_secs = 7 * 24 * 3600 / BUCKET_COUNT as i64;
        assert_eq!(weekly.len(), BUCKET_COUNT);
        for w in weekly.windows(2) {
            assert_eq!(w[1].t - w[0].t, weekly_bucket_secs);
        }
        let five = &out[0].windows[1].buckets;
        let five_bucket_secs = 5 * 3600 / BUCKET_COUNT as i64;
        assert_eq!(five.len(), BUCKET_COUNT);
        for w in five.windows(2) {
            assert_eq!(w[1].t - w[0].t, five_bucket_secs);
        }
    }
}
