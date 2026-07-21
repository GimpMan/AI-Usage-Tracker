use std::collections::HashMap;

use serde::Serialize;

use crate::providers::{ProviderHealth, UsageSnapshot, PROVIDER_IDS};
use crate::scheduler::{HealthMap, SnapshotMap};
use crate::secrets::{
    get_refresh_interval as read_refresh_interval, get_region, is_hidden, set_hidden,
    set_refresh_interval as write_refresh_interval, set_region, Secrets,
};

#[tauri::command]
pub async fn get_update_state(
    manager: tauri::State<'_, crate::updates::UpdateManager>,
) -> Result<crate::updates::UpdateState, String> {
    Ok(manager.state().await)
}

#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    manager: tauri::State<'_, crate::updates::UpdateManager>,
    manual: bool,
) -> Result<crate::updates::UpdateState, String> {
    manager.check(&app, manual).await
}

#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    manager: tauri::State<'_, crate::updates::UpdateManager>,
) -> Result<(), String> {
    manager.install(&app).await
}

#[tauri::command]
pub async fn set_update_channel(
    app: tauri::AppHandle,
    manager: tauri::State<'_, crate::updates::UpdateManager>,
    channel: crate::secrets::UpdateChannel,
) -> Result<crate::updates::UpdateState, String> {
    manager.set_channel(&app, channel).await
}

pub fn secrets_handle() -> Secrets {
    Secrets
}

/// Build the list of snapshots to send to the frontend, ordered by the
/// canonical provider order. Hidden providers are omitted so the minibar
/// drops a card as soon as Settings flips the Overlay switch — even if a
/// last-good snapshot is still in memory for a fast re-show.
pub async fn collect(snaps: &SnapshotMap) -> Vec<UsageSnapshot> {
    let guard = snaps.read().await;
    let mut out: Vec<UsageSnapshot> = Vec::new();
    for id in PROVIDER_IDS {
        if is_hidden(id) {
            continue;
        }
        if let Some(s) = guard.get(*id) {
            out.push(s.clone());
        }
    }
    out
}

/// Structured status of one provider, returned to the Settings window so it can
/// render accurate "configured / available / hidden" hints without guessing from
/// the frontend's hardcoded metadata.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderStatus {
    pub id: &'static str,
    pub label: &'static str,
    pub needs_key: bool,
    pub has_region: bool,
    pub registered: bool,
    pub configured: bool,
    pub eligible: bool,
    pub health: ProviderHealth,
    pub health_reason: Option<String>,
    pub hidden: bool,
    pub region: Option<String>,
    pub unavailable_reason: Option<String>,
}

fn visibility_eligible(registered: bool, configured: bool, health: &ProviderHealth) -> bool {
    registered
        && configured
        && !matches!(
            health,
            ProviderHealth::MissingCredentials
                | ProviderHealth::InvalidCredentials
                | ProviderHealth::NoUsableDetails
        )
}

/// Build the canonical list of provider statuses for *every* known provider
/// id (not only those currently registered). Settings always renders the full
/// PROVIDERS list in the frontend; if we omit an id here (e.g. Claude when no
/// subscription is detected), `status` is undefined and the "Show in overlay"
/// checkbox falls back to checked — even when `hidden: true` is saved on disk.
///
/// Intentionally **does not** call `Provider::fetch` — that hits live APIs and
/// made the settings panel take seconds to learn the saved `hidden` flags,
/// so checkboxes flashed as "shown" then flipped. Configured/registered is
/// decided from local secrets + registration only.
pub async fn provider_statuses(health: &HealthMap) -> Vec<ProviderStatus> {
    let registered: HashMap<String, &'static str> = crate::build_providers()
        .into_iter()
        .map(|p| (p.id().to_string(), p.label()))
        .collect();
    let mut out = Vec::new();
    let health_guard = health.read().await;
    for id in PROVIDER_IDS {
        let registered_label = registered.get(*id).copied();
        let is_registered = registered_label.is_some();
        let mut hidden = is_hidden(id);
        let (configured, unavailable_reason) = provider_configured_local(id, is_registered);
        let live = health_guard.get(*id);
        let current_health = live.map(|state| state.health.clone()).unwrap_or_else(|| {
            if !is_registered || !configured {
                ProviderHealth::MissingCredentials
            } else {
                ProviderHealth::Healthy
            }
        });
        let eligible = visibility_eligible(is_registered, configured, &current_health);
        let health_reason = live
            .and_then(|state| state.reason.clone())
            .or(unavailable_reason.clone());
        // Auto-hide only when there is nothing to track (unregistered,
        // unconfigured, or credentials missing). Credential rejections and
        // upstream schema problems keep the saved visibility flag so the bar
        // segment can recover on its own once the issue clears.
        let auto_hide = !eligible
            && (!is_registered
                || !configured
                || matches!(current_health, ProviderHealth::MissingCredentials));
        if auto_hide && !hidden {
            if let Err(e) = set_hidden(id, true) {
                log::warn!("auto-hide ineligible {id}: {e}");
            } else {
                hidden = true;
            }
        }
        out.push(ProviderStatus {
            id,
            label: registered_label.unwrap_or_else(|| provider_label(id)),
            needs_key: provider_needs_key(id),
            has_region: *id == "minimax",
            registered: is_registered,
            configured,
            eligible,
            health: current_health,
            health_reason: health_reason.clone(),
            hidden,
            region: get_region(id),
            unavailable_reason: health_reason,
        });
    }
    out
}

/// A local auth file can contain a refresh token that has since been revoked.
/// The provider fetch is the only place that can observe that server-side
/// invalidation, so reflect auth-specific live failures in Settings while
/// leaving transient network failures as a signed-in state.
fn apply_live_auth_failures(
    statuses: &mut [ProviderStatus],
    snapshots: &HashMap<String, UsageSnapshot>,
) {
    for status in statuses.iter_mut() {
        if !matches!(status.id, "grok" | "codex" | "kimi") {
            continue;
        }
        let Some(reason) = snapshots
            .get(status.id)
            .and_then(|snapshot| snapshot.unavailable_reason.as_deref())
        else {
            continue;
        };
        if reason.to_ascii_lowercase().contains("session expired") {
            status.configured = false;
            status.eligible = false;
            status.health = ProviderHealth::InvalidCredentials;
            status.health_reason = Some(reason.to_string());
            status.unavailable_reason = Some(reason.to_string());
        }
    }
}

fn provider_label(id: &str) -> &'static str {
    match id {
        "glm" => "Z.ai Coding Plan",
        "minimax" => "MiniMax Coding Plan",
        "codex" => "OpenAI Codex CLI",
        "claude" => "Claude Code",
        "grok" => "Grok (SuperGrok / Build)",
        "kimi" => "Kimi Code",
        "openrouter" => "OpenRouter",
        _ => "Unknown provider",
    }
}

fn provider_unregistered_reason(id: &str) -> &'static str {
    match id {
        "claude" => "not detected — sign in with `claude login` and use an active Pro/Max plan",
        "grok" => "not detected — run `grok login` (Grok Build / SuperGrok)",
        _ => "provider not registered",
    }
}

/// Local-only configured/registered hints — no network. Used by settings so
/// the panel can paint the saved hide flags immediately.
///
/// For OAuth providers (Grok / Codex / Kimi) we classify the app session in
/// Credential Manager (with one-time import from legacy CLI auth files).
/// The badge flips to red NOT SIGNED IN when the session is missing, malformed,
/// empty, expired-without-refresh, or in an unsupported auth mode — and the
/// `unavailable_reason` carries a short explanation the Settings UI can show.
/// Claude's Pro/Max behavior is preserved because its registration is decided
/// by `has_active_claude_subscription` in `main.rs`; if it reaches this
/// function it is registered and has a session.
fn provider_configured_local(id: &str, registered: bool) -> (bool, Option<String>) {
    if provider_needs_key(id) {
        let configured = if id == "openrouter" {
            Secrets.get("openrouter").is_some() || Secrets.get("openrouter_management").is_some()
        } else {
            Secrets.get(id).is_some()
        };
        let reason = if configured {
            None
        } else {
            Some(if id == "openrouter" {
                "no api key or management key configured".to_string()
            } else {
                "no api key configured".to_string()
            })
        };
        (configured, reason)
    } else if !registered {
        (false, Some(provider_unregistered_reason(id).to_string()))
    } else {
        match id {
            "grok" => classify_local_oauth("grok"),
            "codex" => classify_local_oauth("codex"),
            "kimi" => classify_local_oauth("kimi"),
            "claude" => (true, None),
            _ => (true, None),
        }
    }
}

/// Read the provider's app OAuth session (Credential Manager, with one-time
/// CLI-file import) and ask its provider module whether the credentials are
/// usable today (including refreshable sessions).
/// Returns `(configured, reason_for_settings)`. All errors are non-secret —
/// never include the raw token or its length.
fn classify_local_oauth(id: &str) -> (bool, Option<String>) {
    match id {
        "grok" => {
            let path = std::env::var("GROK_HOME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| dirs::home_dir().map(|h| h.join(".grok")))
                .map(|p| p.join("auth.json"));
            let value = match path.as_ref() {
                Some(p) => crate::secrets::oauth_get_json_or_import_file("grok", p),
                None => crate::secrets::oauth_get_json("grok"),
            };
            let Some(value) = value else {
                return (
                    false,
                    Some(crate::providers::grok::REASON_NO_AUTH_STATUS.to_string()),
                );
            };
            let Some(map) = value.as_object() else {
                return (
                    false,
                    Some(crate::providers::grok::REASON_NO_AUTH_STATUS.to_string()),
                );
            };
            crate::providers::grok::classify_auth_status(map)
        }
        "codex" => match crate::providers::codex::load_auth_doc() {
            Ok(doc) => crate::providers::codex::classify_auth_status(&doc),
            Err(_) => (
                false,
                Some(crate::providers::codex::REASON_NO_AUTH_STATUS.to_string()),
            ),
        },
        "kimi" => {
            let path = crate::providers::kimi::credentials_path().ok();
            let value = match path.as_ref() {
                Some(p) => crate::secrets::oauth_get_json_or_import_file("kimi", p),
                None => crate::secrets::oauth_get_json("kimi"),
            };
            let Some(doc) = value else {
                return (
                    false,
                    Some(crate::providers::kimi::REASON_NO_AUTH_STATUS.to_string()),
                );
            };
            crate::providers::kimi::classify_auth_status(&doc)
        }
        _ => (true, None),
    }
}

fn provider_needs_key(id: &str) -> bool {
    matches!(id, "glm" | "minimax" | "openrouter")
}

#[tauri::command]
pub async fn get_usage(
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<UsageSnapshot>, String> {
    Ok(collect(&state.snapshots).await)
}

/// Bucketed burn history for the popup's burn bars (weekly + 5h windows).
#[tauri::command]
pub async fn get_burn_history(
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<crate::history::ProviderBurnHistory>, String> {
    Ok(crate::history::burn_history(&state.snapshots, &state.history).await)
}

#[tauri::command]
pub async fn save_key(
    app: tauri::AppHandle,
    provider: String,
    key: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(format!("{provider}: no key provided"));
    }
    log::info!("save_key: provider={provider} key_len={}", trimmed.len());
    Secrets.set(&provider, trimmed).map_err(|e| {
        log::error!("save_key: set failed: {e}");
        e.to_string()
    })?;
    state.health.write().await.remove(&provider);
    log::info!("save_key: stored, triggering refresh");
    do_refresh(&app, &state).await;
    log::info!("save_key: refresh done");
    Ok(())
}

#[tauri::command]
pub async fn save_openrouter_management_key(
    key: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("openrouter management key: no key provided".into());
    }
    let old = Secrets.get("openrouter_management");
    Secrets
        .set("openrouter_management", trimmed)
        .map_err(|e| e.to_string())?;
    if old.as_deref() != Some(trimmed) {
        crate::secrets::TopupBaseline::clear("openrouter_management")?;
        crate::secrets::AccountBalanceBaseline::clear("openrouter_management")?;
    }
    state.health.write().await.remove("openrouter");
    Ok(())
}

/// Whether a management key is stored. Never returns secret material to the webview.
#[tauri::command]
pub async fn load_openrouter_management_key() -> Result<bool, String> {
    Ok(Secrets
        .get("openrouter_management")
        .is_some_and(|s| !s.trim().is_empty()))
}

#[tauri::command]
pub async fn delete_openrouter_management_key(
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    Secrets
        .delete("openrouter_management")
        .map_err(|e| e.to_string())?;
    crate::secrets::TopupBaseline::clear("openrouter_management")?;
    crate::secrets::AccountBalanceBaseline::clear("openrouter_management")?;
    state.health.write().await.remove("openrouter");
    state.snapshots.write().await.remove("openrouter");
    Ok(())
}

#[tauri::command]
pub async fn test_openrouter_management_key(key: Option<String>) -> Result<String, String> {
    let actual = match key {
        Some(value) if !value.trim().is_empty() => value,
        _ => Secrets
            .get("openrouter_management")
            .ok_or_else(|| "openrouter management key: no key provided".to_string())?,
    };
    crate::providers::openrouter::test_management_key(&actual).await
}

#[tauri::command]
pub async fn rebase_openrouter_account(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let key = Secrets
        .get("openrouter_management")
        .ok_or_else(|| "openrouter management key: no key provided".to_string())?;
    // Hold the provider fetch lock so a scheduled refresh can't read stale
    // baselines mid-rebase and then clobber the rebase when it writes.
    let message = crate::scheduler::with_provider_fetch_lock(
        "openrouter",
        Some(&app),
        crate::providers::openrouter::rebase_account(&key),
    )
    .await?;
    state.health.write().await.remove("openrouter");
    Ok(message)
}

/// Whether an API key is stored for `provider`. Never returns secret material
/// to the webview (defense in depth if XSS is ever introduced).
#[tauri::command]
pub async fn load_key(provider: String) -> Result<bool, String> {
    Ok(Secrets
        .get(&provider)
        .is_some_and(|s| !s.trim().is_empty()))
}

#[tauri::command]
pub async fn delete_key(
    provider: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    Secrets.delete(&provider).map_err(|e| e.to_string())?;
    // Drop the cached snapshot for this provider — without a key we can't fetch,
    // and showing stale data would be misleading.
    {
        let mut guard = state.snapshots.write().await;
        guard.remove(&provider);
    }
    state.health.write().await.remove(&provider);
    crate::scheduler::persist(&state.snapshots).await;
    Ok(())
}

#[tauri::command]
pub async fn save_region(provider: String, region: String) -> Result<(), String> {
    set_region(&provider, &region).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn load_region(provider: String) -> Result<Option<String>, String> {
    Ok(get_region(&provider))
}

/// Shared refresh logic used by both the manual "refresh_now" command and the
/// post-save-key hook. Hidden providers are filtered out so they neither fetch
/// nor occupy snapshot slots. Each provider busy-checks its lock (2s retry).
async fn do_refresh(app: &tauri::AppHandle, state: &crate::AppState) {
    let providers: Vec<Box<dyn crate::providers::Provider>> = crate::build_providers()
        .into_iter()
        .filter(|p| !crate::secrets::is_hidden(p.id()))
        .collect();
    let secrets = Secrets;
    for p in providers {
        let provider_id = p.id().to_string();
        let snaps = state.snapshots.clone();
        let health_map = state.health.clone();
        let (events, snap_clone) =
            crate::scheduler::with_provider_fetch_lock(&provider_id, Some(app), async {
                let outcome = crate::scheduler::fetch_with_timeout(p.as_ref(), &secrets).await;
                let health = outcome.health.clone();
                let (events, snap_clone) = {
                    let mut guard = snaps.write().await;
                    let mut health_guard = health_map.write().await;
                    let events = crate::scheduler::apply_fetch_outcome(
                        &mut guard,
                        &mut health_guard,
                        &provider_id,
                        outcome,
                    );
                    let snap_clone = if matches!(health, ProviderHealth::Healthy) {
                        guard.get(&provider_id).cloned()
                    } else {
                        None
                    };
                    (events, snap_clone)
                };
                (events, snap_clone)
            })
            .await;
        // Snapshot locks are dropped: record the post-hold history point.
        if let Some(snap) = snap_clone {
            let mut h = state.history.write().await;
            crate::history::record_snapshot(&mut h, &provider_id, &snap);
        }
        crate::alerts::dispatch_quota_events(app, &provider_id, &events);
    }
    // Drop any stale snapshot left behind by a provider that has since
    // been hidden — otherwise the bar would keep rendering its segment
    // until the user toggled visibility and refreshed manually.
    {
        let mut guard = state.snapshots.write().await;
        guard.retain(|k, _| !crate::secrets::is_hidden(k));
    }
    crate::scheduler::persist(&state.snapshots).await;
    crate::history::save_history(&state.history).await;
    let snapshots = state.snapshots.read().await;
    crate::tray_tooltip::update_tray_tooltip(app, &snapshots);
}

/// Outcome of a single-provider refresh for the overlay popup status line.
#[derive(Clone, Debug, Serialize)]
pub struct RefreshProviderResult {
    /// True only when the provider returned a healthy snapshot.
    pub ok: bool,
    /// Short user-facing status (`Updated`, network/rate-limit text, etc.).
    pub message: String,
    /// Backend health label when not ok (`TransientFailure`, …).
    pub health: Option<String>,
}

/// Refresh a single provider by id (`glm`, `codex`, …). Used by the per-card
/// refresh control on the overlay popup so only that provider is re-fetched.
/// Always applies the fetch outcome (so last-good / health state stays correct)
/// and reports success vs failure for the popup status line.
async fn do_refresh_provider(
    app: &tauri::AppHandle,
    state: &crate::AppState,
    provider_id: &str,
) -> Result<RefreshProviderResult, String> {
    if !PROVIDER_IDS.contains(&provider_id) {
        return Err(format!("unknown provider: {provider_id}"));
    }
    if crate::secrets::is_hidden(provider_id) {
        return Err(format!("{provider_id}: hidden in settings"));
    }
    let Some(provider) = crate::build_providers()
        .into_iter()
        .find(|p| p.id() == provider_id)
    else {
        return Err(format!("{provider_id}: not registered"));
    };
    let secrets = Secrets;
    let snaps = state.snapshots.clone();
    let health_map = state.health.clone();
    // Busy-check: if already fetching, emit waiting + sleep 2s + retry.
    let (ok, message, health_out, events, snap_clone) =
        crate::scheduler::with_provider_fetch_lock(provider_id, Some(app), async {
            use tauri::Emitter;
            let _ = app.emit(
                "provider-refresh",
                &crate::scheduler::ProviderRefreshEvent {
                    provider: provider_id.to_string(),
                    phase: "started".into(),
                    ok: None,
                    message: Some("2/3 Fetching latest usage…".into()),
                    health: None,
                    attempt: None,
                    retry_in_secs: None,
                },
            );
            let outcome =
                crate::scheduler::fetch_with_timeout(provider.as_ref(), &secrets).await;
            let health = outcome.health.clone();
            let reason = outcome.reason.clone();
            let events = {
                let mut guard = snaps.write().await;
                let mut health_guard = health_map.write().await;
                crate::scheduler::apply_fetch_outcome(
                    &mut guard,
                    &mut health_guard,
                    provider_id,
                    outcome,
                )
            };
            let ok = matches!(health, ProviderHealth::Healthy);
            let snap_clone = if ok {
                let guard = snaps.read().await;
                guard.get(provider_id).cloned()
            } else {
                None
            };
            (
                ok,
                health.refresh_message(reason.as_deref()),
                if ok { None } else { Some(health.label()) },
                events,
                snap_clone,
            )
        })
        .await;
    // Snapshot locks are dropped: record the post-hold history point.
    if let Some(snap) = snap_clone {
        let mut h = state.history.write().await;
        crate::history::record_snapshot(&mut h, provider_id, &snap);
    }
    crate::alerts::dispatch_quota_events(app, provider_id, &events);
    crate::scheduler::persist(&state.snapshots).await;
    crate::history::save_history(&state.history).await;
    let snapshots = state.snapshots.read().await;
    crate::tray_tooltip::update_tray_tooltip(app, &snapshots);
    Ok(RefreshProviderResult {
        ok,
        message,
        health: health_out,
    })
}

/// Test a provider key. If `key` is supplied (even an unsaved draft from the
/// Settings input), use that; otherwise fall back to the stored key.
/// `region` is used for MiniMax only (overseas | china); ignored otherwise.
#[tauri::command]
pub async fn test_key(
    provider: String,
    key: Option<String>,
    region: Option<String>,
) -> Result<String, String> {
    let actual = match key {
        Some(k) if !k.trim().is_empty() => k,
        _ => match Secrets.get(&provider) {
            Some(k) => k,
            None => return Err(format!("{provider}: no key provided")),
        },
    };
    match provider.as_str() {
        "glm" => crate::providers::glm::test_key(&actual).await,
        "minimax" => {
            let stored = get_region("minimax");
            let r = region
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(stored.as_deref())
                .unwrap_or("overseas");
            crate::providers::minimax::test_key(&actual, r).await
        }
        "openrouter" => crate::providers::openrouter::test_key(&actual).await,
        other => Ok(format!(
            "{other}: key present (live probe N/A for this provider yet)"
        )),
    }
}

#[tauri::command]
pub async fn refresh_now(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    do_refresh(&app, &state).await;
    Ok(())
}

/// Re-fetch usage for one provider only (overlay popup refresh button).
/// Returns whether the fetch was healthy plus a short status message so the
/// UI can show network / rate-limit / auth failures instead of always "Updated".
#[tauri::command]
pub async fn refresh_provider(
    app: tauri::AppHandle,
    provider: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<RefreshProviderResult, String> {
    do_refresh_provider(&app, &state, provider.trim()).await
}

/// Structured provider statuses for the Settings window. Surfaces whether a
/// provider is actually registered, configured, and currently hidden, plus
/// any per-provider state the UI needs (region, last-fetch error reason).
#[tauri::command]
pub async fn get_status(
    state: tauri::State<'_, crate::AppState>,
) -> Result<Vec<ProviderStatus>, String> {
    let mut statuses = provider_statuses(&state.health).await;
    let snapshots = state.snapshots.read().await;
    apply_live_auth_failures(&mut statuses, &snapshots);
    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{ProviderHealth, UsageWindow};
    use chrono::Utc;

    fn oauth_status(id: &'static str) -> ProviderStatus {
        ProviderStatus {
            id,
            label: "OAuth",
            needs_key: false,
            has_region: false,
            registered: true,
            configured: true,
            eligible: true,
            health: ProviderHealth::Healthy,
            health_reason: None,
            hidden: false,
            region: None,
            unavailable_reason: None,
        }
    }

    #[test]
    fn live_auth_failure_marks_oauth_status_not_configured() {
        let mut statuses = vec![oauth_status("grok"), oauth_status("codex")];
        let mut snapshots = HashMap::new();
        snapshots.insert(
            "grok".to_string(),
            UsageSnapshot {
                provider: "Grok".into(),
                level: None,
                windows: vec![UsageWindow {
                    label: "weekly".into(),
                    used_percent: 0.0,
                    reset_at: None,
                    bar_visible: true,
                    is_unlimited: false,
                    used_absolute: None,
                    limit_absolute: None,
                }],
                unavailable_reason: Some("session expired — run grok login".into()),
                fetched_at: Utc::now(),
            },
        );

        apply_live_auth_failures(&mut statuses, &snapshots);

        assert!(!statuses[0].configured);
        assert_eq!(
            statuses[0].unavailable_reason.as_deref(),
            Some("session expired — run grok login")
        );
        assert!(statuses[1].configured);
    }

    #[test]
    fn transient_network_failure_does_not_turn_a_signed_in_badge_red() {
        let mut statuses = vec![oauth_status("grok")];
        let mut snapshots = HashMap::new();
        snapshots.insert(
            "grok".to_string(),
            UsageSnapshot::unavailable("Grok", "network error: timeout"),
        );

        apply_live_auth_failures(&mut statuses, &snapshots);

        assert!(statuses[0].configured);
        assert_eq!(statuses[0].unavailable_reason, None);
    }

    #[test]
    fn visibility_eligibility_rejects_missing_or_invalid_details() {
        assert!(!visibility_eligible(true, false, &ProviderHealth::Healthy));
        assert!(!visibility_eligible(
            true,
            true,
            &ProviderHealth::InvalidCredentials
        ));
        assert!(!visibility_eligible(
            true,
            true,
            &ProviderHealth::NoUsableDetails
        ));
    }

    #[test]
    fn visibility_eligibility_keeps_transient_failures_available() {
        assert!(visibility_eligible(
            true,
            true,
            &ProviderHealth::TransientFailure
        ));
        assert!(visibility_eligible(true, true, &ProviderHealth::Healthy));
    }
}

/// Notify every webview that provider visibility (or its snapshot set) changed
/// so the minibar re-pulls without waiting for the 5s poll interval.
fn emit_provider_visibility_changed(app: &tauri::AppHandle) {
    use tauri::Emitter;
    if let Err(e) = app.emit("provider-visibility-changed", ()) {
        log::warn!("set_provider_hidden: emit failed: {e}");
    }
}

/// Toggle whether a provider is hidden from the overlay. Saved keys and
/// region config are preserved.
///
/// Hide: only flips the flag. `collect` filters hidden ids, so the next
/// `get_usage` drops the card immediately while last-good data stays in
/// memory for a fast re-show (the scheduler prunes it on the next cycle).
///
/// Show: if a last-good snapshot is still cached, broadcast immediately so
/// the card paints before the network round-trip; then refresh only this
/// provider (not every provider) and broadcast again with fresh data.
#[tauri::command]
pub async fn set_provider_hidden(
    app: tauri::AppHandle,
    provider: String,
    hidden: bool,
    state: tauri::State<'_, crate::AppState>,
) -> Result<(), String> {
    if !hidden {
        let statuses = provider_statuses(&state.health).await;
        let eligible = statuses
            .iter()
            .find(|status| status.id == provider)
            .map(|status| status.eligible)
            .unwrap_or(false);
        if !eligible {
            return Err(format!("{provider}: provider has no valid usage details"));
        }
    }
    set_hidden(&provider, hidden).map_err(|e| e.to_string())?;
    if hidden {
        // Bar drops on the next pull via collect()'s is_hidden filter.
        emit_provider_visibility_changed(&app);
        return Ok(());
    }

    let has_snap = {
        let guard = state.snapshots.read().await;
        guard.contains_key(&provider)
    };
    // Instant paint from last-good when the user re-enables within a session.
    if has_snap {
        emit_provider_visibility_changed(&app);
    }
    // Single-provider fetch only — full do_refresh made "Overlay on" lag.
    if let Err(e) = do_refresh_provider(&app, &state, &provider).await {
        log::warn!("set_provider_hidden: unhide refresh failed: {e}");
    }
    emit_provider_visibility_changed(&app);
    Ok(())
}

/// Persist the overlay bar's last position so the next open restores it.
/// Called by the frontend after a native drag settles. Reads the overlay
/// window's current geometry and normalizes it to the preallocated footprint
/// (see `persist_overlay_position`) so the bar doesn't drift between sessions.
#[tauri::command]
pub async fn save_overlay_position(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    crate::overlay::persist_overlay_position(&win);
    Ok(())
}

/// Read the current OS-level autostart state. The frontend uses this to keep
/// its toggle in sync with reality — directly querying the plugin avoids any
/// drift with the on-disk config.
#[tauri::command]
pub async fn get_autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    mgr.is_enabled().map_err(|e| e.to_string())
}

/// Flip the OS-level autostart setting on/off. Logs the change so the next
/// session can confirm it stuck.
#[tauri::command]
pub async fn set_autostart_enabled(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())?;
    } else {
        mgr.disable().map_err(|e| e.to_string())?;
    }
    log::info!("set_autostart_enabled: {enabled}");
    Ok(())
}

/// Read the persisted refresh interval (seconds). The Settings panel uses
/// this to initialise the dropdown on mount.
#[tauri::command]
pub async fn get_refresh_interval() -> Result<u64, String> {
    Ok(read_refresh_interval())
}

/// Persist the refresh interval (seconds) and broadcast a
/// `refresh-interval-changed` event. The scheduler reads the interval from
/// config on each tick, so the new value takes effect on the next cycle
/// without a restart.
#[tauri::command]
pub async fn set_refresh_interval(app: tauri::AppHandle, secs: u64) -> Result<(), String> {
    write_refresh_interval(secs).map_err(|e| {
        log::error!("set_refresh_interval: save failed: {e}");
        e.to_string()
    })?;
    use tauri::Emitter;
    if let Err(e) = app.emit("refresh-interval-changed", secs) {
        log::warn!("set_refresh_interval: emit failed: {e}");
    }
    Ok(())
}

/// Whether desktop notifications for quota threshold crossings (below the red
/// line / exhausted) are enabled. The Settings panel toggle binds to this.
#[tauri::command]
pub fn get_notifications_enabled() -> bool {
    crate::secrets::get_notifications_enabled()
}

/// Persist the desktop-notification toggle. The value is read from config at
/// dispatch time, so the change takes effect on the next quota event without
/// a restart.
#[tauri::command]
pub fn set_notifications_enabled(enabled: bool) -> Result<(), String> {
    crate::secrets::set_notifications_enabled(enabled).map_err(|e| {
        log::error!("set_notifications_enabled: save failed: {e}");
        e.to_string()
    })
}

/// Hide the overlay (and Settings if open) without quitting. App stays in the
/// system tray; open again from the tray icon.
#[tauri::command]
pub async fn hide_to_tray(app: tauri::AppHandle) -> Result<(), String> {
    crate::overlay::hide_to_tray(&app);
    Ok(())
}

/// Exit the process after destroying WebView windows (cleaner than raw exit).
#[tauri::command]
pub async fn quit_app(app: tauri::AppHandle) -> Result<(), String> {
    crate::overlay::quit_cleanly(&app);
    Ok(())
}

// ============================================================
// In-app OAuth (Grok / Codex / Claude)
// ============================================================

#[tauri::command]
pub async fn start_oauth_login(provider: String) -> Result<crate::oauth::OAuthStart, String> {
    crate::oauth::start_login(&provider).await
}

/// Open an external https URL in the system default browser. Reuses the OAuth
/// opener's https-only validation so the webview can never be directed at
/// file:/// or other dangerous schemes. Used by in-app links (e.g. the GitHub
/// link on the Updates card) since WebView2 ignores `<a target="_blank">`.
#[tauri::command]
pub async fn open_external(url: String) -> Result<(), String> {
    crate::oauth::open_browser(&url)
}

#[tauri::command]
pub async fn poll_oauth_login(session_id: String) -> Result<crate::oauth::OAuthPoll, String> {
    crate::oauth::poll_login(&session_id).await
}

/// Restore an in-progress (or just-finished) OAuth session after Settings remounts.
#[tauri::command]
pub async fn get_active_oauth(
    provider: String,
) -> Result<Option<crate::oauth::OAuthStart>, String> {
    Ok(crate::oauth::active_for_provider(&provider))
}

#[tauri::command]
pub async fn list_active_oauth() -> Result<Vec<crate::oauth::OAuthStart>, String> {
    Ok(crate::oauth::list_active())
}

#[tauri::command]
pub async fn complete_oauth_login(
    session_id: String,
    code: String,
) -> Result<crate::oauth::OAuthPoll, String> {
    crate::oauth::complete_login(&session_id, &code).await
}

#[tauri::command]
pub async fn cancel_oauth_login(session_id: String) -> Result<(), String> {
    crate::oauth::cancel_login(&session_id)
}

/// Clear local OAuth tokens for a provider, then refresh snapshots.
#[tauri::command]
pub async fn oauth_logout(
    app: tauri::AppHandle,
    provider: String,
    state: tauri::State<'_, crate::AppState>,
) -> Result<String, String> {
    let msg = crate::oauth::logout(&provider)?;
    do_refresh(&app, &state).await;
    Ok(msg)
}

/// Toggle the overlay's click-through extended style. When `click_through`
/// is true the overlay stops absorbing mouse events so snipping tools,
/// drawing apps, and screen markers can operate over it. The frontend pairs
/// this with hover detection so the bar remains interactive on mouseenter.
#[tauri::command]
pub async fn set_overlay_click_through(
    app: tauri::AppHandle,
    click_through: bool,
) -> Result<(), String> {
    use tauri::Manager;
    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    crate::win32::set_click_through(&win, click_through)
}

/// Reapply the overlay's borderless Win32 style after the frontend first
/// shows the initially-hidden transparent window. The builder flag can be
/// overwritten while WebView2 finishes creating its native frame.
#[tauri::command]
pub async fn enforce_overlay_borderless(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    crate::win32::enforce_borderless(&win)
}

/// Tell the click-through / hit-test layer how tall the interactive content
/// strip is (logical px, measured from the bottom of the overlay). The window
/// itself may be taller (pre-sized to the tallest popup); only this bottom
/// strip should capture mouse input. Pass the bar height when collapsed, or
/// bar + gap + popup when a card is open.
///
/// On Windows this also reshapes the HWND via `SetWindowRgn` so empty space
/// above the bar is not part of the window for hit-testing — clicks fall
/// through to apps beneath without relying on the WS_EX_TRANSPARENT poll alone.
#[tauri::command]
pub async fn set_overlay_hit_height(
    app: tauri::AppHandle,
    height: f64,
    width: Option<f64>,
) -> Result<(), String> {
    use tauri::Manager;
    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    let scale = win.scale_factor().map_err(|e| e.to_string())?;
    let physical = (height.max(0.0) * scale).round() as i32;
    let physical_width = width
        .map(|value| (value.max(0.0) * scale).round() as i32)
        .unwrap_or(0);
    crate::win32::set_content_hit_size(&win, physical_width, physical)
}

/// Snap the overlay bar fully inside the monitor work area (the desktop minus
/// the taskbar). Called from the frontend after a drag so a bar parked on the
/// taskbar — where Windows draws the taskbar over it — lifts back into view.
#[tauri::command]
pub async fn clamp_overlay_position(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    crate::win32::clamp_into_work_area(&win)?;
    Ok(())
}

/// Atomically expand or collapse the overlay window. Keeps the bar's bottom
/// edge fixed and grows the window upward (or shrinks downward) via a single
/// Win32 `SetWindowPos` call so there's no flicker between the two halves of a
/// move+resize. The caller passes `popup_height` (logical px) measured from the
/// rendered DOM so the window always fits the content regardless of font/CSS.
///
/// `bar_height` is the bar's measured DOM height (logical px) and lets
/// Standard (36px) and Compact (30px) views share this entry point. If
/// omitted, the legacy 36px constant is used.
#[tauri::command]
pub async fn set_overlay_geometry(
    app: tauri::AppHandle,
    expanded: bool,
    popup_height: Option<f64>,
    bar_height: Option<f64>,
) -> Result<(), String> {
    use tauri::Manager;
    // Match `.bar { height }` in styles.css (POPUP_GAP matches `.popup
    // { margin-bottom }`). Frontend passes the measured bar height.
    const DEFAULT_BAR_H_LOGICAL: f64 = 24.0;
    const POPUP_GAP_LOGICAL: f64 = 6.0;
    const DEFAULT_POPUP_H_LOGICAL: f64 = 286.0;

    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    let scale = win.scale_factor().map_err(|e| e.to_string())?;

    // Anchor: the bar's bottom edge, but corrected to the work area first. If
    // the user parked the bar on the taskbar, this lifts it so the popup grows
    // into visible space instead of from under the taskbar.
    let (cx, cy, cur_w, ch) = crate::win32::clamped_rect(&win)?;
    let bottom = cy + ch;

    let popup_h_logical = popup_height.unwrap_or(DEFAULT_POPUP_H_LOGICAL);
    // Accept any reasonable DOM-measured bar height; clamp to the supported
    // 24..=48 logical px range so a broken measurement can never collapse
    // the window or blow it up. Defaults to 36 when not provided (legacy
    // callers, first paint before the frontend has measured).
    let bar_h_logical = bar_height
        .unwrap_or(DEFAULT_BAR_H_LOGICAL)
        .clamp(24.0, 48.0);
    let target_h_logical = if expanded {
        bar_h_logical + POPUP_GAP_LOGICAL + popup_h_logical
    } else {
        bar_h_logical
    };
    let target_h_physical = (target_h_logical * scale) as i32;
    // Preserve width (managed by `set_overlay_width`). Re-read immediately
    // before apply so a concurrent width update isn't clobbered by a stale
    // `cur_w` captured earlier in this command — that race was clipping the
    // bar's refresh/settings buttons on first startup.
    let (_, _, latest_w, _) = crate::win32::clamped_rect(&win).unwrap_or((cx, cy, cur_w, ch));
    let target_w_physical = latest_w.max(cur_w);
    let target_y = bottom - target_h_physical;
    let target_x = cx;

    // When expanding, claim the full new window height as the hit strip
    // *before* SetWindowPos re-applies the region. Otherwise the region
    // stays at the collapsed bar height and only a thin bottom sliver of
    // the settings/provider popup is visible (the rest is clipped away).
    if expanded {
        crate::win32::store_content_hit_height(target_h_physical);
    }

    crate::win32::set_window_rect_by_label(
        &app,
        crate::overlay::OVERLAY_LABEL,
        target_x,
        target_y,
        target_w_physical,
        target_h_physical,
    )
}

/// Set the overlay window width to fit the status-bar content. Called from the
/// frontend after each snapshot render (and on ResizeObserver reflows). The bar
/// grows/shrinks leftward (right edge anchored where possible) while the final
/// native rectangle stays inside its active monitor work area.
#[tauri::command]
pub async fn set_overlay_width(app: tauri::AppHandle, width: Option<f64>) -> Result<(), String> {
    use tauri::Manager;
    // Floor only needs to fit the chrome (drag + refresh + settings) with no
    // cards. Ceiling is high so many providers (and future trackers) can sit
    // on the bar without clipping; work-area clamp still applies.
    const MIN_BAR_W: f64 = 100.0;
    const MAX_BAR_W: f64 = 1600.0;

    let win = app
        .get_webview_window(crate::overlay::OVERLAY_LABEL)
        .ok_or("overlay window not open")?;
    let scale = win.scale_factor().map_err(|e| e.to_string())?;
    let pos = win.outer_position().map_err(|e| e.to_string())?;
    let size = win.outer_size().map_err(|e| e.to_string())?;
    let work = crate::win32::work_area(&win)?;

    // Default matches the preallocated HWND width (see overlay.rs).
    let new_w_logical = width.unwrap_or(800.0).clamp(MIN_BAR_W, MAX_BAR_W);
    let requested_w_physical = (new_w_logical * scale).round() as i32;
    let (new_x, new_w_physical) = crate::win32::clamp_right_anchored_width(
        pos.x,
        size.width as i32,
        requested_w_physical,
        work.left,
        work.right,
    );

    crate::win32::set_window_rect_by_label(
        &app,
        crate::overlay::OVERLAY_LABEL,
        new_x,
        pos.y,
        new_w_physical,
        size.height as i32,
    )
}
