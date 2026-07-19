use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod claude;
pub mod codex;
pub mod glm;
pub mod grok;
pub mod kimi;
pub mod minimax;
pub mod openrouter;

/// Canonical provider order. Shared by `commands::collect` so the bar shows
/// providers in the same order on every surface.
pub const PROVIDER_IDS: &[&str] = &[
    "glm",
    "minimax",
    "codex",
    "claude",
    "grok",
    "kimi",
    "openrouter",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderHealth {
    Healthy,
    MissingCredentials,
    InvalidCredentials,
    NoUsableDetails,
    TransientFailure,
}

impl ProviderHealth {
    pub fn label(&self) -> String {
        match self {
            ProviderHealth::Healthy => "Healthy".into(),
            ProviderHealth::MissingCredentials => "MissingCredentials".into(),
            ProviderHealth::InvalidCredentials => "InvalidCredentials".into(),
            ProviderHealth::NoUsableDetails => "NoUsableDetails".into(),
            ProviderHealth::TransientFailure => "TransientFailure".into(),
        }
    }

    pub fn refresh_message(&self, reason: Option<&str>) -> String {
        if let Some(r) = reason.map(str::trim).filter(|s| !s.is_empty()) {
            return r.to_string();
        }
        match self {
            ProviderHealth::Healthy => "Updated".into(),
            ProviderHealth::MissingCredentials => "No credentials configured".into(),
            ProviderHealth::InvalidCredentials => "Invalid credentials".into(),
            ProviderHealth::NoUsableDetails => "No usable usage details".into(),
            ProviderHealth::TransientFailure => {
                "Temporary error (network or rate limit) — last values kept".into()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderFetch {
    pub snapshot: Option<UsageSnapshot>,
    pub health: ProviderHealth,
    pub reason: Option<String>,
}

impl ProviderFetch {
    pub fn healthy(snapshot: UsageSnapshot) -> Self {
        Self {
            snapshot: Some(snapshot),
            health: ProviderHealth::Healthy,
            reason: None,
        }
    }

    pub fn hard(health: ProviderHealth, reason: impl Into<String>) -> Self {
        debug_assert!(matches!(
            health,
            ProviderHealth::MissingCredentials
                | ProviderHealth::InvalidCredentials
                | ProviderHealth::NoUsableDetails
        ));
        Self {
            snapshot: None,
            health,
            reason: Some(reason.into()),
        }
    }

    pub fn transient(reason: impl Into<String>) -> Self {
        Self {
            snapshot: None,
            health: ProviderHealth::TransientFailure,
            reason: Some(reason.into()),
        }
    }

    pub fn transient_with_snapshot(snapshot: UsageSnapshot, reason: impl Into<String>) -> Self {
        Self {
            snapshot: Some(snapshot),
            health: ProviderHealth::TransientFailure,
            reason: Some(reason.into()),
        }
    }
}

pub fn classify_snapshot(snapshot: UsageSnapshot) -> ProviderFetch {
    let has_displayable_window = snapshot.windows.iter().any(|window| window.bar_visible);
    let Some(reason) = snapshot.unavailable_reason.clone() else {
        return if !has_displayable_window {
            ProviderFetch::hard(ProviderHealth::NoUsableDetails, "no usable usage details")
        } else {
            ProviderFetch::healthy(snapshot)
        };
    };

    let lower = reason.to_ascii_lowercase();
    let is_missing = lower.contains("no api key")
        || lower.contains("no key")
        || lower.contains("no auth")
        || lower.contains("logs not found")
        || lower.contains("no home directory");
    // NOTE: "login failed" is deliberately NOT an invalid keyword. MiniMax's
    // all-endpoints transport failure reads "network error: login failed at
    // all endpoints", and matching it turned plain network blips into
    // credential failures. Genuine auth rejections say "invalid api key" /
    // 401 / 403 / "session expired" / "auth mode".
    let is_invalid = lower.contains("invalid api key")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("session expired")
        || lower.contains("auth mode");
    let is_transient = lower.contains("network")
        || lower.contains("transport")
        || lower.contains("timeout")
        || lower.contains("decode")
        || lower.contains("http ")
        || lower.contains("join:")
        || lower.contains("stale local log");

    if is_missing {
        ProviderFetch::hard(ProviderHealth::MissingCredentials, reason)
    } else if is_invalid {
        ProviderFetch::hard(ProviderHealth::InvalidCredentials, reason)
    } else if is_transient {
        if !has_displayable_window {
            ProviderFetch::transient(reason)
        } else {
            ProviderFetch::transient_with_snapshot(snapshot, reason)
        }
    } else if !has_displayable_window {
        ProviderFetch::hard(ProviderHealth::NoUsableDetails, reason)
    } else {
        ProviderFetch::transient_with_snapshot(snapshot, reason)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    pub used_percent: f32,
    pub reset_at: Option<DateTime<Utc>>,
    /// Shown in the expanded popup but excluded from the collapsed status bar.
    /// Used for GLM's monthly Web Search / Reader / Zread tool quota.
    #[serde(default = "default_bar_visible")]
    pub bar_visible: bool,
    /// A provider has explicitly reported that this window has no quota cap.
    #[serde(default)]
    pub is_unlimited: bool,
    /// Absolute usage counter when the provider reports one (e.g. Kimi's
    /// `used`/`limit` quota numbers). Popup-only detail; `used_percent`
    /// remains the canonical value everywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_absolute: Option<f64>,
    /// Absolute quota cap paired with `used_absolute`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_absolute: Option<f64>,
}

fn default_bar_visible() -> bool {
    true
}

/// Compact bar-friendly label: "weekly" → "wk", "5h"/"session" → "5h",
/// "daily" → "day", etc. Unknown labels pass through unchanged. Shared by
/// the per-provider Settings → Test summary formatters.
pub(crate) fn short_window_label(label: &str) -> &str {
    match label.to_ascii_lowercase().as_str() {
        "daily" => "day",
        "weekly" | "wk" => "wk",
        "monthly" | "mo" => "mo",
        "5h" | "session" => "5h",
        "3h" => "3h",
        _ => label,
    }
}

/// Short display name for compact surfaces (tray tooltip, notifications).
/// Falls back to the raw id so unknown providers still render something.
pub fn short_provider_name(id: &str) -> String {
    match id {
        "glm" => "GLM",
        "minimax" => "MiniMax",
        "codex" => "Codex",
        "claude" => "Claude",
        "grok" => "Grok",
        "kimi" => "Kimi",
        "openrouter" => "OpenRouter",
        _ => id,
    }
    .to_string()
}

/// Resolve a CLI tool's config directory: honor `$env_var` when set (non-empty),
/// else `~/default_dir`. Shared by Grok/Codex fetch and in-app OAuth paths.
pub(crate) fn cli_home_dir(env_var: &str, default_dir: &str) -> Result<std::path::PathBuf, String> {
    if let Ok(p) = std::env::var(env_var) {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(std::path::PathBuf::from(p));
        }
    }
    let home = dirs::home_dir().ok_or("no home directory")?;
    Ok(home.join(default_dir))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub provider: String,
    pub level: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub unavailable_reason: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

impl UsageSnapshot {
    pub fn unavailable(provider: &str, reason: impl Into<String>) -> Self {
        Self {
            provider: provider.to_string(),
            level: None,
            windows: vec![],
            unavailable_reason: Some(reason.into()),
            fetched_at: Utc::now(),
        }
    }
}

#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    async fn fetch(&self, secrets: &crate::secrets::Secrets) -> ProviderFetch;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_error_with_login_failed_wording_is_transient() {
        // Regression: MiniMax's all-endpoints transport failure reads
        // "network error: login failed at all endpoints". It used to match the
        // "login failed" invalid-credential keyword, which auto-hid the
        // provider on a plain network blip.
        let fetch = classify_snapshot(UsageSnapshot::unavailable(
            "MiniMax Coding Plan",
            "network error: login failed at all endpoints",
        ));
        assert!(matches!(fetch.health, ProviderHealth::TransientFailure));
        assert!(fetch.snapshot.is_none());
    }

    #[test]
    fn genuine_auth_rejections_stay_invalid() {
        for reason in [
            "invalid api key",
            "session expired — run grok login",
            "auth mode not supported",
        ] {
            let fetch = classify_snapshot(UsageSnapshot::unavailable("Test", reason));
            assert!(
                matches!(fetch.health, ProviderHealth::InvalidCredentials),
                "{reason} must classify as InvalidCredentials"
            );
        }
    }

    #[test]
    fn short_provider_name_maps_known_ids_and_falls_back() {
        assert_eq!(short_provider_name("glm"), "GLM");
        assert_eq!(short_provider_name("minimax"), "MiniMax");
        assert_eq!(short_provider_name("openrouter"), "OpenRouter");
        assert_eq!(short_provider_name("unknown-id"), "unknown-id");
    }
}
