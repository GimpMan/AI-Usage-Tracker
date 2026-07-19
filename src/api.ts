import { invoke } from "@tauri-apps/api/core";
import type {
  ProviderBurnHistory,
  ProviderId,
  ProviderStatus,
  UpdateChannel,
  UpdateState,
  UsageSnapshot,
} from "./types";

export async function getUpdateState(): Promise<UpdateState> {
  return invoke<UpdateState>("get_update_state");
}

export async function checkForUpdate(manual: boolean): Promise<UpdateState> {
  return invoke<UpdateState>("check_for_update", { manual });
}

export async function installUpdate(): Promise<void> {
  return invoke<void>("install_update");
}

export async function setUpdateChannel(channel: UpdateChannel): Promise<UpdateState> {
  return invoke<UpdateState>("set_update_channel", { channel });
}

export async function getUsage(): Promise<UsageSnapshot[]> {
  return invoke<UsageSnapshot[]>("get_usage");
}

/** Bucketed burn history for the popup burn bars (weekly + 5h windows). */
export async function getBurnHistory(): Promise<ProviderBurnHistory[]> {
  return invoke<ProviderBurnHistory[]>("get_burn_history");
}

export async function saveKey(provider: ProviderId, key: string): Promise<void> {
  await invoke("save_key", { provider, key });
}

/** Whether a key is stored. Never returns secret material. */
export async function loadKey(provider: ProviderId): Promise<boolean> {
  return invoke<boolean>("load_key", { provider });
}

export async function deleteKey(provider: ProviderId): Promise<void> {
  await invoke("delete_key", { provider });
}

export async function testKey(
  provider: ProviderId,
  key?: string,
  region?: string,
): Promise<string> {
  return invoke<string>("test_key", {
    provider,
    key: key ?? null,
    region: region ?? null,
  });
}

export async function refreshNow(): Promise<void> {
  await invoke("refresh_now");
}

/** Result of a single-provider refresh (popup status line). */
export interface RefreshProviderResult {
  ok: boolean;
  message: string;
  health: string | null;
}

/** Re-fetch usage for a single provider (overlay popup refresh control). */
export async function refreshProvider(
  provider: ProviderId,
): Promise<RefreshProviderResult> {
  return invoke<RefreshProviderResult>("refresh_provider", { provider });
}

export async function saveRegion(provider: ProviderId, region: string): Promise<void> {
  await invoke("save_region", { provider, region });
}

export async function loadRegion(provider: ProviderId): Promise<string | null> {
  return invoke<string | null>("load_region", { provider });
}

export async function getStatus(): Promise<ProviderStatus[]> {
  return invoke<ProviderStatus[]>("get_status");
}

export async function setProviderHidden(provider: ProviderId, hidden: boolean): Promise<void> {
  await invoke("set_provider_hidden", { provider, hidden });
}

export async function saveOverlayPosition(): Promise<void> {
  await invoke("save_overlay_position");
}

export async function getAutostartEnabled(): Promise<boolean> {
  return invoke<boolean>("get_autostart_enabled");
}

export async function setAutostartEnabled(enabled: boolean): Promise<void> {
  await invoke("set_autostart_enabled", { enabled });
}

/** Whether quota red-line / run-out notifications are enabled. */
export async function getNotificationsEnabled(): Promise<boolean> {
  return invoke<boolean>("get_notifications_enabled");
}

/** Enable or disable quota red-line / run-out notifications. */
export async function setNotificationsEnabled(enabled: boolean): Promise<void> {
  await invoke("set_notifications_enabled", { enabled });
}

/** Read the persisted refresh interval (seconds). */
export async function getRefreshInterval(): Promise<number> {
  return invoke<number>("get_refresh_interval");
}

/** Persist the refresh interval (seconds) and broadcast
 *  `refresh-interval-changed`. */
export async function setRefreshInterval(secs: number): Promise<void> {
  await invoke("set_refresh_interval", { secs });
}

/** Hide overlay + settings; keep running in the tray. */
export async function hideToTray(): Promise<void> {
  await invoke("hide_to_tray");
}

/** Fully quit the app (tray + background scheduler). */
export async function quitApp(): Promise<void> {
  await invoke("quit_app");
}

export interface OAuthStart {
  provider: string;
  sessionId: string;
  kind: string;
  userCode?: string | null;
  verificationUri?: string | null;
  verificationUriComplete?: string | null;
  authorizeUrl?: string | null;
  expiresIn?: number | null;
  message: string;
  /** pending | complete | error — present when restoring an active session. */
  status?: string | null;
}

export interface OAuthPoll {
  status: string;
  message?: string | null;
  provider?: string | null;
}

/** Normalize IPC payload whether serde used camelCase or snake_case. */
function normalizeOauthStart(raw: Record<string, unknown>): OAuthStart {
  const str = (a: string, b: string) =>
    (typeof raw[a] === "string" ? (raw[a] as string) : null) ??
    (typeof raw[b] === "string" ? (raw[b] as string) : null);
  const num = (a: string, b: string) => {
    const v = raw[a] ?? raw[b];
    return typeof v === "number" ? v : null;
  };
  return {
    provider: str("provider", "provider") ?? "",
    sessionId: str("sessionId", "session_id") ?? "",
    kind: str("kind", "kind") ?? "",
    userCode: str("userCode", "user_code"),
    verificationUri: str("verificationUri", "verification_uri"),
    verificationUriComplete: str(
      "verificationUriComplete",
      "verification_uri_complete",
    ),
    authorizeUrl: str("authorizeUrl", "authorize_url"),
    expiresIn: num("expiresIn", "expires_in"),
    message: str("message", "message") ?? "",
    status: str("status", "status"),
  };
}

function normalizeOauthPoll(raw: Record<string, unknown>): OAuthPoll {
  const str = (a: string, b: string) =>
    (typeof raw[a] === "string" ? (raw[a] as string) : null) ??
    (typeof raw[b] === "string" ? (raw[b] as string) : null);
  return {
    status: str("status", "status") ?? "error",
    message: str("message", "message"),
    provider: str("provider", "provider"),
  };
}

export async function startOauthLogin(provider: ProviderId): Promise<OAuthStart> {
  const raw = await invoke<Record<string, unknown>>("start_oauth_login", { provider });
  return normalizeOauthStart(raw ?? {});
}

export async function getActiveOauth(
  provider: ProviderId,
): Promise<OAuthStart | null> {
  const raw = await invoke<Record<string, unknown> | null>("get_active_oauth", {
    provider,
  });
  if (!raw) return null;
  return normalizeOauthStart(raw);
}

export async function pollOauthLogin(sessionId: string): Promise<OAuthPoll> {
  const raw = await invoke<Record<string, unknown>>("poll_oauth_login", { sessionId });
  return normalizeOauthPoll(raw ?? {});
}

export async function completeOauthLogin(
  sessionId: string,
  code: string,
): Promise<OAuthPoll> {
  const raw = await invoke<Record<string, unknown>>("complete_oauth_login", {
    sessionId,
    code,
  });
  return normalizeOauthPoll(raw ?? {});
}

export async function cancelOauthLogin(sessionId: string): Promise<void> {
  await invoke("cancel_oauth_login", { sessionId });
}

export async function oauthLogout(provider: ProviderId): Promise<string> {
  return invoke<string>("oauth_logout", { provider });
}

/** Open an external https URL in the system default browser. */
export async function openExternal(url: string): Promise<void> {
  await invoke("open_external", { url });
}

export async function saveOpenrouterManagementKey(key: string): Promise<void> {
  await invoke("save_openrouter_management_key", { key });
}

/** Whether a management key is stored. Never returns secret material. */
export async function loadOpenrouterManagementKey(): Promise<boolean> {
  return invoke<boolean>("load_openrouter_management_key");
}

export async function deleteOpenrouterManagementKey(): Promise<void> {
  await invoke("delete_openrouter_management_key");
}

export async function testOpenrouterManagementKey(key?: string): Promise<string> {
  return invoke<string>("test_openrouter_management_key", { key: key ?? null });
}

/** Store the current OpenRouter account balance as a local tracking budget. */
export async function rebaseOpenrouterAccount(): Promise<string> {
  return invoke<string>("rebase_openrouter_account");
}
