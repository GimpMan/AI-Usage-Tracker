//! OpenAI Codex / ChatGPT OAuth via device-code against auth.openai.com.

use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use super::session::{OAuthPhase, OAuthSession, SessionKind};
use super::{
    http_client, insert_session, new_session_id, open_browser, spawn_device_poller, OAuthPoll,
    OAuthStart,
};
use crate::secrets;

pub(crate) const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// Shared with `providers::codex` for refresh-token rotation (F10).
pub(crate) const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Device-flow redirect used when exchanging the intermediate authorization_code.
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const VERIFY_BASE: &str = "https://auth.openai.com/codex/device";

#[derive(Deserialize)]
struct UserCodeResponse {
    device_auth_id: String,
    user_code: String,
    #[serde(default)]
    interval: Option<serde_json::Value>,
    #[serde(default)]
    expires_at: Option<String>,
}

/// Intermediate success from `deviceauth/token` after the user approves in the browser.
/// Does **not** include tokens — must be exchanged at `/oauth/token`.
#[derive(Deserialize)]
struct DeviceTokenResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    authorization_code: Option<String>,
    #[serde(default)]
    code_verifier: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    tokens: Option<TokenNested>,
    #[serde(default)]
    error: Option<TokenError>,
}

#[derive(Deserialize)]
struct TokenNested {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// Final tokens from `POST /oauth/token`.
#[derive(Deserialize)]
struct OAuthTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// Intentionally unused — never surface to UI (may echo secrets).
    #[serde(default)]
    #[allow(dead_code)]
    error_description: Option<String>,
}

pub async fn start() -> Result<OAuthStart, String> {
    let client = http_client()?;
    let resp = client
        .post(USERCODE_URL)
        .header("Accept", "application/json")
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|e| format!("codex device code: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Status only — never surface response bodies (may contain secrets).
        let _ = resp.text().await;
        return Err(format!("codex device code failed (http {status})"));
    }
    let body: UserCodeResponse = resp
        .json()
        .await
        .map_err(|e| format!("codex device code decode: {e}"))?;

    let interval = body
        .interval
        .as_ref()
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(5)
        .max(1);

    // expires_at is RFC3339; fall back to 15 minutes.
    let expires_at = body
        .expires_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| {
            let secs = (dt.with_timezone(&Utc) - Utc::now()).num_seconds().max(30) as u64;
            Instant::now() + Duration::from_secs(secs)
        })
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(900));

    let code = body.user_code.clone();
    let verify_url = VERIFY_BASE.to_string();
    let message =
        format!("Type this code in the browser: {code}  (then Continue — we wait for approval)");
    let expires_in = expires_at
        .saturating_duration_since(Instant::now())
        .as_secs();

    let session_id = new_session_id();
    insert_session(OAuthSession {
        id: session_id.clone(),
        provider: "codex".into(),
        kind: SessionKind::CodexDevice {
            device_auth_id: body.device_auth_id,
            user_code: code.clone(),
        },
        kind_label: "device".into(),
        user_code: Some(code.clone()),
        verification_uri: Some(VERIFY_BASE.into()),
        verification_uri_complete: Some(verify_url.clone()),
        authorize_url: None,
        message: message.clone(),
        expires_at,
        phase: OAuthPhase::Pending,
        created_at: Instant::now(),
    })?;
    spawn_device_poller(session_id.clone(), interval);

    // Open without relying on query-param prefill — form boxes are often empty.
    let _ = open_browser(&verify_url);

    Ok(OAuthStart {
        provider: "codex".into(),
        session_id,
        kind: "device".into(),
        user_code: Some(code),
        verification_uri: Some(VERIFY_BASE.into()),
        verification_uri_complete: Some(verify_url),
        authorize_url: None,
        expires_in: Some(expires_in),
        message,
        status: "pending".into(),
    })
}

/// One token-endpoint poll (no expiry handling — caller does that).
pub async fn poll_once(session: &OAuthSession) -> Result<OAuthPoll, String> {
    let (device_auth_id, user_code) = match &session.kind {
        SessionKind::CodexDevice {
            device_auth_id,
            user_code,
        } => (device_auth_id.clone(), user_code.clone()),
        _ => return Err("not a codex device session".into()),
    };

    let client = http_client()?;
    let resp = client
        .post(DEVICE_TOKEN_URL)
        .header("Accept", "application/json")
        .json(&json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
        }))
        .send()
        .await
        .map_err(|e| format!("codex device token: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("codex device token body: {e}"))?;

    let pending_poll = || OAuthPoll {
        status: "pending".into(),
        message: Some(session.message.clone()),
        provider: Some("codex".into()),
        user_code: session.user_code.clone(),
        session_id: Some(session.id.clone()),
    };

    // Pending: 403 / 404 / structured pending error.
    if status.as_u16() == 403 || status.as_u16() == 404 {
        if text.to_ascii_lowercase().contains("pending") || !status.is_success() {
            return Ok(pending_poll());
        }
    }

    let body: DeviceTokenResponse = serde_json::from_str(&text).unwrap_or(DeviceTokenResponse {
        status: None,
        authorization_code: None,
        code_verifier: None,
        access_token: None,
        refresh_token: None,
        id_token: None,
        account_id: None,
        tokens: None,
        error: None,
    });

    if let Some(err) = body.error.as_ref() {
        let code = err.code.as_deref().unwrap_or("");
        let msg = err.message.as_deref().unwrap_or("");
        let pending = code.contains("pending")
            || msg.to_ascii_lowercase().contains("pending")
            || code == "deviceauth_authorization_pending";
        if pending || text.to_ascii_lowercase().contains("pending") {
            return Ok(pending_poll());
        }
        if !status.is_success() {
            let code = err.code.as_deref().filter(|s| !s.is_empty());
            return Ok(OAuthPoll {
                status: "error".into(),
                // OAuth error code / status only — never raw body or free-form
                // server messages that might echo token material.
                message: Some(match code {
                    Some(c) => format!("codex device token error: {c} (http {status})"),
                    None => format!("codex device token failed (http {status})"),
                }),
                provider: Some("codex".into()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            });
        }
    }

    if !status.is_success() {
        if text.to_ascii_lowercase().contains("pending") {
            return Ok(pending_poll());
        }
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("codex device token failed (http {status})")),
            provider: Some("codex".into()),
            user_code: session.user_code.clone(),
            session_id: Some(session.id.clone()),
        });
    }

    // Path A: rare — tokens already present on the device endpoint.
    if let Some(access) = body
        .access_token
        .as_deref()
        .or_else(|| body.tokens.as_ref().and_then(|t| t.access_token.as_deref()))
        .filter(|s| !s.is_empty())
    {
        let refresh = body
            .refresh_token
            .or_else(|| body.tokens.as_ref().and_then(|t| t.refresh_token.clone()));
        let id_token = body
            .id_token
            .or_else(|| body.tokens.as_ref().and_then(|t| t.id_token.clone()));
        let account_id = body
            .account_id
            .or_else(|| body.tokens.as_ref().and_then(|t| t.account_id.clone()))
            .or_else(|| extract_account_id_from_jwt(access));
        persist_tokens(
            access,
            refresh.as_deref(),
            id_token.as_deref(),
            account_id.as_deref(),
        )?;
        return Ok(done_poll(session));
    }

    // Path B (normal device flow): exchange authorization_code + code_verifier.
    let auth_code = body
        .authorization_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let code_verifier = body
        .code_verifier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let (Some(auth_code), Some(code_verifier)) = (auth_code, code_verifier) else {
        // Success without code yet — keep waiting.
        if body
            .status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("success"))
            .unwrap_or(false)
        {
            return Ok(OAuthPoll {
                status: "error".into(),
                message: Some(
                    "Codex approved in browser but no authorization_code was returned — try Sign in again."
                        .into(),
                ),
                provider: Some("codex".into()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            });
        }
        return Ok(pending_poll());
    };

    exchange_authorization_code(&client, auth_code, code_verifier, session).await
}

async fn exchange_authorization_code(
    client: &reqwest::Client,
    authorization_code: &str,
    code_verifier: &str,
    session: &OAuthSession,
) -> Result<OAuthPoll, String> {
    let resp = client
        .post(OAUTH_TOKEN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("codex oauth exchange: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("codex oauth exchange body: {e}"))?;

    let body: OAuthTokenResponse = serde_json::from_str(&text).unwrap_or(OAuthTokenResponse {
        access_token: None,
        refresh_token: None,
        id_token: None,
        error: None,
        error_description: None,
    });

    if let Some(err) = body.error {
        // OAuth error code only — never error_description or raw body.
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("codex oauth exchange error: {err} (http {status})")),
            provider: Some("codex".into()),
            user_code: session.user_code.clone(),
            session_id: Some(session.id.clone()),
        });
    }
    if !status.is_success() {
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("codex oauth exchange failed (http {status})")),
            provider: Some("codex".into()),
            user_code: session.user_code.clone(),
            session_id: Some(session.id.clone()),
        });
    }

    let access = body
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            // Never interpolate `text` — a partial token response can include
            // refresh_token without access_token.
            "codex oauth exchange missing access_token".to_string()
        })?;
    let account_id = extract_account_id_from_jwt(&access).or_else(|| {
        body.id_token
            .as_deref()
            .and_then(extract_account_id_from_jwt)
    });

    persist_tokens(
        &access,
        body.refresh_token.as_deref(),
        body.id_token.as_deref(),
        account_id.as_deref(),
    )?;

    Ok(done_poll(session))
}

fn done_poll(session: &OAuthSession) -> OAuthPoll {
    OAuthPoll {
        status: "complete".into(),
        message: Some("Signed in to Codex (ChatGPT). Usage will refresh on the next poll.".into()),
        provider: Some("codex".into()),
        user_code: session.user_code.clone(),
        session_id: Some(session.id.clone()),
    }
}

fn extract_account_id_from_jwt(token: &str) -> Option<String> {
    // ChatGPT access tokens are often JWTs with an `https://api.openai.com/auth` claim
    // containing chatgpt_account_id. Best-effort only.
    let payload = token.split('.').nth(1)?;
    let padded = match payload.len() % 4 {
        2 => format!("{payload}=="),
        3 => format!("{payload}="),
        _ => payload.to_string(),
    };
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        payload.as_bytes(),
    )
    .or_else(|_| {
        base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE,
            padded.as_bytes(),
        )
    })
    .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.pointer("/https://api.openai.com/auth/chatgpt_account_id")
        .or_else(|| v.get("chatgpt_account_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

fn persist_tokens(
    access_token: &str,
    refresh_token: Option<&str>,
    id_token: Option<&str>,
    account_id: Option<&str>,
) -> Result<(), String> {
    // App-only session in Windows Credential Manager — never writes ~/.codex/auth.json.
    let doc = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        },
        "last_refresh": Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
    });
    secrets::oauth_set_json("codex", &doc).map_err(|e| format!("store codex oauth: {e}"))?;
    log::info!("codex oauth: stored session in Credential Manager");
    Ok(())
}

pub fn logout() -> Result<String, String> {
    secrets::oauth_delete("codex").map_err(|e| e.to_string())?;
    Ok("Codex app sign-in cleared (CLI login unchanged)".into())
}
