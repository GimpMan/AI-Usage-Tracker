//! In-app OAuth for subscription-backed providers (Grok, Codex, Claude, Kimi).
//!
//! Completed tokens are stored in **Windows Credential Manager** (app-only),
//! separate from CLI auth files. In-flight sessions live in process memory so
//! **closing Settings does not lose the device code**. Device-code providers
//! also get a **background poller** that finishes login even if the UI is closed.

mod claude;
pub(crate) mod codex;
mod grok;
mod kimi;
mod pkce;
mod session;

use serde::Serialize;
use session::{sessions, OAuthPhase, SessionKind};
use std::time::Duration;

pub use session::OAuthSession;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthStart {
    pub provider: String,
    pub session_id: String,
    pub kind: String,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub authorize_url: Option<String>,
    pub expires_in: Option<u64>,
    pub message: String,
    /// pending | complete | error — so reopened Settings can restore state.
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthPoll {
    pub status: String, // pending | complete | error | cancelled
    pub message: Option<String>,
    pub provider: Option<String>,
    pub user_code: Option<String>,
    pub session_id: Option<String>,
}

/// Start an interactive login for `provider` (`grok` | `codex` | `claude` | `kimi`).
pub async fn start_login(provider: &str) -> Result<OAuthStart, String> {
    match provider {
        "grok" => grok::start().await,
        "codex" => codex::start().await,
        "claude" => claude::start().await,
        "kimi" => kimi::start().await,
        other => Err(format!("oauth not supported for '{other}'")),
    }
}

/// Snapshot of one active (or just-finished) session for UI restore.
pub fn active_for_provider(provider: &str) -> Option<OAuthStart> {
    let store = sessions().lock().ok()?;
    // Prefer non-expired pending; else most recent for this provider.
    let mut best: Option<&OAuthSession> = None;
    for s in store.values() {
        if s.provider != provider {
            continue;
        }
        best = Some(match best {
            None => s,
            Some(prev) => {
                if s.created_at >= prev.created_at {
                    s
                } else {
                    prev
                }
            }
        });
    }
    best.map(session_to_start)
}

/// All sessions that are still useful to show in Settings.
pub fn list_active() -> Vec<OAuthStart> {
    let Ok(store) = sessions().lock() else {
        return Vec::new();
    };
    // One entry per provider (newest).
    let mut by_provider: std::collections::HashMap<String, OAuthSession> =
        std::collections::HashMap::new();
    for s in store.values() {
        by_provider
            .entry(s.provider.clone())
            .and_modify(|prev| {
                if s.created_at >= prev.created_at {
                    *prev = s.clone();
                }
            })
            .or_insert_with(|| s.clone());
    }
    by_provider.values().map(session_to_start).collect()
}

fn session_to_start(s: &OAuthSession) -> OAuthStart {
    let (status, message) = match &s.phase {
        OAuthPhase::Pending => {
            if s.is_expired() {
                (
                    "error".to_string(),
                    "login timed out — start again".to_string(),
                )
            } else {
                ("pending".to_string(), s.message.clone())
            }
        }
        OAuthPhase::Complete { message } => ("complete".to_string(), message.clone()),
        OAuthPhase::Error { message } => ("error".to_string(), message.clone()),
    };
    OAuthStart {
        provider: s.provider.clone(),
        session_id: s.id.clone(),
        kind: s.kind_label.clone(),
        user_code: s.user_code.clone(),
        verification_uri: s.verification_uri.clone(),
        verification_uri_complete: s.verification_uri_complete.clone(),
        authorize_url: s.authorize_url.clone(),
        expires_in: Some(s.expires_in_secs()),
        message,
        status,
    }
}

/// Poll a device-code session once (also used by the background poller).
pub async fn poll_login(session_id: &str) -> Result<OAuthPoll, String> {
    let snap = {
        let store = sessions().lock().map_err(|e| e.to_string())?;
        store.get(session_id).cloned()
    };
    let Some(session) = snap else {
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some("unknown or expired login session".into()),
            provider: None,
            user_code: None,
            session_id: Some(session_id.into()),
        });
    };

    // Already finished (e.g. background poller won the race).
    match &session.phase {
        OAuthPhase::Complete { message } => {
            return Ok(OAuthPoll {
                status: "complete".into(),
                message: Some(message.clone()),
                provider: Some(session.provider.clone()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            });
        }
        OAuthPhase::Error { message } => {
            return Ok(OAuthPoll {
                status: "error".into(),
                message: Some(message.clone()),
                provider: Some(session.provider.clone()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            });
        }
        OAuthPhase::Pending => {}
    }

    if session.is_expired() {
        set_phase(
            &session.id,
            OAuthPhase::Error {
                message: "login timed out — start again".into(),
            },
        );
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some("login timed out — start again".into()),
            provider: Some(session.provider.clone()),
            user_code: session.user_code.clone(),
            session_id: Some(session.id.clone()),
        });
    }

    let result = match &session.kind {
        SessionKind::GrokDevice { .. } => grok::poll_once(&session).await,
        SessionKind::CodexDevice { .. } => codex::poll_once(&session).await,
        SessionKind::KimiDevice { .. } => kimi::poll_once(&session).await,
        SessionKind::ClaudeManual { .. } => Ok(OAuthPoll {
            status: "pending".into(),
            message: Some(
                "Open the authorize link, then paste the CODE#STATE value into Complete.".into(),
            ),
            provider: Some(session.provider.clone()),
            user_code: None,
            session_id: Some(session.id.clone()),
        }),
    };

    // Persist terminal phases so reopened Settings sees them.
    if let Ok(ref poll) = result {
        match poll.status.as_str() {
            "complete" => {
                set_phase(
                    &session.id,
                    OAuthPhase::Complete {
                        message: poll.message.clone().unwrap_or_else(|| "Signed in.".into()),
                    },
                );
            }
            "error" => {
                set_phase(
                    &session.id,
                    OAuthPhase::Error {
                        message: poll
                            .message
                            .clone()
                            .unwrap_or_else(|| "Sign-in failed.".into()),
                    },
                );
            }
            _ => {}
        }
    }

    result.map(|mut p| {
        if p.user_code.is_none() {
            p.user_code = session.user_code.clone();
        }
        if p.session_id.is_none() {
            p.session_id = Some(session.id.clone());
        }
        p
    })
}

/// Finish a manual-code flow (Claude) by submitting the pasted authorization code.
pub async fn complete_login(session_id: &str, code: &str) -> Result<OAuthPoll, String> {
    let session = {
        let store = sessions().lock().map_err(|e| e.to_string())?;
        store.get(session_id).cloned()
    };
    let Some(session) = session else {
        return Err("unknown or expired login session".into());
    };
    match session.kind {
        SessionKind::ClaudeManual { .. } => {
            let result = claude::complete(&session, code).await;
            if let Ok(ref poll) = result {
                match poll.status.as_str() {
                    "complete" => set_phase(
                        &session.id,
                        OAuthPhase::Complete {
                            message: poll.message.clone().unwrap_or_else(|| "Signed in.".into()),
                        },
                    ),
                    "error" => set_phase(
                        &session.id,
                        OAuthPhase::Error {
                            message: poll
                                .message
                                .clone()
                                .unwrap_or_else(|| "Sign-in failed.".into()),
                        },
                    ),
                    _ => {}
                }
            }
            result
        }
        _ => Err("this login session does not accept a pasted code".into()),
    }
}

pub fn cancel_login(session_id: &str) -> Result<(), String> {
    let mut store = sessions().lock().map_err(|e| e.to_string())?;
    store.remove(session_id);
    Ok(())
}

/// Best-effort local sign-out: remove tokens from the CLI auth files.
pub fn logout(provider: &str) -> Result<String, String> {
    match provider {
        "grok" => grok::logout(),
        "codex" => codex::logout(),
        "claude" => claude::logout(),
        "kimi" => kimi::logout(),
        other => Err(format!("oauth logout not supported for '{other}'")),
    }
}

/// Open a URL in the system default browser (best-effort).
///
/// On Windows, bare `cmd /C start <url>` is unsafe for OAuth authorize links:
/// `cmd` treats `&` as a command separator, so `?a=1&client_id=…` is truncated
/// and Claude shows "Missing client_id parameter". Always quote the URL.
pub fn open_browser(url: &str) -> Result<(), String> {
    // Only https (and http://localhost for local IdP dev). Blocks file:// etc.
    // from a hostile device-code verification_uri.
    let parsed = url::Url::parse(url).map_err(|e| format!("open browser: invalid url: {e}"))?;
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("");
    let ok = scheme == "https"
        || (scheme == "http" && (host == "localhost" || host == "127.0.0.1" || host == "[::1]"));
    if !ok {
        return Err(format!(
            "open browser: blocked non-https url scheme '{scheme}'"
        ));
    }

    // Prefer PowerShell Start-Process — it does not re-parse `&` in the URL.
    let ps_url = url.replace('\'', "''");
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!("Start-Process -FilePath '{ps_url}'"),
        ])
        .status()
        .map_err(|e| format!("open browser (powershell): {e}"))?;
    if status.success() {
        return Ok(());
    }
    // Fallback: cmd start with a double-quoted URL (empty window title first).
    let quoted = format!("\"{}\"", url.replace('"', ""));
    std::process::Command::new("cmd")
        .args(["/C", "start", "", &quoted])
        .spawn()
        .map_err(|e| format!("open browser (cmd): {e}"))?;
    Ok(())
}

pub(crate) fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut buf = [0u8; 8];
    let _ = getrandom::fill(&mut buf);
    format!("{t:x}-{}", hex8(&buf))
}

fn hex8(bytes: &[u8; 8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn insert_session(session: OAuthSession) -> Result<String, String> {
    let id = session.id.clone();
    let mut store = sessions().lock().map_err(|e| e.to_string())?;
    // Drop other sessions for the same provider so only one login runs at once.
    store.retain(|_, s| s.provider != session.provider);
    store.insert(id.clone(), session);
    Ok(id)
}

pub(crate) fn set_phase(id: &str, phase: OAuthPhase) {
    if let Ok(mut store) = sessions().lock() {
        if let Some(s) = store.get_mut(id) {
            s.phase = phase;
        }
    }
}

pub(crate) fn http_client() -> Result<reqwest::Client, String> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(45))
                .user_agent("ai-usage-tracker/0.1")
                .build()
                .map_err(|e| format!("http client: {e}"))
        })
        .clone()
}

/// Spawn a background task that polls a device-code session until it finishes
/// or expires. Safe to call even if Settings is closed.
pub(crate) fn spawn_device_poller(session_id: String, interval_secs: u64) {
    let interval = Duration::from_secs(interval_secs.max(3));
    tauri::async_runtime::spawn(async move {
        // Small initial delay so the first UI paint wins the race.
        tokio::time::sleep(Duration::from_millis(800)).await;
        loop {
            let still_pending = {
                let Ok(store) = sessions().lock() else {
                    break;
                };
                match store.get(&session_id) {
                    Some(s) => matches!(s.phase, OAuthPhase::Pending) && !s.is_expired(),
                    None => false,
                }
            };
            if !still_pending {
                break;
            }
            match poll_login(&session_id).await {
                Ok(p) if p.status == "pending" => {
                    tokio::time::sleep(interval).await;
                }
                Ok(p) => {
                    log::info!(
                        "oauth background poll finished session={session_id} status={}",
                        p.status
                    );
                    break;
                }
                Err(e) => {
                    log::warn!("oauth background poll error session={session_id}: {e}");
                    tokio::time::sleep(interval).await;
                }
            }
        }
    });
}
