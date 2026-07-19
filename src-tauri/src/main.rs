// Hide the black console window in release builds. Dev (`cargo run` / `tauri
// dev`) still gets a console so logs stay visible while debugging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod commands;
mod history;
mod oauth;
mod overlay;
mod providers;
mod scheduler;
mod secrets;
mod tray_tooltip;
mod updates;
mod win32;

use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tokio::sync::RwLock;

use commands::secrets_handle;
use providers::{
    claude::ClaudeProvider, codex::CodexProvider, glm::GlmProvider, grok::GrokProvider,
    kimi::KimiProvider, minimax::MinimaxProvider, openrouter::OpenrouterProvider, Provider,
};
use scheduler::Scheduler;

pub struct AppState {
    pub snapshots: scheduler::SnapshotMap,
    pub health: scheduler::HealthMap,
    pub history: history::HistoryMap,
}

/// Detect an active Claude Pro/Max subscription for the **app** OAuth session.
///
/// Sessions live in Windows Credential Manager (`oauth_claude`). A free-tier
/// account can still have a live token, so a non-empty `accessToken` alone is
/// *not* enough. The discriminator is `claudeAiOauth.subscriptionType`:
/// Anthropic writes a non-null value (`"pro"`, `"max"`, etc.) only on paid
/// plans. We also re-check `expiresAt` so a cancelled plan with a stale
/// session does not register.
///
/// One-time import: if CM is empty, copy from `~/.claude/.credentials.json`
/// without modifying that CLI file.
fn has_active_claude_subscription() -> bool {
    let legacy = dirs::home_dir().map(|h| h.join(".claude").join(".credentials.json"));
    let value = match legacy.as_ref() {
        Some(path) => crate::secrets::oauth_get_json_or_import_file("claude", path),
        None => crate::secrets::oauth_get_json("claude"),
    };
    let Some(value) = value else {
        return false;
    };
    let Some(oauth) = value.get("claudeAiOauth") else {
        return false;
    };

    // Free tier / no subscription: subscriptionType is missing or null.
    match oauth.get("subscriptionType") {
        None | Some(serde_json::Value::Null) => return false,
        Some(_) => {}
    }

    let token_ok = oauth
        .get("accessToken")
        .and_then(|t| t.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !token_ok {
        return false;
    }

    if let Some(expires_at) = oauth.get("expiresAt").and_then(|e| e.as_i64()) {
        if expires_at > 0 && expires_at <= chrono::Utc::now().timestamp_millis() {
            return false;
        }
    }

    true
}

/// Build the provider list. Z.ai Coding Plan + MiniMax Coding Plan (live) + Codex CLI (local)
/// + Grok SuperGrok/Build (local OIDC auth) + OpenRouter (prepaid credits via
/// bearer key). Claude Code is only included when an active OAuth subscription
/// is detected — otherwise stale session files from a lapsed plan would make
/// the bar render an irrelevant "no recent usage" segment.
pub fn build_providers() -> Vec<Box<dyn Provider>> {
    let mut providers: Vec<Box<dyn Provider>> = vec![
        Box::new(GlmProvider),
        Box::new(MinimaxProvider),
        Box::new(CodexProvider),
        Box::new(GrokProvider),
        Box::new(KimiProvider),
        Box::new(OpenrouterProvider),
    ];
    if has_active_claude_subscription() {
        providers.push(Box::new(ClaudeProvider));
    }
    providers
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init()
        .ok();

    let snapshots: scheduler::SnapshotMap = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let health: scheduler::HealthMap = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let history: history::HistoryMap = Arc::new(RwLock::new(std::collections::HashMap::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .manage(updates::UpdateManager::default())
        .manage(AppState {
            snapshots: snapshots.clone(),
            health: health.clone(),
            history: history.clone(),
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_usage,
            commands::save_key,
            commands::save_openrouter_management_key,
            commands::load_openrouter_management_key,
            commands::delete_openrouter_management_key,
            commands::test_openrouter_management_key,
            commands::rebase_openrouter_account,
            commands::load_key,
            commands::delete_key,
            commands::save_region,
            commands::load_region,
            commands::test_key,
            commands::refresh_now,
            commands::refresh_provider,
            commands::set_overlay_geometry,
            commands::clamp_overlay_position,
            commands::set_overlay_width,
            commands::set_overlay_hit_height,
            commands::get_status,
            commands::set_provider_hidden,
            commands::save_overlay_position,
            commands::get_autostart_enabled,
            commands::set_autostart_enabled,
            commands::get_refresh_interval,
            commands::set_refresh_interval,
            commands::get_notifications_enabled,
            commands::set_notifications_enabled,
            commands::set_overlay_click_through,
            commands::enforce_overlay_borderless,
            commands::start_oauth_login,
            commands::open_external,
            commands::poll_oauth_login,
            commands::get_active_oauth,
            commands::list_active_oauth,
            commands::complete_oauth_login,
            commands::cancel_oauth_login,
            commands::oauth_logout,
            commands::hide_to_tray,
            commands::quit_app,
            commands::get_update_state,
            commands::check_for_update,
            commands::install_update,
            commands::set_update_channel,
            commands::get_burn_history,
        ])
        .setup({
            let snapshots = snapshots.clone();
            let health = health.clone();
            move |app| {
                // Tray: left-click opens the bar. Right-click is Open / Hide / Quit
                // only — Settings lives exclusively on the bar gear icon (embedded
                // popup), never as a separate tray-launched window.
                let open_item = MenuItem::with_id(app, "open", "Open Tracker", true, None::<&str>)?;
                let hide_item = MenuItem::with_id(app, "hide", "Hide to Tray", true, None::<&str>)?;
                let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&open_item, &hide_item, &quit_item])?;

                let _tray = TrayIconBuilder::with_id("main")
                    .tooltip("AI Usage Tracker")
                    .icon(app.default_window_icon().cloned().ok_or_else(|| {
                        tauri::Error::Anyhow(anyhow::anyhow!("no default window icon"))
                    })?)
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let app = tray.app_handle();
                            if let Err(e) = overlay::open_overlay(app) {
                                log::error!("open_overlay: {e}");
                            }
                        }
                    })
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "open" => {
                            if let Err(e) = overlay::open_overlay(app) {
                                log::error!("open_overlay: {e}");
                            }
                        }
                        "hide" => {
                            overlay::hide_to_tray(app);
                        }
                        "quit" => {
                            overlay::quit_cleanly(app);
                        }
                        _ => {}
                    })
                    .build(app)?;

                // Boot scheduler. 60s interval, in line with GLM & MiniMax
                // safe cadences (see plan §3).
                let secrets = secrets_handle();
                let sched = Scheduler::new(
                    snapshots.clone(),
                    health.clone(),
                    history.clone(),
                    secrets,
                    app.handle().clone(),
                );
                tauri::async_runtime::spawn(async move {
                    // Warm cache from disk so first open is instant.
                    scheduler::load_persisted(&snapshots).await;
                    crate::history::load_history(&history).await;
                    sched.run().await;
                });

                // Default to opening the bar at launch — the user wanted the
                // tracker visible without having to click the tray. Tray still
                // works as the manual "Hide to tray" / "Open Tracker" surface.
                if let Err(e) = overlay::open_overlay(&app.handle().clone()) {
                    log::error!("open_overlay at startup: {e}");
                }

                let update_app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    use tauri::Manager;
                    let manager = update_app.state::<updates::UpdateManager>();
                    if let Err(error) = manager.check(&update_app, false).await {
                        log::debug!("automatic update check did not complete: {error}");
                    }
                });

                Ok(())
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AI Usage Tracker");
}
