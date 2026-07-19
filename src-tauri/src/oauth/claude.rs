//! Claude Code OAuth (subscription) via auth-code + PKCE.
//!
//! Claude's public client redirects to `platform.claude.com/oauth/code/callback`,
//! which shows a `CODE#STATE` string for the user to paste back — same as the CLI
//! when localhost callback is unavailable.
//!
//! Tokens are stored in Windows Credential Manager (app-only). The Claude CLI
//! keeps a separate login under `~/.claude/`.

use std::time::Instant;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::pkce::{code_challenge_s256, random_urlsafe};
use super::session::{OAuthPhase, OAuthSession, SessionKind};
use super::{http_client, insert_session, new_session_id, open_browser, OAuthPoll, OAuthStart};
use crate::secrets;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
const SCOPES: &str = "user:inference user:profile user:sessions:claude_code user:mcp_servers";

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// Intentionally unused — never surface to UI (may echo secrets).
    #[serde(default)]
    #[allow(dead_code)]
    error_description: Option<String>,
    #[serde(default)]
    account: Option<AccountInfo>,
    #[serde(default)]
    organization: Option<OrgInfo>,
}

#[derive(Deserialize)]
struct AccountInfo {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    email_address: Option<String>,
}

#[derive(Deserialize)]
struct OrgInfo {
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

pub async fn start() -> Result<OAuthStart, String> {
    let verifier = random_urlsafe(32);
    let challenge = code_challenge_s256(&verifier);
    let state = random_urlsafe(32);

    let mut url = url::Url::parse(AUTHORIZE_URL).map_err(|e| e.to_string())?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("code", "true");
        q.append_pair("client_id", CLIENT_ID);
        q.append_pair("response_type", "code");
        q.append_pair("redirect_uri", REDIRECT_URI);
        q.append_pair("scope", SCOPES);
        q.append_pair("code_challenge", &challenge);
        q.append_pair("code_challenge_method", "S256");
        q.append_pair("state", &state);
    }
    let authorize_url = url.to_string();
    let message = "In the browser, authorize Claude Code. Then paste the CODE#STATE value and press Complete.".to_string();

    let session_id = new_session_id();
    insert_session(OAuthSession {
        id: session_id.clone(),
        provider: "claude".into(),
        kind: SessionKind::ClaudeManual {
            code_verifier: verifier,
            state,
            redirect_uri: REDIRECT_URI.into(),
        },
        kind_label: "manual_code".into(),
        user_code: None,
        verification_uri: None,
        verification_uri_complete: None,
        authorize_url: Some(authorize_url.clone()),
        message: message.clone(),
        expires_at: Instant::now() + std::time::Duration::from_secs(600),
        phase: OAuthPhase::Pending,
        created_at: Instant::now(),
    })?;

    let _ = open_browser(&authorize_url);

    Ok(OAuthStart {
        provider: "claude".into(),
        session_id,
        kind: "manual_code".into(),
        user_code: None,
        verification_uri: None,
        verification_uri_complete: None,
        authorize_url: Some(authorize_url),
        expires_in: Some(600),
        message,
        status: "pending".into(),
    })
}

pub async fn complete(session: &OAuthSession, pasted: &str) -> Result<OAuthPoll, String> {
    let (verifier, expected_state, redirect_uri) = match &session.kind {
        SessionKind::ClaudeManual {
            code_verifier,
            state,
            redirect_uri,
        } => (code_verifier.clone(), state.clone(), redirect_uri.clone()),
        _ => return Err("not a claude manual session".into()),
    };

    let pasted = pasted.trim();
    if pasted.is_empty() {
        return Err("paste the CODE#STATE value from the browser".into());
    }

    // Accept "CODE#STATE", full redirect URL, or bare code.
    let (code, state_from_paste) = parse_pasted_code(pasted);
    if let Some(s) = state_from_paste {
        if s != expected_state {
            return Err("state mismatch — restart Sign in and use the new code".into());
        }
    }

    let client = http_client()?;
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier.as_str()),
            ("state", expected_state.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("claude token: {e}"))?;

    let status = resp.status();
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| format!("claude token decode: {e}"))?;

    if let Some(err) = body.error {
        // OAuth error code only — never error_description or raw body.
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("claude token error: {err} (http {status})")),
            provider: Some("claude".into()),
            user_code: None,
            session_id: Some(session.id.clone()),
        });
    }
    if !status.is_success() {
        return Ok(OAuthPoll {
            status: "error".into(),
            message: Some(format!("claude token http {status}")),
            provider: Some("claude".into()),
            user_code: None,
            session_id: Some(session.id.clone()),
        });
    }

    let access = body
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or("claude token missing access_token")?;
    let refresh = body.refresh_token;
    let expires_in = body.expires_in.unwrap_or(28_800);
    let scopes: Vec<String> = body
        .scope
        .as_deref()
        .unwrap_or(SCOPES)
        .split_whitespace()
        .map(str::to_string)
        .collect();

    // Best-effort profile for subscriptionType (needed for provider registration).
    let (subscription_type, rate_limit_tiers) = fetch_subscription_meta(&client, &access).await;

    persist_tokens(
        &access,
        refresh.as_deref(),
        expires_in,
        &scopes,
        subscription_type.as_deref(),
        rate_limit_tiers.as_deref(),
        body.account.as_ref(),
        body.organization.as_ref(),
    )?;

    Ok(OAuthPoll {
        status: "complete".into(),
        message: Some(
            "Signed in to Claude. Pro/Max plans show on the bar after Recheck; free plans stay local-only."
                .into(),
        ),
        provider: Some("claude".into()),
        user_code: None,
        session_id: Some(session.id.clone()),
    })
}

fn parse_pasted_code(pasted: &str) -> (String, Option<String>) {
    // Full callback URL?
    if pasted.contains("code=") {
        if let Ok(u) = url::Url::parse(pasted) {
            let mut code = None;
            let mut state = None;
            for (k, v) in u.query_pairs() {
                if k == "code" {
                    code = Some(v.into_owned());
                } else if k == "state" {
                    state = Some(v.into_owned());
                }
            }
            // Sometimes code is in the fragment: CODE#STATE
            if code.is_none() {
                if let Some(frag) = u.fragment() {
                    return split_code_state(frag);
                }
            }
            if let Some(c) = code {
                return (c, state);
            }
        }
    }
    split_code_state(pasted)
}

fn split_code_state(s: &str) -> (String, Option<String>) {
    if let Some((code, state)) = s.split_once('#') {
        (code.trim().to_string(), Some(state.trim().to_string()))
    } else {
        (s.trim().to_string(), None)
    }
}

async fn fetch_subscription_meta(
    client: &reqwest::Client,
    access: &str,
) -> (Option<String>, Option<String>) {
    let resp = match client
        .get(PROFILE_URL)
        .bearer_auth(access)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return (None, None),
    };
    if !resp.status().is_success() {
        return (None, None);
    }
    let v: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let sub = v
        .get("subscriptionType")
        .or_else(|| v.pointer("/account/subscriptionType"))
        .or_else(|| v.pointer("/organization/subscriptionType"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let tiers = v
        .get("rateLimitTier")
        .or_else(|| v.pointer("/account/rateLimitTier"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    (sub, tiers)
}

fn persist_tokens(
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: u64,
    scopes: &[String],
    subscription_type: Option<&str>,
    rate_limit_tiers: Option<&str>,
    account: Option<&AccountInfo>,
    org: Option<&OrgInfo>,
) -> Result<(), String> {
    // App-only session in Credential Manager — never writes ~/.claude/.credentials.json
    // or ~/.claude.json (CLI keeps its own login).
    let mut root: Map<String, Value> = secrets::oauth_get_json("claude")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let expires_at = chrono::Utc::now().timestamp_millis() + (expires_in as i64) * 1000;
    let mut oauth = json!({
        "accessToken": access_token,
        "expiresAt": expires_at,
        "scopes": scopes,
        "subscriptionType": subscription_type,
        "rateLimitTier": rate_limit_tiers,
    });
    if let Some(rt) = refresh_token {
        oauth
            .as_object_mut()
            .unwrap()
            .insert("refreshToken".into(), Value::String(rt.to_string()));
    }
    root.insert("claudeAiOauth".into(), oauth);

    // Keep account metadata inside the app blob (not in ~/.claude.json).
    if let (Some(acc), Some(o)) = (account, org) {
        if let (Some(au), Some(email), Some(ou)) = (
            acc.uuid.as_ref(),
            acc.email_address.as_ref(),
            o.uuid.as_ref(),
        ) {
            root.insert(
                "oauthAccount".into(),
                json!({
                    "accountUuid": au,
                    "emailAddress": email,
                    "organizationUuid": ou,
                    "organizationName": o.name,
                }),
            );
        }
    }

    secrets::oauth_set_json("claude", &Value::Object(root))
        .map_err(|e| format!("store claude oauth: {e}"))?;
    log::info!("claude oauth: stored session in Credential Manager");
    Ok(())
}

pub fn logout() -> Result<String, String> {
    secrets::oauth_delete("claude").map_err(|e| e.to_string())?;
    Ok("Claude app sign-in cleared (CLI login unchanged)".into())
}
