use anyhow::Result;
use chrono::Utc;
use keyring::Entry;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Service name used in Windows Credential Manager.
const SERVICE: &str = "ai-usage-tracker";
static APP_CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn app_config_lock() -> &'static Mutex<()> {
    APP_CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Default, Clone)]
pub struct Secrets;

impl Secrets {
    pub fn get(&self, provider: &str) -> Option<String> {
        match Entry::new(SERVICE, provider).and_then(|e| e.get_password()) {
            Ok(s) if !s.trim().is_empty() => {
                log::info!("secrets: hit for provider={provider} ({} chars)", s.len());
                Some(s)
            }
            Ok(_) => {
                log::warn!("secrets: empty value for provider={provider}");
                None
            }
            Err(e) => {
                log::warn!("secrets: miss for provider={provider}: {e}");
                None
            }
        }
    }

    pub fn set(&self, provider: &str, key: &str) -> Result<()> {
        log::info!("secrets: writing provider={provider} ({} chars)", key.len());
        let entry = Entry::new(SERVICE, provider)?;
        entry.set_password(key)?;
        log::info!("secrets: wrote provider={provider} OK");
        Ok(())
    }

    pub fn delete(&self, provider: &str) -> Result<()> {
        match Entry::new(SERVICE, provider).and_then(|e| e.delete_credential()) {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// App-only OAuth sessions (Windows Credential Manager)
//
// Separate from CLI auth files (`~/.codex/auth.json`, etc.). Sign-in in this
// app never writes those files; the CLI keeps its own login.
//
// Windows CredMan caps each secret at 2560 UTF-16 code units (~1280 chars).
// OAuth JWT blobs often exceed that, so we split JSON across chunked entries:
//   oauth_<provider>_parts  → count
//   oauth_<provider>_0..N   → chunks
// Legacy single-entry `oauth_<provider>` is still read for older installs.
// ---------------------------------------------------------------------------

/// Safe chunk size well under the Windows CredMan UTF-16 blob limit (2560).
const OAUTH_CHUNK_CHARS: usize = 1000;
const OAUTH_MAX_CHUNKS: usize = 64;

fn oauth_legacy_name(provider: &str) -> String {
    format!("oauth_{provider}")
}

fn oauth_parts_name(provider: &str) -> String {
    format!("oauth_{provider}_parts")
}

fn oauth_chunk_name(provider: &str, index: usize) -> String {
    format!("oauth_{provider}_{index}")
}

fn oauth_tombstone_name(provider: &str) -> String {
    // After app Sign out, skip one-time CLI import so the badge does not
    // immediately flip back to "Signed in" from a leftover CLI file.
    format!("oauth_{provider}_no_import")
}

fn oauth_tombstone_active(provider: &str) -> bool {
    Secrets
        .get(&oauth_tombstone_name(provider))
        .is_some_and(|s| s == "1" || s.eq_ignore_ascii_case("true"))
}

fn clear_oauth_blobs(provider: &str) -> Result<()> {
    // Legacy single blob.
    Secrets.delete(&oauth_legacy_name(provider))?;

    // Chunked layout.
    let parts_name = oauth_parts_name(provider);
    if let Some(raw) = Secrets.get(&parts_name) {
        if let Ok(n) = raw.trim().parse::<usize>() {
            for i in 0..n.min(OAUTH_MAX_CHUNKS) {
                Secrets.delete(&oauth_chunk_name(provider, i))?;
            }
        }
    } else {
        // Best-effort sweep if the parts count entry is missing.
        for i in 0..OAUTH_MAX_CHUNKS {
            Secrets.delete(&oauth_chunk_name(provider, i))?;
        }
    }
    Secrets.delete(&parts_name)?;
    Ok(())
}

/// Load the app OAuth JSON blob for `provider` (e.g. `"codex"`, `"grok"`).
pub fn oauth_get_json(provider: &str) -> Option<serde_json::Value> {
    let raw = read_oauth_raw(provider)?;
    match serde_json::from_str(&raw) {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("oauth secrets: invalid JSON for {provider}: {e}");
            None
        }
    }
}

fn read_oauth_raw(provider: &str) -> Option<String> {
    // Prefer chunked layout.
    if let Some(parts_raw) = Secrets.get(&oauth_parts_name(provider)) {
        let n = parts_raw.trim().parse::<usize>().ok()?;
        if n == 0 || n > OAUTH_MAX_CHUNKS {
            log::warn!("oauth secrets: bad parts count for {provider}: {parts_raw}");
            return None;
        }
        let mut out = String::new();
        for i in 0..n {
            let chunk = Secrets.get(&oauth_chunk_name(provider, i))?;
            out.push_str(&chunk);
        }
        return Some(out);
    }
    // Legacy single-entry blob (short sessions / older builds).
    Secrets.get(&oauth_legacy_name(provider))
}

/// Persist the app OAuth JSON blob for `provider`.
///
/// Always uses chunked Credential Manager entries so large JWT payloads
/// (Kimi / Claude / etc.) fit under the Windows 2560 UTF-16 unit limit.
pub fn oauth_set_json(provider: &str, value: &serde_json::Value) -> Result<()> {
    let raw = serde_json::to_string(value)?;
    let chars: Vec<char> = raw.chars().collect();
    if chars.is_empty() {
        return Err(anyhow::anyhow!("oauth blob empty"));
    }
    let chunks: Vec<String> = chars
        .chunks(OAUTH_CHUNK_CHARS)
        .map(|c| c.iter().collect())
        .collect();
    if chunks.len() > OAUTH_MAX_CHUNKS {
        return Err(anyhow::anyhow!(
            "oauth blob too large ({} chunks)",
            chunks.len()
        ));
    }

    // Replace atomically-ish: clear old, write new, clear tombstone last.
    clear_oauth_blobs(provider)?;

    let secrets = Secrets;
    if let Err(e) = secrets.set(&oauth_parts_name(provider), &chunks.len().to_string()) {
        let _ = clear_oauth_blobs(provider);
        return Err(e);
    }
    for (i, chunk) in chunks.iter().enumerate() {
        if let Err(e) = secrets.set(&oauth_chunk_name(provider, i), chunk) {
            let _ = clear_oauth_blobs(provider);
            return Err(e);
        }
    }
    // Successful app session → allow future imports only if this is deleted.
    Secrets.delete(&oauth_tombstone_name(provider))?;
    log::info!(
        "oauth: stored {provider} session in Credential Manager ({} chunks, {} chars)",
        chunks.len(),
        chars.len()
    );
    Ok(())
}

/// Delete the app OAuth session and block automatic CLI re-import until the
/// user signs in again. Returns true if a session blob was present.
pub fn oauth_delete(provider: &str) -> Result<bool> {
    let present = oauth_get_json(provider).is_some();
    clear_oauth_blobs(provider)?;
    // Always set tombstone so a leftover CLI auth file does not immediately
    // re-mark the provider as signed in (that left Sign in/out stuck).
    Secrets.set(&oauth_tombstone_name(provider), "1")?;
    Ok(present)
}

/// If Credential Manager has no session yet, import once from a legacy CLI
/// auth file so upgrades keep working. Never writes back to the CLI path.
///
/// Import only succeeds when the blob can be stored in Credential Manager.
/// A failed store does **not** treat the CLI file as an app session (that
/// caused a stuck SIGNED IN badge with Sign in disabled).
pub fn oauth_get_json_or_import_file(
    provider: &str,
    legacy_path: &std::path::Path,
) -> Option<serde_json::Value> {
    if let Some(v) = oauth_get_json(provider) {
        return Some(v);
    }
    if oauth_tombstone_active(provider) {
        return None;
    }
    let raw = std::fs::read_to_string(legacy_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    // Only import non-empty objects (ignore empty `{}` placeholders).
    let non_empty = value
        .as_object()
        .map(|o| !o.is_empty())
        .unwrap_or(false);
    if !non_empty {
        return None;
    }
    match oauth_set_json(provider, &value) {
        Ok(()) => {
            log::info!(
                "oauth: imported {provider} session from {} into Credential Manager (app-only; CLI file left unchanged)",
                legacy_path.display()
            );
            Some(value)
        }
        Err(e) => {
            log::warn!(
                "oauth: failed to import {provider} into Credential Manager: {e} — use Sign in in the app"
            );
            None
        }
    }
}

/// Non-secret per-provider config (e.g. MiniMax region) lives in a small JSON
/// next to the snapshot state.
pub fn config_dir() -> Result<std::path::PathBuf> {
    let base =
        dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir on this platform"))?;
    let dir = base.join("ai-usage-tracker");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn config_path() -> Result<std::path::PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig {
    /// MiniMax region ("overseas" | "china").
    #[serde(default)]
    pub region: Option<String>,
    /// If true, hide this provider from the overlay and stop fetching it.
    /// Saved keys are preserved.
    #[serde(default)]
    pub hidden: bool,
    /// OpenRouter: last-seen `total_credits` so we can detect top-ups between
    /// fetches. `None` on first run / after a manual reset. Other providers
    /// ignore this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topup_baseline: Option<TopupBaseline>,
    /// OpenRouter's optional local account-budget baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_balance_baseline: Option<AccountBalanceBaseline>,
    #[serde(default, flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

pub type ConfigMap = std::collections::HashMap<String, ProviderConfig>;

/// Per-provider baseline used by providers whose state is not purely "used /
/// limit at a known reset". OpenRouter is the first consumer: its lifetime
/// balance grows when the user tops up, so we pin the last-seen `total_credits`
/// and treat any subsequent rise as a top-up (which resets the implied
/// "used this cycle" view).
///
/// Kept inside `ProviderConfig` rather than a separate file so it lives on the
/// same save path as region/hidden (one round-trip, no extra fs::write).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TopupBaseline {
    /// The `total_credits` value at the moment of the baseline snapshot.
    pub total_credits: f64,
    /// When the baseline was last written. Diagnostic only — the fetch logic
    /// uses `total_credits` exclusively for comparison.
    #[serde(default = "Utc::now")]
    pub saved_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AccountBalanceBaseline {
    /// The account balance to use as the local tracking budget.
    pub balance: f64,
    #[serde(default = "Utc::now")]
    pub saved_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

impl TopupBaseline {
    /// Load the baseline for a provider id (if any).
    pub fn load(provider: &str) -> Option<Self> {
        crate::secrets::load_config()
            .get(provider)
            .and_then(|c| c.topup_baseline.clone())
    }

    /// Persist a new baseline. Errors are non-fatal for the caller — the
    /// top-up detection is a nice-to-have, not a hard dependency.
    pub fn save(provider: &str, baseline: TopupBaseline) -> Result<(), String> {
        update_app_config(|config| {
            config
                .providers
                .entry(provider.to_string())
                .or_default()
                .topup_baseline = Some(baseline);
        })
        .map_err(|e| e.to_string())
    }

    /// Drop the stored baseline so the next fetch treats the current
    /// `total_credits` as a fresh baseline (i.e. forces a top-up reset).
    pub fn clear(provider: &str) -> Result<(), String> {
        update_app_config(|config| {
            if let Some(entry) = config.providers.get_mut(provider) {
                entry.topup_baseline = None;
            }
        })
        .map_err(|e| e.to_string())
    }
}

impl AccountBalanceBaseline {
    pub fn load(provider: &str) -> Option<Self> {
        crate::secrets::load_config()
            .get(provider)
            .and_then(|config| config.account_balance_baseline.clone())
    }

    pub fn save(provider: &str, baseline: Self) -> Result<(), String> {
        update_app_config(|config| {
            config
                .providers
                .entry(provider.to_string())
                .or_default()
                .account_balance_baseline = Some(baseline);
        })
        .map_err(|e| e.to_string())
    }

    pub fn clear(provider: &str) -> Result<(), String> {
        update_app_config(|config| {
            if let Some(entry) = config.providers.get_mut(provider) {
                entry.account_balance_baseline = None;
            }
        })
        .map_err(|e| e.to_string())
    }
}

/// Save both OpenRouter baselines (top-up + account balance) in a single
/// atomic config write. This eliminates the window between two separate saves
/// where a concurrent fetch could read one new baseline and one stale one.
pub fn save_openrouter_baselines(
    total_credits: f64,
    balance: f64,
) -> Result<(), String> {
    update_app_config(|config| {
        let entry = config
            .providers
            .entry("openrouter_management".to_string())
            .or_default();
        entry.topup_baseline = Some(TopupBaseline {
            total_credits,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        });
        entry.account_balance_baseline = Some(AccountBalanceBaseline {
            balance,
            saved_at: Utc::now(),
            extra: serde_json::Map::new(),
        });
    })
    .map_err(|e| e.to_string())
}

/// Preferred updater channel. Serialized exactly as `stable` / `prerelease`.
/// Legacy configs without this field default to stable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Prerelease,
}

/// Top-level app config. Wraps the per-provider map plus app-wide settings
/// (overlay position, refresh interval). Loaded with backward-compat for
/// older `ConfigMap` payloads — those are treated as an all-providers entry
/// with no overlay position and the default 60s refresh interval.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub providers: ConfigMap,
    #[serde(default)]
    pub overlay_position: Option<OverlayPosition>,
    /// Scheduler refresh interval in seconds. Defaults to 60 for new and
    /// legacy configs. The Settings panel constrains this to a safe preset
    /// list (30s–5m).
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
    /// Desktop notifications for quota threshold crossings (below the red
    /// line / exhausted). Defaults to on for new and legacy configs.
    #[serde(default = "default_notifications_enabled")]
    pub notifications_enabled: bool,
    /// Last updater response that parsed successfully. Failed checks never
    /// advance this value, so transient outages cannot suppress retries.
    #[serde(default)]
    pub last_update_check_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Preferred release channel for the in-app updater. Defaults to stable.
    #[serde(default)]
    pub update_channel: UpdateChannel,
    #[serde(default, flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn default_refresh_interval() -> u64 {
    60
}

fn default_notifications_enabled() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            providers: ConfigMap::default(),
            overlay_position: None,
            refresh_interval_secs: default_refresh_interval(),
            notifications_enabled: default_notifications_enabled(),
            last_update_check_at: None,
            update_channel: UpdateChannel::Stable,
            extra: serde_json::Map::new(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OverlayPosition {
    /// Logical pixels (DIP) on the primary monitor's work area.
    pub x: f64,
    pub y: f64,
    #[serde(default, flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

fn load_app_config_at(path: &std::path::Path) -> AppConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return AppConfig::default();
    };
    // New format
    if let Ok(cfg) = serde_json::from_str::<AppConfig>(&text) {
        return cfg;
    }
    // Legacy format: flat provider map.
    if let Ok(map) = serde_json::from_str::<ConfigMap>(&text) {
        return AppConfig {
            providers: map,
            overlay_position: None,
            refresh_interval_secs: default_refresh_interval(),
            notifications_enabled: default_notifications_enabled(),
            last_update_check_at: None,
            update_channel: UpdateChannel::Stable,
            extra: serde_json::Map::new(),
        };
    }
    AppConfig::default()
}

pub fn load_app_config() -> AppConfig {
    let _guard = app_config_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    config_path()
        .map(|path| load_app_config_at(&path))
        .unwrap_or_default()
}

fn save_app_config_at(path: &std::path::Path, cfg: &AppConfig) -> Result<()> {
    let text = serde_json::to_string_pretty(cfg)?;
    atomic_write_with_pre_replace(path, text.as_bytes(), |_| Ok(())).map_err(Into::into)
}

fn atomic_write_with_pre_replace(
    path: &std::path::Path,
    contents: &[u8],
    before_replace: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "config path has no parent",
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        before_replace(&temp)?;
        replace_file_atomically(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(windows)]
fn replace_file_atomically(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(std::io::Error::other)
}

#[cfg(not(windows))]
fn replace_file_atomically(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

fn update_app_config_at(path: &std::path::Path, mutate: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let _guard = app_config_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut config = load_app_config_at(path);
    mutate(&mut config);
    save_app_config_at(path, &config)
}

pub fn update_app_config(mutate: impl FnOnce(&mut AppConfig)) -> Result<()> {
    let path = config_path()?;
    update_app_config_at(&path, mutate)
}

pub fn set_last_update_check_at(checked: chrono::DateTime<chrono::Utc>) -> Result<()> {
    update_app_config(|config| config.last_update_check_at = Some(checked))
}

pub fn set_update_channel(channel: UpdateChannel) -> Result<()> {
    update_app_config(|config| config.update_channel = channel)
}

/// Backward-compatible accessor for the existing per-provider call sites.
pub fn load_config() -> ConfigMap {
    load_app_config().providers
}

pub fn get_region(provider: &str) -> Option<String> {
    load_config().get(provider).and_then(|c| c.region.clone())
}

pub fn set_region(provider: &str, region: &str) -> Result<()> {
    update_app_config(|config| {
        config
            .providers
            .entry(provider.to_string())
            .or_default()
            .region = Some(region.to_string());
    })
}

pub fn is_hidden(provider: &str) -> bool {
    load_config()
        .get(provider)
        .map(|c| c.hidden)
        .unwrap_or(false)
}

pub fn set_hidden(provider: &str, hidden: bool) -> Result<()> {
    update_app_config(|config| {
        config
            .providers
            .entry(provider.to_string())
            .or_default()
            .hidden = hidden
    })
}

pub fn get_overlay_position() -> Option<OverlayPosition> {
    load_app_config().overlay_position
}

pub fn set_overlay_position(pos: OverlayPosition) -> Result<()> {
    update_app_config(|config| config.overlay_position = Some(pos))
}

/// Read the persisted scheduler refresh interval (seconds). Always returns
/// a value (`60` for legacy configs and first launches).
pub fn get_refresh_interval() -> u64 {
    let cfg = load_app_config();
    let secs = cfg.refresh_interval_secs;
    if secs == 0 {
        default_refresh_interval()
    } else {
        secs
    }
}

/// Persist the scheduler refresh interval (seconds). Used by the Settings
/// panel; the scheduler reads this value on each tick so changes take
/// effect without a restart.
pub fn set_refresh_interval(secs: u64) -> Result<()> {
    update_app_config(|config| config.refresh_interval_secs = secs)
}

/// Whether desktop notifications for quota alerts are enabled. Always
/// returns a value (`true` for legacy configs and first launches).
pub fn get_notifications_enabled() -> bool {
    load_app_config().notifications_enabled
}

/// Persist the desktop-notification toggle. Read from config at dispatch
/// time, so changes take effect without a restart.
pub fn set_notifications_enabled(enabled: bool) -> Result<()> {
    update_app_config(|config| config.notifications_enabled = enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn isolated_config_path(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ai-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.join("config.json"), dir)
    }

    #[test]
    fn atomic_write_failure_before_replace_preserves_original() {
        let (path, dir) = isolated_config_path("atomic-failure");
        std::fs::write(&path, b"original").unwrap();
        let result = atomic_write_with_pre_replace(&path, b"replacement", |_| {
            Err(std::io::Error::new(std::io::ErrorKind::Other, "simulated"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_write_success_replaces_with_complete_json() {
        let (path, dir) = isolated_config_path("atomic-success");
        std::fs::write(&path, br#"{"old":true}"#).unwrap();
        let replacement = br#"{"providers":{},"refresh_interval_secs":90}"#;
        atomic_write_with_pre_replace(&path, replacement, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_top_level_and_provider_fields_survive_update() {
        let (path, dir) = isolated_config_path("unknown-fields");
        std::fs::write(
            &path,
            r#"{
            "providers":{"glm":{"hidden":false,"future_provider_setting":{"mode":"new"},
                "topup_baseline":{"total_credits":10.0,"future_baseline":"kept"}}},
            "overlay_position":{"x":1.0,"y":2.0,"future_position":"kept"},
            "future_app_setting":{"enabled":true}
        }"#,
        )
        .unwrap();
        update_app_config_at(&path, |config| config.refresh_interval_secs = 90).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["future_app_setting"]["enabled"], true);
        assert_eq!(
            value["providers"]["glm"]["future_provider_setting"]["mode"],
            "new"
        );
        assert_eq!(
            value["providers"]["glm"]["topup_baseline"]["future_baseline"],
            "kept"
        );
        assert_eq!(value["overlay_position"]["future_position"], "kept");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn serialized_config_updates_preserve_independent_fields() {
        let dir = std::env::temp_dir().join(format!("ai-usage-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"providers":{}}"#).unwrap();
        let path = Arc::new(path);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let first_path = path.clone();
        let first = std::thread::spawn(move || {
            update_app_config_at(&first_path, |cfg| {
                entered_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
                cfg.refresh_interval_secs = 90;
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            update_app_config_at(&second_path, |cfg| {
                cfg.overlay_position = Some(OverlayPosition {
                    x: 10.0,
                    y: 20.0,
                    extra: serde_json::Map::new(),
                });
            })
            .unwrap();
        });
        first.join().unwrap();
        second.join().unwrap();
        let cfg = load_app_config_at(&path);
        assert_eq!(cfg.refresh_interval_secs, 90);
        let position = cfg.overlay_position.unwrap();
        assert_eq!((position.x, position.y), (10.0, 20.0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_app_config_defaults_update_check_timestamp() {
        let cfg: AppConfig = serde_json::from_str(r#"{"providers":{}}"#).unwrap();
        assert_eq!(cfg.last_update_check_at, None);
    }

    #[test]
    fn app_config_round_trips_update_check_timestamp() {
        let timestamp = Utc::now();
        let cfg = AppConfig {
            last_update_check_at: Some(timestamp),
            ..Default::default()
        };
        let reparsed: AppConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed.last_update_check_at, Some(timestamp));
    }

    #[test]
    fn app_config_defaults_refresh_interval_to_60_for_existing_files() {
        let cfg: AppConfig = serde_json::from_str(r#"{"providers":{}}"#).expect("config parses");
        assert_eq!(cfg.refresh_interval_secs, 60);
    }

    #[test]
    fn app_config_serializes_refresh_interval() {
        let cfg = AppConfig {
            refresh_interval_secs: 90,
            ..AppConfig::default()
        };
        let value = serde_json::to_value(&cfg).expect("serialize");
        assert_eq!(value["refresh_interval_secs"], 90);
    }

    #[test]
    fn app_config_defaults_notifications_enabled_for_existing_files() {
        let cfg: AppConfig = serde_json::from_str(r#"{"providers":{}}"#).expect("config parses");
        assert!(cfg.notifications_enabled);
    }

    #[test]
    fn set_notifications_enabled_persists_selection() {
        let (path, dir) = isolated_config_path("notifications-enabled");
        std::fs::write(&path, r#"{"providers":{}}"#).unwrap();
        update_app_config_at(&path, |config| config.notifications_enabled = false).unwrap();
        let cfg = load_app_config_at(&path);
        assert!(!cfg.notifications_enabled);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["notifications_enabled"], false);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_config_map_payload_still_loads() {
        let map: ConfigMap = std::collections::HashMap::new();
        let text = serde_json::to_string(&map).expect("serialize map");
        let parsed: Result<AppConfig, _> = serde_json::from_str(&text);
        let cfg = match parsed {
            Ok(cfg) => cfg,
            Err(_) => {
                let map: ConfigMap = serde_json::from_str(&text).expect("legacy map parses");
                AppConfig {
                    providers: map,
                    overlay_position: None,
                    refresh_interval_secs: default_refresh_interval(),
                    notifications_enabled: default_notifications_enabled(),
                    last_update_check_at: None,
                    update_channel: UpdateChannel::Stable,
                    extra: serde_json::Map::new(),
                }
            }
        };
        assert_eq!(cfg.refresh_interval_secs, 60);
        assert!(cfg.providers.is_empty());
    }

    // --- Update channel persistence ---

    #[test]
    fn update_channel_serializes_as_stable_and_prerelease_only() {
        assert_eq!(
            serde_json::to_value(&UpdateChannel::Stable).unwrap(),
            serde_json::json!("stable")
        );
        assert_eq!(
            serde_json::to_value(&UpdateChannel::Prerelease).unwrap(),
            serde_json::json!("prerelease")
        );
    }

    #[test]
    fn legacy_app_config_defaults_update_channel_to_stable() {
        let cfg: AppConfig = serde_json::from_str(r#"{"providers":{}}"#).unwrap();
        assert_eq!(cfg.update_channel, UpdateChannel::Stable);
    }

    #[test]
    fn app_config_round_trips_update_channel() {
        let cfg = AppConfig {
            update_channel: UpdateChannel::Prerelease,
            ..Default::default()
        };
        let reparsed: AppConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed.update_channel, UpdateChannel::Prerelease);
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["update_channel"], "prerelease");
    }

    #[test]
    fn set_update_channel_persists_selection() {
        let (path, dir) = isolated_config_path("update-channel");
        std::fs::write(&path, r#"{"providers":{}}"#).unwrap();
        // Load defaults, then persist prerelease via the config helper API.
        update_app_config_at(&path, |config| {
            config.update_channel = UpdateChannel::Prerelease;
        })
        .unwrap();
        let cfg = load_app_config_at(&path);
        assert_eq!(cfg.update_channel, UpdateChannel::Prerelease);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["update_channel"], "prerelease");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
