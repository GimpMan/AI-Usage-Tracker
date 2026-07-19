use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, OnceLock};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};

use crate::providers::{ProviderFetch, ProviderHealth, UsageSnapshot};
use crate::secrets::Secrets;

/// Seconds to sleep when a provider is busy, before retrying the lock.
pub const BUSY_RETRY_SECS: u64 = 2;

/// Hard cap on a single provider fetch so a hung network / disk walk cannot
/// hold the per-provider lock (and stall the sequential refresh cycle) forever.
pub const PROVIDER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Run `provider.fetch` with [`PROVIDER_FETCH_TIMEOUT`]. On timeout, return a
/// transient failure so last-good snapshots are kept.
pub async fn fetch_with_timeout(
    provider: &dyn crate::providers::Provider,
    secrets: &Secrets,
) -> ProviderFetch {
    match tokio::time::timeout(PROVIDER_FETCH_TIMEOUT, provider.fetch(secrets)).await {
        Ok(outcome) => outcome,
        Err(_) => ProviderFetch::transient(format!(
            "{}: fetch timed out after {}s",
            provider.id(),
            PROVIDER_FETCH_TIMEOUT.as_secs()
        )),
    }
}

/// Per-provider fetch locks. Manual popup refresh and the scheduler share these.
fn provider_fetch_locks() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn provider_lock(provider_id: &str) -> Arc<Mutex<()>> {
    let mut map = provider_fetch_locks().lock().await;
    map.entry(provider_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn emit_waiting(
    app: Option<&AppHandle>,
    provider_id: &str,
    message: String,
    attempt: Option<u32>,
    retry_in_secs: Option<u64>,
) {
    let Some(app) = app else {
        return;
    };
    let _ = app.emit(
        "provider-refresh",
        &ProviderRefreshEvent {
            provider: provider_id.to_string(),
            phase: "waiting".into(),
            ok: None,
            message: Some(message),
            health: None,
            attempt,
            retry_in_secs,
        },
    );
}

/// Run `fut` while holding the exclusive fetch lock for `provider_id`.
///
/// Always starts with step **1/3** (busy check). If that provider is already
/// fetching: stay on `phase: "waiting"`, count down each second of the
/// [`BUSY_RETRY_SECS`] delay, then retry — until the lock is free. Then run
/// the fetch (2/3) and apply (3/3 in UI).
pub async fn with_provider_fetch_lock<T>(
    provider_id: &str,
    app: Option<&AppHandle>,
    fut: impl Future<Output = T>,
) -> T {
    let lock = provider_lock(provider_id).await;
    // Step 1/3 — always announced so the progress UI shows three checks.
    emit_waiting(
        app,
        provider_id,
        "1/3 Checking if provider is free…".into(),
        None,
        None,
    );
    let mut attempt: u32 = 0;
    let guard = loop {
        match lock.try_lock() {
            Ok(g) => break g,
            Err(_) => {
                attempt = attempt.saturating_add(1);
                // Tick the full 2s wait so the UI can show waiting + retry countdown.
                for remaining in (1..=BUSY_RETRY_SECS).rev() {
                    emit_waiting(
                        app,
                        provider_id,
                        format!(
                            "1/3 Provider busy — waiting {remaining}s, then retry (attempt {attempt})"
                        ),
                        Some(attempt),
                        Some(remaining),
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
                emit_waiting(
                    app,
                    provider_id,
                    format!("1/3 Retrying now (attempt {attempt})…"),
                    Some(attempt),
                    Some(0),
                );
            }
        }
    };
    let result = fut.await;
    drop(guard);
    result
}

/// Emitted per provider during refreshes so the open popup can show progress.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderRefreshEvent {
    pub provider: String,
    /// `"waiting"` | `"started"` | `"finished"`
    pub phase: String,
    pub ok: Option<bool>,
    pub message: Option<String>,
    pub health: Option<String>,
    /// Busy-wait attempt number (1-based), when phase is `waiting` after a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Seconds left in the current 2s retry delay (counts down while waiting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_secs: Option<u64>,
}

/// Latest snapshot per provider id.
pub type SnapshotMap = Arc<RwLock<HashMap<String, UsageSnapshot>>>;
pub type HealthMap = Arc<RwLock<HashMap<String, ProviderHealthState>>>;

#[derive(Clone, Debug)]
pub struct ProviderHealthState {
    pub health: ProviderHealth,
    pub transient_failures: u8,
    pub reason: Option<String>,
}

impl ProviderHealthState {
    pub fn healthy() -> Self {
        Self {
            health: ProviderHealth::Healthy,
            transient_failures: 0,
            reason: None,
        }
    }
}

pub const TRANSIENT_FAILURE_THRESHOLD: u8 = 3;

pub fn next_transient_count(current: u8) -> u8 {
    current.saturating_add(1).min(TRANSIENT_FAILURE_THRESHOLD)
}

pub fn transient_warning_after(count: u8) -> bool {
    count >= TRANSIENT_FAILURE_THRESHOLD
}

pub fn reset_transient_count() -> u8 {
    0
}

pub struct Scheduler {
    snaps: SnapshotMap,
    health: HealthMap,
    history: crate::history::HistoryMap,
    secrets: Secrets,
    app: Option<AppHandle>,
}

impl Scheduler {
    pub fn new(
        snaps: SnapshotMap,
        health: HealthMap,
        history: crate::history::HistoryMap,
        secrets: Secrets,
        app: AppHandle,
    ) -> Self {
        Self {
            snaps,
            health,
            history,
            secrets,
            app: Some(app),
        }
    }

    fn emit_provider_refresh(&self, event: ProviderRefreshEvent) {
        let Some(app) = self.app.as_ref() else {
            return;
        };
        if let Err(e) = app.emit("provider-refresh", &event) {
            log::warn!("provider-refresh emit failed: {e}");
        }
    }

    /// Fetch every currently-registered, non-hidden provider **one at a time**.
    ///
    /// Before each fetch: if that provider is busy, emit `waiting`, sleep 2s,
    /// and retry until the lock is free — then fetch. Snapshot locks are
    /// released between providers so the overlay can pull mid-cycle. Quota
    /// events (window resets, threshold alerts) are dispatched per provider
    /// after its locks drop; the tray tooltip is rewritten after persist.
    pub async fn refresh_once(&self) {
        let providers: Vec<Box<dyn crate::providers::Provider>> = crate::build_providers()
            .into_iter()
            .filter(|p| !crate::secrets::is_hidden(p.id()))
            .collect();
        let active: HashSet<String> = providers.iter().map(|p| p.id().to_string()).collect();

        for p in &providers {
            let provider_id = p.id().to_string();
            let snaps = self.snaps.clone();
            let health_map = self.health.clone();

            let (events, snap_clone) =
                with_provider_fetch_lock(&provider_id, self.app.as_ref(), async {
                    self.emit_provider_refresh(ProviderRefreshEvent {
                        provider: provider_id.clone(),
                        phase: "started".into(),
                        ok: None,
                        message: Some("2/3 Fetching latest usage…".into()),
                        health: None,
                        attempt: None,
                        retry_in_secs: None,
                    });
                    let outcome = fetch_with_timeout(p.as_ref(), &self.secrets).await;
                    let health = outcome.health.clone();
                    let reason = outcome.reason.clone();
                    let events = {
                        let mut guard = snaps.write().await;
                        let mut health_guard = health_map.write().await;
                        apply_fetch_outcome(&mut guard, &mut health_guard, &provider_id, outcome)
                    };
                    let ok = matches!(health, ProviderHealth::Healthy);
                    let snap_clone = if ok {
                        let guard = snaps.read().await;
                        guard.get(&provider_id).cloned()
                    } else {
                        None
                    };
                    self.emit_provider_refresh(ProviderRefreshEvent {
                        provider: provider_id.clone(),
                        phase: "finished".into(),
                        ok: Some(ok),
                        message: Some(health.refresh_message(reason.as_deref())),
                        health: if ok { None } else { Some(health.label()) },
                        attempt: None,
                        retry_in_secs: None,
                    });
                    (events, snap_clone)
                })
                .await;
            // Snapshot locks are dropped: record history, emit reset events,
            // and fire notifications. Record here so we capture the
            // post-hold value with no concurrent writers.
            if let Some(snap) = snap_clone {
                let mut h = self.history.write().await;
                crate::history::record_snapshot(&mut h, &provider_id, &snap);
            }
            if let Some(app) = &self.app {
                crate::alerts::dispatch_quota_events(app, &provider_id, &events);
            }
        }
        {
            let mut guard = self.snaps.write().await;
            guard.retain(|k, _| active.contains(k));
        }
        persist(&self.snaps).await;
        crate::history::save_history(&self.history).await;
        if let Some(app) = &self.app {
            let guard = self.snaps.read().await;
            crate::tray_tooltip::update_tray_tooltip(app, &guard);
        }
    }

    /// Run forever, reading the refresh interval from the persisted config
    /// on each tick so the user can change it from Settings without a
    /// restart.
    pub async fn run(self) {
        // initial fetch so the overlay has data on first open
        self.refresh_once().await;
        loop {
            let secs = crate::secrets::get_refresh_interval();
            tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
            self.refresh_once().await;
        }
    }
}

/// Apply a fetch outcome to the snapshot and health maps.
///
/// Healthy snapshots go through last-good holding (impossible same-window
/// drops are replaced by the previous value) before insertion. Returns the
/// quota events detected between the previous and the incoming snapshot —
/// window resets and edge-triggered threshold alerts — so callers can
/// dispatch them after releasing the map locks. Non-healthy outcomes and
/// first-ever fetches produce no events.
pub fn apply_fetch_outcome(
    snapshots: &mut HashMap<String, UsageSnapshot>,
    health: &mut HashMap<String, ProviderHealthState>,
    provider_id: &str,
    outcome: ProviderFetch,
) -> crate::alerts::QuotaEvents {
    match outcome.health {
        ProviderHealth::Healthy => {
            let mut events = crate::alerts::QuotaEvents::default();
            if let Some(mut snapshot) = outcome.snapshot {
                if let Some(prev) = snapshots.get(provider_id) {
                    hold_last_good_used_percent(prev, &mut snapshot);
                    events = crate::alerts::detect_quota_events(prev, &snapshot, provider_id);
                }
                snapshots.insert(provider_id.to_string(), snapshot);
            }
            health.insert(provider_id.to_string(), ProviderHealthState::healthy());
            events
        }
        ProviderHealth::TransientFailure => {
            let state = health
                .entry(provider_id.to_string())
                .or_insert_with(ProviderHealthState::healthy);
            state.transient_failures = next_transient_count(state.transient_failures);
            state.health = ProviderHealth::TransientFailure;
            state.reason = outcome.reason.clone();

            if let Some(mut snapshot) = outcome.snapshot {
                if !transient_warning_after(state.transient_failures) {
                    snapshot.unavailable_reason = None;
                }
                snapshots.insert(provider_id.to_string(), snapshot);
            } else if transient_warning_after(state.transient_failures) {
                if let Some(snapshot) = snapshots.get_mut(provider_id) {
                    snapshot.unavailable_reason = outcome.reason;
                }
            }
            crate::alerts::QuotaEvents::default()
        }
        ProviderHealth::MissingCredentials => {
            // No credentials at all: nothing to show, and displaying last-good
            // data for a deliberately unconfigured provider would be wrong.
            health.insert(
                provider_id.to_string(),
                ProviderHealthState {
                    health: outcome.health,
                    transient_failures: reset_transient_count(),
                    reason: outcome.reason,
                },
            );
            snapshots.remove(provider_id);
            let _ = crate::secrets::set_hidden(provider_id, true);
            crate::alerts::QuotaEvents::default()
        }
        ProviderHealth::InvalidCredentials | ProviderHealth::NoUsableDetails => {
            // Definitive rejection, but possibly short-lived (rotated-then-
            // fixed key, upstream schema drift). Keep the last-good snapshot
            // and stamp the reason on it: the bar renders it as stale, and
            // the segment recovers on its own once the provider accepts us
            // again. Never remove + auto-hide here — a single bad cycle must
            // not make the segment disappear permanently.
            health.insert(
                provider_id.to_string(),
                ProviderHealthState {
                    health: outcome.health.clone(),
                    transient_failures: reset_transient_count(),
                    reason: outcome.reason.clone(),
                },
            );
            if let Some(snapshot) = snapshots.get_mut(provider_id) {
                snapshot.unavailable_reason = outcome.reason;
            }
            crate::alerts::QuotaEvents::default()
        }
    }
}

/// Tolerated downward fluctuation (used-percent points) within the same
/// rate-limit window. `used_percent` is monotonic non-decreasing within a
/// window; a larger drop without a `reset_at` change is a transient bad
/// reading (the `/wham/usage` endpoint intermittently sends near-zero values).
pub(crate) const USED_PERCENT_DROP_TOLERANCE: f32 = 1.0;

/// Hold the previous good `used_percent` when an incoming reading is
/// impossible. Rate-limit usage can't decrease within a window until it
/// resets, so a same-`reset_at` drop larger than the tolerance is treated as
/// upstream garbage and the last good value is kept. Only applies to bar
/// windows that carry a `reset_at` and aren't unlimited on either side, so
/// MiniMax limited↔unlimited transitions, reset-at-less balances, and
/// popup-only windows pass through unchanged.
fn hold_last_good_used_percent(prev: &UsageSnapshot, incoming: &mut UsageSnapshot) {
    for w in incoming.windows.iter_mut() {
        if !w.bar_visible || w.is_unlimited {
            continue;
        }
        let Some(reset_at) = w.reset_at else {
            continue;
        };
        let Some(prev_w) = prev.windows.iter().find(|p| p.label == w.label) else {
            continue;
        };
        if !prev_w.bar_visible || prev_w.is_unlimited {
            continue;
        }
        if prev_w.reset_at == Some(reset_at)
            && w.used_percent + USED_PERCENT_DROP_TOLERANCE < prev_w.used_percent
        {
            w.used_percent = prev_w.used_percent;
        }
    }
}

/// Load last-good snapshot state from disk (if any) so transient network errors
/// never blank the overlay. `persist` only writes successful snapshots, so
/// anything in this file is by construction a last-good view.
pub fn state_path() -> Option<std::path::PathBuf> {
    crate::secrets::config_dir()
        .ok()
        .map(|d| d.join("state.json"))
}

pub async fn load_persisted(snaps: &SnapshotMap) {
    let Some(path) = state_path() else { return };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(map) = serde_json::from_str::<HashMap<String, UsageSnapshot>>(&text) else {
        return;
    };
    // Only rehydrate providers that are currently registered — otherwise a
    // stale Claude snapshot from before the user logged out would briefly
    // resurrect the segment on startup.
    let active: HashSet<String> = crate::build_providers()
        .iter()
        .map(|p| p.id().to_string())
        .collect();
    let mut guard = snaps.write().await;
    for (k, v) in map {
        if active.contains(&k) {
            guard.insert(k, v);
        }
    }
}

/// Merge current snapshots into the persisted file, but only for entries that
/// do NOT have an `unavailable_reason`. This honors GLM_USAGE_TRACKER.md §8:
/// "Persist the last successful snapshot so a transient network failure
/// doesn't make the bar disappear."
pub async fn persist(snaps: &SnapshotMap) {
    let current: HashMap<String, UsageSnapshot> = snaps.read().await.clone();
    let Some(path) = state_path() else {
        return;
    };

    // Start from whatever is already on disk (last-good), then overlay only
    // the entries that succeeded in this tick.
    let mut merged: HashMap<String, UsageSnapshot> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();

    for (k, v) in current {
        if v.unavailable_reason.is_none() {
            merged.insert(k, v);
        }
    }

    if let Ok(text) = serde_json::to_string_pretty(&merged) {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderFetch, UsageWindow};
    use chrono::{DateTime, Utc};

    #[test]
    fn transient_failures_increment_to_the_three_failure_threshold() {
        assert_eq!(next_transient_count(0), 1);
        assert_eq!(next_transient_count(1), 2);
        assert_eq!(next_transient_count(2), 3);
        assert_eq!(next_transient_count(3), 3);
        assert!(!transient_warning_after(2));
        assert!(transient_warning_after(3));
    }

    #[test]
    fn healthy_fetch_resets_transient_failures() {
        assert_eq!(reset_transient_count(), 0);
    }

    #[test]
    fn transient_failures_keep_last_good_snapshot_until_third_attempt() {
        let mut snapshots = HashMap::from([(
            "glm".to_string(),
            UsageSnapshot {
                provider: "Z.ai Coding Plan".into(),
                level: None,
                windows: vec![UsageWindow {
                    label: "weekly".into(),
                    used_percent: 20.0,
                    reset_at: None,
                    bar_visible: true,
                    is_unlimited: false,
                    used_absolute: None,
                    limit_absolute: None,
                }],
                unavailable_reason: None,
                fetched_at: Utc::now(),
            },
        )]);
        let mut health = HashMap::new();

        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "glm",
            ProviderFetch::transient("network error"),
        );
        assert!(snapshots["glm"].unavailable_reason.is_none());
        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "glm",
            ProviderFetch::transient("network error"),
        );
        assert!(snapshots["glm"].unavailable_reason.is_none());
        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "glm",
            ProviderFetch::transient("network error"),
        );
        assert_eq!(
            snapshots["glm"].unavailable_reason.as_deref(),
            Some("network error")
        );
        assert_eq!(health["glm"].transient_failures, 3);
    }

    fn snap(provider: &str, windows: Vec<UsageWindow>) -> UsageSnapshot {
        UsageSnapshot {
            provider: provider.into(),
            level: None,
            windows,
            unavailable_reason: None,
            fetched_at: Utc::now(),
        }
    }

    #[test]
    fn invalid_credentials_keep_last_good_snapshot_with_reason() {
        // A revoked key must not make the segment vanish: the last-good
        // snapshot stays, stamped with the reason so the bar renders stale.
        let mut snapshots = HashMap::from([(
            "minimax".to_string(),
            snap("MiniMax Coding Plan", vec![win("5h", 40.0, None)]),
        )]);
        let mut health = HashMap::new();

        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "minimax",
            ProviderFetch::hard(ProviderHealth::InvalidCredentials, "invalid api key"),
        );

        let kept = &snapshots["minimax"];
        assert_eq!(kept.unavailable_reason.as_deref(), Some("invalid api key"));
        assert_eq!(kept.windows.len(), 1);
        assert_eq!(health["minimax"].health, ProviderHealth::InvalidCredentials);
        assert_eq!(health["minimax"].transient_failures, 0);
    }

    #[test]
    fn no_usable_details_keep_last_good_snapshot_with_reason() {
        // Upstream schema drift ("endpoint returned no percentage fields")
        // must not wipe the segment either.
        let mut snapshots = HashMap::from([(
            "minimax".to_string(),
            snap("MiniMax Coding Plan", vec![win("5h", 40.0, None)]),
        )]);
        let mut health = HashMap::new();

        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "minimax",
            ProviderFetch::hard(
                ProviderHealth::NoUsableDetails,
                "endpoint returned no percentage fields",
            ),
        );

        let kept = &snapshots["minimax"];
        assert_eq!(
            kept.unavailable_reason.as_deref(),
            Some("endpoint returned no percentage fields")
        );
        assert_eq!(kept.windows.len(), 1);
        assert_eq!(health["minimax"].health, ProviderHealth::NoUsableDetails);
    }

    #[test]
    fn invalid_credentials_without_prior_snapshot_insert_nothing() {
        // First-ever fetch rejected: nothing to keep, nothing to show — but
        // the provider must stay unhidden so later cycles can recover.
        let mut snapshots = HashMap::new();
        let mut health = HashMap::new();

        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "minimax",
            ProviderFetch::hard(ProviderHealth::InvalidCredentials, "invalid api key"),
        );

        assert!(!snapshots.contains_key("minimax"));
        assert_eq!(health["minimax"].health, ProviderHealth::InvalidCredentials);
    }

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

    #[test]
    fn holds_last_good_used_percent_when_same_window_drops() {
        // The /wham/usage bug: weekly jumps 24 -> 1 with the SAME reset_at.
        // Impossible within a window, so the last good value (24) is held.
        let rst = Utc::now() + chrono::Duration::days(2);
        let prev = snap("Codex", vec![win("wk", 24.0, Some(rst))]);
        let mut incoming = snap("Codex", vec![win("wk", 1.0, Some(rst))]);
        hold_last_good_used_percent(&prev, &mut incoming);
        assert!((incoming.windows[0].used_percent - 24.0).abs() < 0.001);
    }

    #[test]
    fn accepts_used_percent_drop_when_reset_at_changed() {
        // A legit post-reset drop: new reset_at -> the low value is real.
        let rst_old = Utc::now() + chrono::Duration::days(1);
        let rst_new = Utc::now() + chrono::Duration::days(7);
        let prev = snap("Codex", vec![win("wk", 24.0, Some(rst_old))]);
        let mut incoming = snap("Codex", vec![win("wk", 2.0, Some(rst_new))]);
        hold_last_good_used_percent(&prev, &mut incoming);
        assert!((incoming.windows[0].used_percent - 2.0).abs() < 0.001);
    }

    #[test]
    fn tolerates_small_refinement_within_tolerance() {
        // <= 1pt downward refinement is accepted (server-side recalculation).
        let rst = Utc::now() + chrono::Duration::days(2);
        let prev = snap("GLM", vec![win("5h", 24.0, Some(rst))]);
        let mut incoming = snap("GLM", vec![win("5h", 23.5, Some(rst))]);
        hold_last_good_used_percent(&prev, &mut incoming);
        assert!((incoming.windows[0].used_percent - 23.5).abs() < 0.001);
    }

    #[test]
    fn ignores_unlimited_windows_and_missing_reset_at() {
        // MiniMax weekly going unlimited must not be held; balances (no
        // reset_at) are ignored entirely.
        let rst = Utc::now() + chrono::Duration::days(2);
        let prev = snap(
            "MiniMax",
            vec![
                win("5h", 50.0, Some(rst)),
                UsageWindow {
                    label: "wk".into(),
                    used_percent: 50.0,
                    reset_at: Some(rst),
                    bar_visible: true,
                    is_unlimited: true,
                    used_absolute: None,
                    limit_absolute: None,
                },
            ],
        );
        let mut incoming = snap(
            "MiniMax",
            vec![
                win("5h", 5.0, Some(rst)), // same reset, big drop -> held to 50
                UsageWindow {
                    label: "wk".into(),
                    used_percent: 0.0,
                    reset_at: Some(rst),
                    bar_visible: true,
                    is_unlimited: true,
                    used_absolute: None,
                    limit_absolute: None,
                }, // unlimited -> untouched
                win("balance", 3.0, None), // no reset_at -> untouched
            ],
        );
        hold_last_good_used_percent(&prev, &mut incoming);
        assert!(
            (incoming.windows[0].used_percent - 50.0).abs() < 0.001,
            "5h held"
        );
        assert!(
            (incoming.windows[1].used_percent - 0.0).abs() < 0.001,
            "unlimited untouched"
        );
        assert!(
            (incoming.windows[2].used_percent - 3.0).abs() < 0.001,
            "balance untouched"
        );
    }

    #[test]
    fn apply_fetch_outcome_holds_value_across_healthy_refresh() {
        // End-to-end through the public path: a Healthy refresh carrying the
        // garbage value still leaves the held good value in the map.
        let rst = Utc::now() + chrono::Duration::days(2);
        let mut snapshots = HashMap::from([(
            "codex".to_string(),
            snap("Codex", vec![win("wk", 24.0, Some(rst))]),
        )]);
        let mut health = HashMap::new();
        apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "codex",
            ProviderFetch::healthy(snap("Codex", vec![win("wk", 1.0, Some(rst))])),
        );
        assert!((snapshots["codex"].windows[0].used_percent - 24.0).abs() < 0.001);
    }

    #[test]
    fn apply_fetch_outcome_returns_quota_events() {
        // A new reset_at plus a real usage drop surfaces as a reset event.
        let now = Utc::now();
        let mut snapshots = HashMap::from([(
            "glm".to_string(),
            snap(
                "GLM",
                vec![win("weekly", 80.0, Some(now + chrono::Duration::days(2)))],
            ),
        )]);
        let mut health = HashMap::new();
        let events = apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "glm",
            ProviderFetch::healthy(snap(
                "GLM",
                vec![win("weekly", 5.0, Some(now + chrono::Duration::days(7)))],
            )),
        );
        assert_eq!(events.resets, vec!["weekly".to_string()]);

        // First-ever fetch: no previous snapshot, no events.
        let events = apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "codex",
            ProviderFetch::healthy(snap("Codex", vec![win("weekly", 5.0, None)])),
        );
        assert!(events.resets.is_empty());
        assert!(events.alerts.is_empty());

        // Non-healthy outcomes never produce events.
        let events = apply_fetch_outcome(
            &mut snapshots,
            &mut health,
            "glm",
            ProviderFetch::transient("network error"),
        );
        assert!(events.resets.is_empty());
        assert!(events.alerts.is_empty());
    }
}
