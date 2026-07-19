//! Grok SuperGrok / Build OAuth via device-code against auth.x.ai.

use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::session::{OAuthPhase, OAuthSession, SessionKind};
use super::{
    http_client, insert_session, new_session_id, open_browser, spawn_device_poller, OAuthPoll,
    OAuthStart,
};
use crate::secrets;

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
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
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", CLIENT_ID), ("scope", SCOPE)])
        .send()
        .await
        .map_err(|e| format!("grok device code: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Status only — never surface response bodies (may contain secrets).
        let _ = resp.text().await;
        return Err(format!("grok device code failed (http {status})"));
    }
    let body: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|e| format!("grok device code decode: {e}"))?;

    let expires_in = body.expires_in.unwrap_or(1800);
    let interval = body.interval.unwrap_or(5).max(1);
    let complete = body
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| format!("{}?user_code={}", body.verification_uri, body.user_code));
    let code = body.user_code.clone();
    let message =
        format!("If the browser asks for a code, enter: {code}  (then approve — we wait)");

    let session_id = new_session_id();
    insert_session(OAuthSession {
        id: session_id.clone(),
        provider: "grok".into(),
        kind: SessionKind::GrokDevice {
            device_code: body.device_code,
        },
        kind_label: "device".into(),
        user_code: Some(code.clone()),
        verification_uri: Some(body.verification_uri.clone()),
        verification_uri_complete: Some(complete.clone()),
        authorize_url: None,
        message: message.clone(),
        expires_at: Instant::now() + Duration::from_secs(expires_in),
        phase: OAuthPhase::Pending,
        created_at: Instant::now(),
    })?;
    spawn_device_poller(session_id.clone(), interval);

    // Prefer the complete URL (pre-fills when the IdP supports it).
    let _ = open_browser(&complete);

    Ok(OAuthStart {
        provider: "grok".into(),
        session_id,
        kind: "device".into(),
        user_code: Some(code),
        verification_uri: Some(body.verification_uri),
        verification_uri_complete: Some(complete),
        authorize_url: None,
        expires_in: Some(expires_in),
        message,
        status: "pending".into(),
    })
}

/// One token-endpoint poll (no expiry handling — caller does that).
pub async fn poll_once(session: &OAuthSession) -> Result<OAuthPoll, String> {
    let device_code = match &session.kind {
        SessionKind::GrokDevice { device_code } => device_code.clone(),
        _ => return Err("not a grok device session".into()),
    };

    let client = http_client()?;
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("grok token: {e}"))?;

    let status = resp.status();
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("grok token decode: {e}"))?;

    if let Some(err) = body.error.as_deref() {
        return match err {
            "authorization_pending" | "slow_down" => Ok(OAuthPoll {
                status: "pending".into(),
                message: Some(session.message.clone()),
                provider: Some("grok".into()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            }),
            // OAuth error code only — never error_description or raw body.
            "expired_token" | "access_denied" => Ok(OAuthPoll {
                status: "error".into(),
                message: Some(format!("grok token error: {err}")),
                provider: Some("grok".into()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            }),
            other => Ok(OAuthPoll {
                status: "error".into(),
                message: Some(format!("grok token error: {other} (http {status})")),
                provider: Some("grok".into()),
                user_code: session.user_code.clone(),
                session_id: Some(session.id.clone()),
            }),
        };
    }

    let access = body
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("grok token missing access_token (http {status})"))?;

    persist_tokens(
        &access,
        body.refresh_token.as_deref(),
        body.expires_in.unwrap_or(21_600),
        body.id_token.as_deref(),
    )?;

    Ok(OAuthPoll {
        status: "complete".into(),
        message: Some("Signed in to Grok. SuperGrok usage will refresh on the next poll.".into()),
        provider: Some("grok".into()),
        user_code: session.user_code.clone(),
        session_id: Some(session.id.clone()),
    })
}

fn persist_tokens(
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: u64,
    _id_token: Option<&str>,
) -> Result<(), String> {
    // App-only session in Credential Manager — same JSON shape as CLI auth.json
    // (map of OIDC entries) so the provider module can reuse select/refresh logic.
    let mut map: Map<String, Value> = secrets::oauth_get_json("grok")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let expires_at = (Utc::now() + chrono::Duration::seconds(expires_in as i64))
        .to_rfc3339_opts(SecondsFormat::Nanos, true);
    let key = format!("https://auth.x.ai::{CLIENT_ID}");
    let mut entry = json!({
        "key": access_token,
        "auth_mode": "oidc",
        "create_time": Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
        "expires_at": expires_at,
        "oidc_issuer": "https://auth.x.ai",
        "oidc_client_id": CLIENT_ID,
    });
    if let Some(rt) = refresh_token {
        entry
            .as_object_mut()
            .unwrap()
            .insert("refresh_token".into(), Value::String(rt.to_string()));
    }
    map.insert(key, entry);

    secrets::oauth_set_json("grok", &Value::Object(map))
        .map_err(|e| format!("store grok oauth: {e}"))?;
    log::info!("grok oauth: stored session in Credential Manager");
    Ok(())
}

pub fn logout() -> Result<String, String> {
    secrets::oauth_delete("grok").map_err(|e| e.to_string())?;
    Ok("Grok app sign-in cleared (CLI login unchanged)".into())
}
