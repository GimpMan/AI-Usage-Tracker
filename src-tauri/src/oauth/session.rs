use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// In-memory pending OAuth sessions (device codes / PKCE verifiers).
/// Survives Settings window close/reopen as long as the app process is alive.
pub fn sessions() -> &'static Mutex<SessionStore> {
    static OAUTH_SESSIONS: OnceLock<Mutex<SessionStore>> = OnceLock::new();
    OAUTH_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub type SessionStore = HashMap<String, OAuthSession>;

#[derive(Debug, Clone)]
pub enum OAuthPhase {
    /// Waiting for browser approval (device) or pasted code (Claude).
    Pending,
    Complete {
        message: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct OAuthSession {
    pub id: String,
    pub provider: String,
    pub kind: SessionKind,
    /// "device" | "manual_code" — for the frontend.
    pub kind_label: String,
    /// Shown in Settings; must survive window remount.
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub verification_uri_complete: Option<String>,
    pub authorize_url: Option<String>,
    pub message: String,
    pub expires_at: Instant,
    pub phase: OAuthPhase,
    pub created_at: Instant,
}

#[derive(Debug, Clone)]
pub enum SessionKind {
    GrokDevice {
        device_code: String,
    },
    CodexDevice {
        device_auth_id: String,
        user_code: String,
    },
    ClaudeManual {
        code_verifier: String,
        state: String,
        redirect_uri: String,
    },
    KimiDevice {
        device_code: String,
    },
}

impl OAuthSession {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    pub fn expires_in_secs(&self) -> u64 {
        self.expires_at
            .saturating_duration_since(Instant::now())
            .as_secs()
    }
}
