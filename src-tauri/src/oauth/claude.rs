//! Claude Code OAuth (subscription) via auth-code + PKCE.
//!
//! Claude's public client redirects to `platform.claude.com/oauth/code/callback`,
//! which shows a `CODE#STATE` string for the user to paste back — same as the CLI
//! when localhost callback is unavailable.
//!
//! Tokens are stored in Windows Credential Manager (app-only). The Claude CLI
//! keeps a separate login under `~/.claude/`.

use std::sync::atomic::{AtomicI64, Ordering};
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
    let meta = fetch_subscription_meta(&client, &access).await;
    let profile_read = meta.is_some();
    let (subscription_type, rate_limit_tiers) = meta.unwrap_or((None, None));

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

    let message = match (subscription_type.as_deref(), profile_read) {
        (Some(plan), _) => format!("Signed in to Claude ({plan} plan). Claude Code is now on the bar."),
        (None, true) => "Signed in, but this Claude account has no Pro/Max plan — Claude stays off the bar."
            .to_string(),
        (None, false) => {
            "Signed in, but the plan could not be read. Press Recheck to try again.".to_string()
        }
    };

    Ok(OAuthPoll {
        status: "complete".into(),
        message: Some(message),
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

/// `(subscription_type, rate_limit_tier)`; `None` from the fetch means the
/// profile could not be read at all (network / auth), as opposed to a readable
/// profile that simply has no paid plan.
type SubscriptionMeta = (Option<String>, Option<String>);

async fn fetch_subscription_meta(
    client: &reqwest::Client,
    access: &str,
) -> Option<SubscriptionMeta> {
    let resp = client
        .get(PROFILE_URL)
        .bearer_auth(access)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        log::warn!("claude oauth: profile http {}", resp.status());
        return None;
    }
    let v: Value = resp.json().await.ok()?;
    Some(subscription_from_profile(&v))
}

/// Map the `/api/oauth/profile` body to `(subscriptionType, rateLimitTier)`.
///
/// The endpoint has no `subscriptionType` field. The Claude CLI derives it
/// from `organization.organization_type` (`claude_max` → `max`, `claude_pro`
/// → `pro`, `claude_team` → `team`, `claude_enterprise` → `enterprise`), so we
/// do the same, falling back to `account.has_claude_max/pro`. Free accounts
/// (`claude_free`, or no flags) yield `None`.
fn subscription_from_profile(v: &Value) -> SubscriptionMeta {
    let explicit = v
        .get("subscriptionType")
        .or_else(|| v.pointer("/account/subscriptionType"))
        .or_else(|| v.pointer("/organization/subscriptionType"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let from_org = v
        .pointer("/organization/organization_type")
        .and_then(Value::as_str)
        .and_then(|t| match t {
            "claude_max" => Some("max"),
            "claude_pro" => Some("pro"),
            "claude_team" => Some("team"),
            "claude_enterprise" => Some("enterprise"),
            _ => None,
        })
        .map(str::to_string);

    let flag = |p: &str| v.pointer(p).and_then(Value::as_bool).unwrap_or(false);
    let from_flags = if flag("/account/has_claude_max") {
        Some("max".to_string())
    } else if flag("/account/has_claude_pro") {
        Some("pro".to_string())
    } else {
        None
    };

    let sub = explicit.or(from_org).or(from_flags);
    let tier = v
        .get("rateLimitTier")
        .or_else(|| v.pointer("/organization/rate_limit_tier"))
        .or_else(|| v.pointer("/account/rateLimitTier"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (sub, tier)
}

static LAST_PROFILE_ATTEMPT_MS: AtomicI64 = AtomicI64::new(0);
/// Free / undetected accounts are re-checked at most this often on the
/// background cycle; the manual Recheck bypasses it.
const PROFILE_RETRY_MS: i64 = 10 * 60 * 1000;

/// Keep the stored app session usable.
///
/// * Refreshes the access token (8h lifetime) shortly before it expires —
///   only for sessions this app created (`appManaged`), because refresh tokens
///   rotate and refreshing a session imported from the CLI would sign the CLI out.
/// * Re-reads the plan when `subscriptionType` is missing, which repairs
///   sessions stored before the profile parser understood `organization_type`
///   and picks up a plan bought after signing in.
///
/// Best-effort: failures are logged and leave the stored session untouched.
pub async fn ensure_session(force: bool) {
    let Some(mut root) = secrets::oauth_get_json("claude") else {
        return;
    };
    let app_managed = root.get("appManaged").and_then(Value::as_bool) == Some(true);
    let Some(oauth) = root
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Ok(client) = http_client() else {
        return;
    };

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut dirty = false;
    let mut refreshed = false;

    let expires_at = oauth.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
    if app_managed && expires_at > 0 && expires_at <= now_ms + 60_000 {
        let refresh = oauth
            .get("refreshToken")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(rt) = refresh {
            match refresh_access_token(&client, &rt).await {
                Ok(t) => {
                    oauth.insert("accessToken".into(), Value::String(t.access));
                    oauth.insert(
                        "expiresAt".into(),
                        json!(now_ms + (t.expires_in as i64) * 1000),
                    );
                    if let Some(new_rt) = t.refresh {
                        oauth.insert("refreshToken".into(), Value::String(new_rt));
                    }
                    if let Some(scopes) = t.scopes {
                        oauth.insert("scopes".into(), json!(scopes));
                    }
                    dirty = true;
                    refreshed = true;
                    log::info!("claude oauth: refreshed access token");
                }
                Err(e) => log::warn!("claude oauth: token refresh failed: {e}"),
            }
        }
    }

    let access = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let expires_at = oauth.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
    let token_live = !access.is_empty() && (expires_at <= 0 || expires_at > now_ms);
    let sub_missing = oauth
        .get("subscriptionType")
        .map_or(true, Value::is_null);
    let retry_due =
        force || now_ms - LAST_PROFILE_ATTEMPT_MS.load(Ordering::Relaxed) >= PROFILE_RETRY_MS;

    if token_live && (refreshed || (sub_missing && retry_due)) {
        LAST_PROFILE_ATTEMPT_MS.store(now_ms, Ordering::Relaxed);
        if let Some((sub, tier)) = fetch_subscription_meta(&client, &access).await {
            let new_sub = sub.map_or(Value::Null, Value::String);
            let new_tier = tier.map_or(Value::Null, Value::String);
            if oauth.get("subscriptionType") != Some(&new_sub)
                || oauth.get("rateLimitTier") != Some(&new_tier)
            {
                oauth.insert("subscriptionType".into(), new_sub);
                oauth.insert("rateLimitTier".into(), new_tier);
                dirty = true;
            }
        }
    }

    if dirty {
        if let Err(e) = secrets::oauth_set_json("claude", &root) {
            log::warn!("claude oauth: could not store updated session: {e}");
        }
    }
}

struct RefreshedToken {
    access: String,
    refresh: Option<String>,
    expires_in: u64,
    scopes: Option<Vec<String>>,
}

async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<RefreshedToken, String> {
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": SCOPES,
        }))
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    let body: TokenResponse = resp.json().await.map_err(|e| format!("decode: {e}"))?;
    if let Some(err) = body.error {
        return Err(format!("{err} (http {status})"));
    }
    if !status.is_success() {
        return Err(format!("http {status}"));
    }
    let access = body
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or("missing access_token")?;
    Ok(RefreshedToken {
        access,
        refresh: body.refresh_token.filter(|s| !s.is_empty()),
        expires_in: body.expires_in.unwrap_or(28_800),
        scopes: body
            .scope
            .map(|s| s.split_whitespace().map(str::to_string).collect()),
    })
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
    // Marks a session this app minted itself; only those may be refreshed (a
    // session imported from the CLI shares a rotating refresh token with it).
    root.insert("appManaged".into(), Value::Bool(true));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_max_org_maps_to_max_and_reads_tier() {
        let v = json!({
            "account": { "has_claude_max": true, "has_claude_pro": false },
            "organization": {
                "organization_type": "claude_max",
                "rate_limit_tier": "default_claude_max_5x"
            }
        });
        let (sub, tier) = subscription_from_profile(&v);
        assert_eq!(sub.as_deref(), Some("max"));
        assert_eq!(tier.as_deref(), Some("default_claude_max_5x"));
    }

    #[test]
    fn profile_org_types_map_to_plan_names() {
        for (org, want) in [
            ("claude_pro", "pro"),
            ("claude_team", "team"),
            ("claude_enterprise", "enterprise"),
        ] {
            let v = json!({ "organization": { "organization_type": org } });
            assert_eq!(subscription_from_profile(&v).0.as_deref(), Some(want));
        }
    }

    #[test]
    fn profile_falls_back_to_account_flags() {
        let v = json!({ "account": { "has_claude_pro": true } });
        assert_eq!(subscription_from_profile(&v).0.as_deref(), Some("pro"));
    }

    #[test]
    fn profile_free_account_has_no_subscription() {
        let v = json!({
            "account": { "has_claude_max": false, "has_claude_pro": false },
            "organization": { "organization_type": "claude_free" }
        });
        assert_eq!(subscription_from_profile(&v), (None, None));
    }

    #[test]
    fn profile_explicit_subscription_type_still_wins() {
        let v = json!({
            "subscriptionType": "max",
            "organization": { "organization_type": "claude_pro" }
        });
        assert_eq!(subscription_from_profile(&v).0.as_deref(), Some("max"));
    }
}
