use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::secrets::UpdateChannel;

const CHECK_INTERVAL: Duration = Duration::hours(24);
const UPDATE_EVENT: &str = "update-state-changed";
const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/GimpMan/AI-Usage-Tracker/releases?per_page=100";
const GITHUB_USER_AGENT: &str = "AI-Usage-Tracker";
const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/GimpMan/AI-Usage-Tracker/releases/download";

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Installing,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateState {
    pub phase: UpdatePhase,
    pub current_version: String,
    pub available_version: Option<String>,
    pub notes: Option<String>,
    pub published_at: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
    pub channel: UpdateChannel,
}

impl Default for UpdateState {
    fn default() -> Self {
        let cfg = crate::secrets::load_app_config();
        Self {
            phase: UpdatePhase::Idle,
            current_version: env!("CARGO_PKG_VERSION").into(),
            available_version: None,
            notes: None,
            published_at: None,
            last_checked_at: cfg.last_update_check_at,
            downloaded_bytes: 0,
            total_bytes: None,
            error: None,
            channel: cfg.update_channel,
        }
    }
}

pub struct UpdateManager {
    state: RwLock<UpdateState>,
    operation: Mutex<()>,
}

impl Default for UpdateManager {
    fn default() -> Self {
        Self {
            state: RwLock::new(UpdateState::default()),
            operation: Mutex::new(()),
        }
    }
}

pub fn should_check(manual: bool, last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    manual
        || last
            .map(|t| now.signed_duration_since(t) >= CHECK_INTERVAL)
            .unwrap_or(true)
}

#[cfg(test)]
fn is_newer_stable(current: &str, candidate: &str) -> bool {
    is_acceptable_update(current, candidate, UpdateChannel::Stable)
}

fn is_acceptable_update(current: &str, candidate: &str, channel: UpdateChannel) -> bool {
    let (Ok(current), Ok(candidate)) = (
        semver::Version::parse(current),
        semver::Version::parse(candidate),
    ) else {
        return false;
    };
    if candidate <= current {
        return false;
    }
    match channel {
        UpdateChannel::Stable => candidate.pre.is_empty(),
        // The GitHub release feed was already filtered by `release_eligible`.
        // A GitHub prerelease can legitimately use a core SemVer version, so
        // do not reject that selected candidate merely because it lacks `-rc`.
        UpdateChannel::Prerelease => true,
    }
}

fn next_progress(
    downloaded: u64,
    current_total: Option<u64>,
    chunk: usize,
    total: Option<u64>,
) -> (u64, Option<u64>) {
    (
        downloaded.saturating_add(chunk as u64),
        total.or(current_total),
    )
}

fn user_safe_error(_: &str) -> String {
    "Unable to check for updates. Please try again later.".into()
}

fn install_safe_error(technical: &str) -> String {
    let lower = technical.to_ascii_lowercase();
    if lower.contains("signature") || lower.contains("minisign") || lower.contains("verification") {
        "The update could not be installed because its security signature could not be verified."
            .into()
    } else {
        "Unable to install the update. Please try again later.".into()
    }
}

fn log_safe_error(_: &str) -> &'static str {
    "updater operation failed; details suppressed"
}

fn apply_successful_recheck(state: &mut UpdateState, checked: DateTime<Utc>) {
    state.last_checked_at = Some(checked);
}

fn apply_check_error(state: &mut UpdateState, safe: String) {
    state.phase = UpdatePhase::Error;
    state.error = Some(safe);
}

fn restore_after_automatic_error(state: &mut UpdateState, previous: &UpdateState) {
    *state = previous.clone();
}

/// Minimal release summary used for channel selection (tests + GitHub parse).
#[derive(Clone, Debug)]
struct GithubReleaseSummary {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    asset_names: Vec<String>,
}

#[derive(Deserialize)]
struct GithubReleaseApi {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAssetApi>,
}

#[derive(Deserialize)]
struct GithubAssetApi {
    name: String,
}

fn parse_tag_version(tag: &str) -> Option<semver::Version> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(stripped).ok()
}

fn has_exactly_one_latest_json(asset_names: &[String]) -> bool {
    asset_names
        .iter()
        .filter(|n| n.as_str() == "latest.json")
        .count()
        == 1
}

fn release_eligible(
    release: &GithubReleaseSummary,
    channel: UpdateChannel,
) -> Option<semver::Version> {
    if release.draft || !has_exactly_one_latest_json(&release.asset_names) {
        return None;
    }
    let version = parse_tag_version(&release.tag_name)?;
    let is_prerelease = release.prerelease || !version.pre.is_empty();
    match channel {
        UpdateChannel::Stable if is_prerelease => return None,
        UpdateChannel::Prerelease if !is_prerelease => return None,
        _ => {}
    }
    Some(version)
}

fn latest_json_endpoint_for_tag(tag: &str) -> String {
    format!("{RELEASE_DOWNLOAD_BASE}/{tag}/latest.json")
}

fn select_latest_json_endpoint(
    releases: &[GithubReleaseSummary],
    channel: UpdateChannel,
) -> Option<String> {
    let mut best: Option<(semver::Version, &str)> = None;
    for release in releases {
        let Some(version) = release_eligible(release, channel) else {
            continue;
        };
        match &best {
            None => best = Some((version, release.tag_name.as_str())),
            Some((best_ver, _)) if version > *best_ver => {
                best = Some((version, release.tag_name.as_str()));
            }
            _ => {}
        }
    }
    best.map(|(_, tag)| latest_json_endpoint_for_tag(tag))
}

async fn fetch_github_releases() -> Result<Vec<GithubReleaseSummary>, String> {
    let client = reqwest::Client::builder()
        .user_agent(GITHUB_USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let releases: Vec<GithubReleaseApi> = client
        .get(GITHUB_RELEASES_URL)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(releases
        .into_iter()
        .map(|r| GithubReleaseSummary {
            tag_name: r.tag_name,
            draft: r.draft,
            prerelease: r.prerelease,
            asset_names: r.assets.into_iter().map(|a| a.name).collect(),
        })
        .collect())
}

async fn resolve_channel_endpoint(channel: UpdateChannel) -> Result<Option<String>, String> {
    let releases = fetch_github_releases().await?;
    Ok(select_latest_json_endpoint(&releases, channel))
}

async fn run_updater_check(
    app: &AppHandle,
    endpoint: &str,
) -> Result<Option<tauri_plugin_updater::Update>, String> {
    let url = Url::parse(endpoint).map_err(|e| e.to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    updater.check().await.map_err(|e| e.to_string())
}

impl UpdateManager {
    pub async fn state(&self) -> UpdateState {
        self.state.read().await.clone()
    }

    async fn transition(&self, app: &AppHandle, mutate: impl FnOnce(&mut UpdateState)) {
        let snapshot = {
            let mut state = self.state.write().await;
            mutate(&mut state);
            state.clone()
        };
        if let Err(error) = app.emit(UPDATE_EVENT, snapshot) {
            log::warn!("update event emit failed: {error}");
        }
    }

    fn transition_blocking(&self, app: &AppHandle, mutate: impl FnOnce(&mut UpdateState)) {
        let snapshot = tokio::task::block_in_place(|| {
            tauri::async_runtime::block_on(async {
                let mut state = self.state.write().await;
                mutate(&mut state);
                state.clone()
            })
        });
        if let Err(error) = app.emit(UPDATE_EVENT, snapshot) {
            log::warn!("update event emit failed: {error}");
        }
    }

    pub async fn check(&self, app: &AppHandle, manual: bool) -> Result<UpdateState, String> {
        let Ok(_operation) = self.operation.try_lock() else {
            return Ok(self.state().await);
        };
        self.check_locked(app, manual).await
    }

    pub async fn set_channel(
        &self,
        app: &AppHandle,
        channel: UpdateChannel,
    ) -> Result<UpdateState, String> {
        let Ok(_operation) = self.operation.try_lock() else {
            return Err("An update operation is already in progress.".to_string());
        };
        crate::secrets::set_update_channel(channel).map_err(|e| {
            log::error!("persist update channel failed: {e}");
            "Unable to save update channel. Please try again.".to_string()
        })?;
        self.transition(app, |state| {
            state.channel = channel;
            state.available_version = None;
            state.notes = None;
            state.published_at = None;
            state.downloaded_bytes = 0;
            state.total_bytes = None;
            state.error = None;
            if matches!(
                state.phase,
                UpdatePhase::Available | UpdatePhase::UpToDate | UpdatePhase::Error
            ) {
                state.phase = UpdatePhase::Idle;
            }
        })
        .await;
        // Channel is already persisted and applied. A recheck failure must not
        // reject the command; check_locked still sets Error-phase + safe message.
        let _ = self.check_locked(app, true).await;
        Ok(self.state().await)
    }

    async fn check_locked(&self, app: &AppHandle, manual: bool) -> Result<UpdateState, String> {
        let now = Utc::now();
        let last = self.state.read().await.last_checked_at;
        if !should_check(manual, last, now) {
            return Ok(self.state().await);
        }
        let previous = self.state().await;
        let channel = previous.channel;
        self.transition(app, |s| {
            s.phase = UpdatePhase::Checking;
            s.error = None;
        })
        .await;
        let result = async {
            match resolve_channel_endpoint(channel).await? {
                None => Ok(None),
                Some(endpoint) => run_updater_check(app, &endpoint).await,
            }
        }
        .await;
        match result {
            Ok(update) => {
                let checked = Utc::now();
                if let Err(error) = crate::secrets::set_last_update_check_at(checked) {
                    log::error!("persist update check timestamp failed: {error}");
                }
                let current = env!("CARGO_PKG_VERSION");
                self.transition(app, |state| {
                    state.last_checked_at = Some(checked);
                    state.downloaded_bytes = 0;
                    state.total_bytes = None;
                    state.error = None;
                    if let Some(update) =
                        update.filter(|u| is_acceptable_update(current, &u.version, channel))
                    {
                        state.phase = UpdatePhase::Available;
                        state.available_version = Some(update.version);
                        state.notes = update.body;
                        state.published_at = update.date.map(|d| d.to_string());
                    } else {
                        state.phase = UpdatePhase::UpToDate;
                        state.available_version = None;
                        state.notes = None;
                        state.published_at = None;
                    }
                })
                .await;
                Ok(self.state().await)
            }
            Err(technical) => {
                log::error!("{}", log_safe_error(&technical));
                let safe = user_safe_error(&technical);
                if manual {
                    self.transition(app, |state| apply_check_error(state, safe.clone()))
                        .await;
                } else {
                    self.transition(app, |state| restore_after_automatic_error(state, &previous))
                        .await;
                }
                Err(safe)
            }
        }
    }

    pub async fn install(&self, app: &AppHandle) -> Result<(), String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "An update operation is already in progress.".to_string())?;
        let (expected, channel) = {
            let state = self.state.read().await;
            (
                state
                    .available_version
                    .clone()
                    .ok_or_else(|| "No update is available.".to_string())?,
                state.channel,
            )
        };
        self.transition(app, |s| {
            s.phase = UpdatePhase::Checking;
            s.error = None;
        })
        .await;
        let recheck = async {
            match resolve_channel_endpoint(channel).await? {
                None => Ok(None),
                Some(endpoint) => run_updater_check(app, &endpoint).await,
            }
        }
        .await;
        let candidate = match recheck {
            Ok(candidate) => candidate,
            Err(technical) => {
                log::error!("{}", log_safe_error(&technical));
                let safe = user_safe_error(&technical);
                self.transition(app, |state| apply_check_error(state, safe.clone()))
                    .await;
                return Err(safe);
            }
        };
        let checked = Utc::now();
        self.transition(app, |state| apply_successful_recheck(state, checked))
            .await;
        if let Err(error) = crate::secrets::set_last_update_check_at(checked) {
            log::error!("persist update install recheck timestamp failed: {error}");
        }
        let Some(update) = candidate.filter(|update| {
            is_acceptable_update(env!("CARGO_PKG_VERSION"), &update.version, channel)
                && update.version == expected
        }) else {
            let safe = "The available update changed. Please check again.".to_string();
            self.transition(app, |state| {
                state.phase = UpdatePhase::Error;
                state.available_version = None;
                state.error = Some(safe.clone());
            })
            .await;
            return Err(safe);
        };
        self.transition(app, |s| {
            s.phase = UpdatePhase::Downloading;
            s.downloaded_bytes = 0;
            s.total_bytes = None;
        })
        .await;
        let app_progress = app.clone();
        let app_finished = app.clone();
        let install_result = update
            .download_and_install(
                |chunk, total| {
                    self.transition_blocking(&app_progress, |state| {
                        (state.downloaded_bytes, state.total_bytes) =
                            next_progress(state.downloaded_bytes, state.total_bytes, chunk, total);
                    });
                },
                || {
                    self.transition_blocking(&app_finished, |state| {
                        state.phase = UpdatePhase::Installing;
                    });
                },
            )
            .await;
        if let Err(technical) = install_result {
            log::error!("{}", log_safe_error(&technical.to_string()));
            let safe = install_safe_error(&technical.to_string());
            self.transition(app, |s| {
                s.phase = UpdatePhase::Error;
                s.error = Some(safe.clone());
            })
            .await;
            return Err(safe);
        }
        app.request_restart();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    #[test]
    fn automatic_check_is_throttled_before_24_hours_but_allowed_at_boundary() {
        let now = Utc.with_ymd_and_hms(2026, 7, 13, 12, 0, 0).unwrap();
        assert!(!should_check(false, Some(now - Duration::hours(23)), now));
        assert!(should_check(false, Some(now - Duration::hours(24)), now));
    }

    #[test]
    fn manual_check_bypasses_throttle() {
        let now = Utc::now();
        assert!(should_check(true, Some(now), now));
    }

    #[test]
    fn failed_state_transition_preserves_success_timestamp() {
        let old = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 13, 0, 0, 0).unwrap();
        let mut state = UpdateState::default();
        state.last_checked_at = Some(old);
        apply_check_error(&mut state, "safe".into());
        assert_eq!(state.last_checked_at, Some(old));
        apply_successful_recheck(&mut state, now);
        assert_eq!(state.last_checked_at, Some(now));
    }

    #[test]
    fn accepts_only_stable_newer_versions() {
        assert!(is_newer_stable("1.2.2", "1.2.3"));
        assert!(!is_newer_stable("1.2.2", "1.2.2"));
        assert!(!is_newer_stable("1.2.2", "1.2.1"));
        assert!(!is_newer_stable("1.2.2", "1.3.0-beta.1"));
        assert!(!is_newer_stable("1.2.2", "not-semver"));
    }

    #[test]
    fn progress_supports_known_and_unknown_totals() {
        assert_eq!(next_progress(10, Some(100), 25, Some(100)), (35, Some(100)));
        assert_eq!(next_progress(10, None, 5, None), (15, None));
    }

    #[test]
    fn technical_errors_are_mapped_to_safe_messages() {
        let safe = user_safe_error("request failed with token SECRET at https://private/path");
        assert_eq!(safe, "Unable to check for updates. Please try again later.");
        assert!(!safe.contains("SECRET"));
    }

    #[test]
    fn log_safe_error_excludes_credentials_and_private_urls() {
        let safe =
            log_safe_error("request https://user:SECRET@private.example/path?token=SECRET failed");
        assert!(!safe.contains("SECRET"));
        assert!(!safe.contains("private.example"));
        assert!(!safe.contains("token="));
    }

    #[tokio::test]
    async fn manager_operation_guard_serializes_access() {
        let manager = UpdateManager::default();
        let first = manager.operation.lock().await;
        assert!(manager.operation.try_lock().is_err());
        drop(first);
        assert!(manager.operation.try_lock().is_ok());
    }

    #[tokio::test]
    async fn simultaneous_callers_do_not_queue() {
        let manager = UpdateManager::default();
        let first = manager.operation.lock().await;
        assert!(manager.operation.try_lock().is_err());
        assert!(manager.operation.try_lock().is_err());
        drop(first);
    }

    #[test]
    fn automatic_failure_restores_complete_previous_state() {
        let mut previous = UpdateState::default();
        previous.phase = UpdatePhase::Available;
        previous.available_version = Some("9.0.0".into());
        let mut state = previous.clone();
        state.phase = UpdatePhase::Checking;
        restore_after_automatic_error(&mut state, &previous);
        assert_eq!(state.phase, UpdatePhase::Available);
        assert_eq!(state.available_version.as_deref(), Some("9.0.0"));
        assert_eq!(state.error, None);
    }

    #[test]
    fn signature_failures_have_a_dedicated_safe_message() {
        let safe = install_safe_error("minisign signature verification failed: SECRET");
        assert!(safe.contains("security signature"));
        assert!(!safe.contains("SECRET"));
        assert_eq!(
            install_safe_error("network broke"),
            "Unable to install the update. Please try again later."
        );
    }

    #[test]
    fn successful_recheck_updates_timestamp_before_download_state() {
        let checked = Utc::now();
        let mut state = UpdateState::default();
        apply_successful_recheck(&mut state, checked);
        assert_eq!(state.last_checked_at, Some(checked));
    }

    // --- Update channel selection ---

    fn release(
        tag: &str,
        draft: bool,
        prerelease: bool,
        latest_json_count: usize,
    ) -> GithubReleaseSummary {
        let mut asset_names = Vec::new();
        for _ in 0..latest_json_count {
            asset_names.push("latest.json".into());
        }
        GithubReleaseSummary {
            tag_name: tag.into(),
            draft,
            prerelease,
            asset_names,
        }
    }

    fn endpoint(tag: &str) -> String {
        format!("https://github.com/GimpMan/AI-Usage-Tracker/releases/download/{tag}/latest.json")
    }

    #[test]
    fn update_channel_serializes_exactly_stable_and_prerelease() {
        assert_eq!(
            serde_json::to_string(&UpdateChannel::Stable).unwrap(),
            "\"stable\""
        );
        assert_eq!(
            serde_json::to_string(&UpdateChannel::Prerelease).unwrap(),
            "\"prerelease\""
        );
        assert_eq!(
            serde_json::from_str::<UpdateChannel>("\"stable\"").unwrap(),
            UpdateChannel::Stable
        );
        assert_eq!(
            serde_json::from_str::<UpdateChannel>("\"prerelease\"").unwrap(),
            UpdateChannel::Prerelease
        );
    }

    #[test]
    fn update_state_exposes_active_channel() {
        let stable = UpdateState {
            channel: UpdateChannel::Stable,
            ..UpdateState::default()
        };
        let prerelease = UpdateState {
            channel: UpdateChannel::Prerelease,
            ..UpdateState::default()
        };
        let stable_json = serde_json::to_value(&stable).unwrap();
        let prerelease_json = serde_json::to_value(&prerelease).unwrap();
        assert_eq!(stable_json["channel"], "stable");
        assert_eq!(prerelease_json["channel"], "prerelease");
    }

    #[test]
    fn stable_channel_rejects_semver_prerelease_candidates() {
        assert!(is_acceptable_update(
            "1.2.2",
            "1.2.3",
            UpdateChannel::Stable
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.2.2",
            UpdateChannel::Stable
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.2.1",
            UpdateChannel::Stable
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.3.0-beta.1",
            UpdateChannel::Stable
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "not-semver",
            UpdateChannel::Stable
        ));
    }

    #[test]
    fn prerelease_channel_accepts_newer_versions_from_the_preselected_feed() {
        assert!(is_acceptable_update(
            "1.2.2",
            "1.2.3",
            UpdateChannel::Prerelease
        ));
        assert!(is_acceptable_update(
            "1.2.2",
            "1.3.0-beta.1",
            UpdateChannel::Prerelease
        ));
        assert!(is_acceptable_update(
            "1.2.2",
            "1.2.3-rc.1",
            UpdateChannel::Prerelease
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.2.2",
            UpdateChannel::Prerelease
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.2.1",
            UpdateChannel::Prerelease
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "1.2.2-beta.1",
            UpdateChannel::Prerelease
        ));
        assert!(!is_acceptable_update(
            "1.2.2",
            "not-semver",
            UpdateChannel::Prerelease
        ));
    }

    #[test]
    fn stable_channel_selects_highest_non_prerelease_non_draft_with_single_latest_json() {
        let releases = vec![
            release("v1.4.0", false, false, 1),
            release("v1.5.0", false, true, 1), // GitHub prerelease — excluded on stable
            release("v1.3.0", false, false, 1),
            release("v1.6.0", true, false, 1),  // draft
            release("v1.4.1", false, false, 0), // no latest.json
            release("v1.4.2", false, false, 2), // multiple latest.json
            release("not-a-version", false, false, 1),
            release("v1.4.0-beta", false, false, 1), // malformed/pre tag treated via parse
        ];
        assert_eq!(
            select_latest_json_endpoint(&releases, UpdateChannel::Stable),
            Some(endpoint("v1.4.0"))
        );
    }

    #[test]
    fn prerelease_channel_excludes_stable_and_picks_highest_prerelease() {
        let releases = vec![
            release("v9.9.9", false, false, 1), // stable, highest semver — excluded on prerelease
            release("v1.4.0", false, false, 1), // stable — excluded
            release("v1.5.0", false, true, 1),
            release("v1.5.1", false, true, 1),
            release("v1.6.0", true, false, 1), // draft ignored
            release("v1.5.2", false, true, 0), // missing asset
            release("bogus", false, true, 1),
        ];
        assert_eq!(
            select_latest_json_endpoint(&releases, UpdateChannel::Prerelease),
            Some(endpoint("v1.5.1"))
        );
    }

    #[test]
    fn github_prerelease_with_core_semver_is_an_acceptable_upgrade() {
        let releases = vec![release("v0.6.6", false, true, 1)];
        assert_eq!(
            select_latest_json_endpoint(&releases, UpdateChannel::Prerelease),
            Some(endpoint("v0.6.6"))
        );
        assert!(is_acceptable_update(
            "0.6.5",
            "0.6.6",
            UpdateChannel::Prerelease
        ));
    }

    #[test]
    fn prerelease_channel_returns_none_when_only_stable_releases_exist() {
        let releases = vec![
            release("v1.0.0", false, false, 1),
            release("v1.0.1", false, false, 1),
        ];
        assert_eq!(
            select_latest_json_endpoint(&releases, UpdateChannel::Prerelease),
            None
        );
    }

    #[test]
    fn selected_endpoint_is_assets_only_download_url_for_tag() {
        let releases = vec![release("v0.5.1", false, true, 1)];
        let url =
            select_latest_json_endpoint(&releases, UpdateChannel::Prerelease).expect("endpoint");
        assert_eq!(
            url,
            "https://github.com/GimpMan/AI-Usage-Tracker/releases/download/v0.5.1/latest.json"
        );
        assert!(
            !url.contains("/releases/latest/"),
            "must not use the /releases/latest/ shortcut"
        );
    }

    #[test]
    fn channel_selection_returns_none_when_no_valid_candidates() {
        let releases = vec![
            release("v1.0.0", true, false, 1),
            release("v1.0.1", false, false, 0),
            release("v1.0.2", false, true, 1), // prerelease-only pool empty for stable
        ];
        assert_eq!(
            select_latest_json_endpoint(&releases, UpdateChannel::Stable),
            None
        );
    }
}
