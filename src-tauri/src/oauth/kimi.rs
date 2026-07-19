//! Kimi Code OAuth (device authorization against auth.kimi.com).
//!
//! Public client (no secret). Tokens are stored in Windows Credential Manager
//! (app-only). The Kimi CLI keeps a separate login under `~/.kimi-code/`.

use std::time::{Duration, Instant};

use serde::Deserialize;

use super::session::{OAuthPhase, OAuthSession, SessionKind};
use super::{
    http_client, insert_session, new_session_id, open_browser, spawn_device_poller, OAuthPoll,
    OAuthStart,
};
use crate::providers::kimi::{
    oauth_host, persist_credential_tokens, remove_credential_file, TokenPersist, CLIENT_ID,
};

const DEVICE_PATH: &str = "/api/oauth/device_authorization";
const TOKEN_PATH: &str = "/api/oauth/token";
const GRANT_DEVICE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Parsed OAuth token endpoint JSON (device poll or refresh).
#[derive(Debug, Clone)]
pub struct ParsedTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub scope: Option<String>,
    pub token_type: Option<String>,
    /// Present when the server sends Unix-second expiry; otherwise derived.
    pub expires_at: Option<i64>,
}

/// Require non-empty access + refresh and a positive `expires_in`.
pub fn parse_token_response(body: &str) -> Result<ParsedTokenResponse, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("kimi token JSON: {e}"))?;

    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        // OAuth error code only — never surface error_description or raw body.
        return Err(format!("kimi token error: {err}"));
    }

    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "kimi token missing access_token".to_string())?
        .to_string();

    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "kimi token missing refresh_token".to_string())?
        .to_string();

    // Positive expiry only: zero / negative decoded values are rejected.
    let expires_in = v
        .get("expires_in")
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_i64().and_then(|i| u64::try_from(i).ok()))
                .or_else(|| {
                    x.as_f64()
                        .filter(|f| f.is_finite() && *f > 0.0)
                        .map(|f| f as u64)
                })
                .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .filter(|&n| n > 0)
        .ok_or_else(|| "kimi token missing or non-positive expires_in".to_string())?;

    let scope = v
        .get("scope")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let token_type = v
        .get("token_type")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let expires_at = v.get("expires_at").and_then(|x| {
        x.as_i64()
            .or_else(|| x.as_u64().and_then(|u| i64::try_from(u).ok()))
            .or_else(|| x.as_f64().filter(|f| f.is_finite()).map(|f| f as i64))
            .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
    });

    Ok(ParsedTokenResponse {
        access_token: access,
        refresh_token: refresh,
        expires_in,
        scope,
        token_type,
        expires_at,
    })
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_url: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    verification_url_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

fn token_url() -> String {
    format!("{}{TOKEN_PATH}", oauth_host().trim_end_matches('/'))
}

fn device_url() -> String {
    format!("{}{DEVICE_PATH}", oauth_host().trim_end_matches('/'))
}

pub async fn start() -> Result<OAuthStart, String> {
    let client = http_client()?;
    let mut req = client
        .post(device_url())
        .header("Accept", "application/json")
        .form(&[("client_id", CLIENT_ID)]);
    for (k, v) in crate::providers::kimi::device_headers() {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("kimi device code: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Status only — never put raw OAuth response bodies into UI strings.
        return Err(format!("kimi device code http {status}"));
    }
    let body: DeviceCodeResponse = resp
        .json()
        .await
        .map_err(|e| format!("kimi device code decode: {e}"))?;

    let host = oauth_host();
    let verification_uri = body
        .verification_uri
        .or(body.verification_url)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{}/device", host.trim_end_matches('/')));
    let complete = body
        .verification_uri_complete
        .or(body.verification_url_complete)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("{verification_uri}?user_code={}", body.user_code));

    let expires_in = body.expires_in.unwrap_or(1800);
    let interval = body.interval.unwrap_or(5).max(1);
    let code = body.user_code.clone();
    let message =
        format!("If the browser asks for a code, enter: {code}  (then approve — we wait)");

    let session_id = new_session_id();
    insert_session(OAuthSession {
        id: session_id.clone(),
        provider: "kimi".into(),
        kind: SessionKind::KimiDevice {
            device_code: body.device_code,
        },
        kind_label: "device".into(),
        user_code: Some(code.clone()),
        verification_uri: Some(verification_uri.clone()),
        verification_uri_complete: Some(complete.clone()),
        authorize_url: None,
        message: message.clone(),
        expires_at: Instant::now() + Duration::from_secs(expires_in),
        phase: OAuthPhase::Pending,
        created_at: Instant::now(),
    })?;
    spawn_device_poller(session_id.clone(), interval);

    let _ = open_browser(&complete);

    Ok(OAuthStart {
        provider: "kimi".into(),
        session_id,
        kind: "device".into(),
        user_code: Some(code),
        verification_uri: Some(verification_uri),
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
        SessionKind::KimiDevice { device_code } => device_code.clone(),
        _ => return Err("not a kimi device session".into()),
    };

    let client = http_client()?;
    let mut req = client
        .post(token_url())
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", GRANT_DEVICE),
            ("device_code", device_code.as_str()),
            ("client_id", CLIENT_ID),
        ]);
    for (k, v) in crate::providers::kimi::device_headers() {
        req = req.header(k, v);
    }
    let resp = req.send().await.map_err(|e| format!("kimi token: {e}"))?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("kimi token body: {e}"))?;

    // Pending / slow_down / denied via OAuth error object (may be non-2xx).
    // Messages use OAuth error codes / status only — never error_description or body.
    if let Ok(err_obj) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(err) = err_obj.get("error").and_then(|e| e.as_str()) {
            return match err {
                "authorization_pending" | "slow_down" => Ok(OAuthPoll {
                    status: "pending".into(),
                    message: Some(session.message.clone()),
                    provider: Some("kimi".into()),
                    user_code: session.user_code.clone(),
                    session_id: Some(session.id.clone()),
                }),
                "expired_token" | "access_denied" => Ok(OAuthPoll {
                    status: "error".into(),
                    message: Some(err.to_string()),
                    provider: Some("kimi".into()),
                    user_code: session.user_code.clone(),
                    session_id: Some(session.id.clone()),
                }),
                other => {
                    // Only treat as OAuth error when access_token is absent.
                    if err_obj.get("access_token").is_none() {
                        Ok(OAuthPoll {
                            status: "error".into(),
                            message: Some(format!("{other} (http {status})")),
                            provider: Some("kimi".into()),
                            user_code: session.user_code.clone(),
                            session_id: Some(session.id.clone()),
                        })
                    } else {
                        // Fall through to success parse below.
                        finish_token_text(&text, session)
                    }
                }
            };
        }
    }

    if !status.is_success() {
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("kimi token http {status}")),
            provider: Some("kimi".into()),
            user_code: session.user_code.clone(),
            session_id: Some(session.id.clone()),
        });
    }

    finish_token_text(&text, session)
}

fn finish_token_text(text: &str, session: &OAuthSession) -> Result<OAuthPoll, String> {
    let parsed = parse_token_response(text)?;
    let expires_at = parsed
        .expires_at
        .unwrap_or_else(|| chrono::Utc::now().timestamp() + parsed.expires_in as i64);
    persist_credential_tokens(&TokenPersist {
        access_token: &parsed.access_token,
        refresh_token: Some(&parsed.refresh_token),
        expires_in: parsed.expires_in,
        expires_at: Some(expires_at),
        scope: parsed.scope.as_deref(),
        token_type: parsed.token_type.as_deref(),
    })?;

    Ok(OAuthPoll {
        status: "complete".into(),
        message: Some("Signed in to Kimi Code. Plan quotas will refresh on the next poll.".into()),
        provider: Some("kimi".into()),
        user_code: session.user_code.clone(),
        session_id: Some(session.id.clone()),
    })
}

pub fn logout() -> Result<String, String> {
    // Always clears CM session + sets no-import tombstone so a CLI file
    // cannot immediately re-mark the app as signed in.
    remove_credential_file()?;
    Ok("Kimi Code app sign-in cleared (CLI login unchanged)".into())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    #[test]
    fn parse_token_response_requires_and_preserves_oauth_tokens() {
        let body = r#"{
            "access_token": "kimi-access-token-abc",
            "refresh_token": "kimi-refresh-token-xyz",
            "expires_in": 3600,
            "scope": "openid profile offline_access",
            "token_type": "Bearer"
        }"#;

        let parsed = super::parse_token_response(body).expect("token response should parse");

        assert_eq!(parsed.access_token, "kimi-access-token-abc");
        assert_eq!(parsed.refresh_token, "kimi-refresh-token-xyz");
        assert_eq!(parsed.expires_in, 3600);
        assert_eq!(
            parsed.scope.as_deref(),
            Some("openid profile offline_access")
        );
        assert_eq!(parsed.token_type.as_deref(), Some("Bearer"));
        assert_eq!(
            Duration::from_secs(parsed.expires_in),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn parse_token_response_rejects_zero_expires_in() {
        let body = r#"{
            "access_token": "kimi-access-token-zero-exp",
            "refresh_token": "kimi-refresh-token-zero-exp",
            "expires_in": 0,
            "scope": "openid profile offline_access",
            "token_type": "Bearer"
        }"#;

        // Match Ok vs Err so a wrong Ok never Debug-prints fixture tokens.
        match super::parse_token_response(body) {
            Ok(_) => panic!("expires_in: 0 must be rejected"),
            Err(err) => {
                assert!(
                    err.to_lowercase().contains("expires_in"),
                    "error should mention expires_in, got: {err}"
                );
                assert!(
                    !err.contains("kimi-access-token-zero-exp"),
                    "error must not leak access token: {err}"
                );
                assert!(
                    !err.contains("kimi-refresh-token-zero-exp"),
                    "error must not leak refresh token: {err}"
                );
            }
        }
    }
}
