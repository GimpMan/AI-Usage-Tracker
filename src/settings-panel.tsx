import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type { ProviderId, ProviderStatus, UpdateChannel } from "./types";
import { checkboxDisabled } from "./provider-visibility";
import {
  cancelOauthLogin,
  checkForUpdate,
  completeOauthLogin,
  deleteOpenrouterManagementKey,
  deleteKey,
  getActiveOauth,
  getAutostartEnabled,
  getNotificationsEnabled,
  getRefreshInterval,
  getStatus,
  hideToTray,
  installUpdate,
  loadOpenrouterManagementKey,
  loadKey,
  loadRegion,
  oauthLogout,
  openExternal,
  pollOauthLogin,
  quitApp,
  rebaseOpenrouterAccount,
  refreshNow,
  saveOpenrouterManagementKey,
  saveKey,
  saveRegion,
  setAutostartEnabled,
  setNotificationsEnabled,
  setProviderHidden,
  setRefreshInterval,
  setUpdateChannel,
  startOauthLogin,
  testKey,
  testOpenrouterManagementKey,
  type OAuthStart,
} from "./api";
import { useUpdateState } from "./update-state";
import { clampUpdateProgressPercent } from "./update-state-logic";

interface ProviderMeta {
  id: ProviderId;
  label: string;
  hint: string;
  placeholder: string;
  hasRegion?: boolean;
  /** Supports in-app OAuth (device code or paste-code flow). */
  oauth?: boolean;
}

const PROVIDERS: ProviderMeta[] = [
  {
    id: "claude",
    label: "Claude Code",
    hint: "Sign in here (app session stored encrypted in Windows Credential Manager — separate from the Claude CLI). Requires Pro or Max. Shows recent local token use from project logs — not live rate-limit %.",
    placeholder: "",
    oauth: true,
  },
  {
    id: "codex",
    label: "OpenAI Codex CLI",
    hint: "Sign in with ChatGPT here (app session in Windows Credential Manager — separate from the Codex CLI). Live primary and secondary rate limits — no API key.",
    placeholder: "",
    oauth: true,
  },
  {
    id: "grok",
    label: "Grok (SuperGrok / Build)",
    hint: "Sign in with SuperGrok here (app session in Windows Credential Manager — separate from the Grok CLI). Tracks the monthly SuperGrok pool (and credits in the popup when available).",
    placeholder: "",
    oauth: true,
  },
  {
    id: "kimi",
    label: "Kimi Code",
    hint: "Sign in with Kimi Code here (app session in Windows Credential Manager — separate from the Kimi CLI). Tracks 5-hour and 7-day Kimi Code plan quotas — no API key.",
    placeholder: "",
    oauth: true,
  },
  {
    id: "minimax",
    label: "MiniMax Coding Plan",
    hint: "Coding Plan or Token Plan key (platform.minimax.io or minimaxi.com). Tracks 5-hour and weekly quotas — set Region to match where the key was issued.",
    placeholder: "coding-plan-key",
    hasRegion: true,
  },
  {
    id: "glm",
    label: "Z.ai Coding Plan",
    hint: "API key from z.ai → Manage API Key. Minibar shows 5-hour and weekly coding windows; monthly tool quota appears only in the popup.",
    placeholder: "your-z.ai-api-key",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    hint: "API key (sk-or-v1-…) for per-key limits (daily, weekly, monthly, or lifetime). Optional Management key for account balance, top-ups, and rebase.",
    placeholder: "sk-or-v1-...",
  },
];

/** Only allow https (and http://localhost) for OAuth links rendered in the webview. */
function safeHttpsHref(href: string | null | undefined): string | null {
  if (!href) return null;
  try {
    const u = new URL(href);
    if (u.protocol === "https:") return href;
    if (
      u.protocol === "http:" &&
      (u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "[::1]")
    ) {
      return href;
    }
  } catch {
    /* invalid */
  }
  return null;
}

/** Instant paint of the "Overlay" toggle before getStatus returns. Updated
 *  whenever we learn the real flag from disk so the next open never flashes. */
const HIDDEN_CACHE_KEY = "ai-usage-tracker:hidden-v1";

function readHiddenCache(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(HIDDEN_CACHE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object") return {};
    return parsed as Record<string, boolean>;
  } catch {
    return {};
  }
}

function writeHiddenCache(id: string, hidden: boolean) {
  const next = readHiddenCache();
  next[id] = hidden;
  try {
    localStorage.setItem(HIDDEN_CACHE_KEY, JSON.stringify(next));
  } catch {
    /* ignore quota */
  }
}

function writeHiddenCacheAll(statuses: ProviderStatus[]) {
  const next = readHiddenCache();
  for (const s of statuses) next[s.id] = s.hidden;
  try {
    localStorage.setItem(HIDDEN_CACHE_KEY, JSON.stringify(next));
  } catch {
    /* ignore quota */
  }
}

function ProviderSection({
  meta,
  status,
  onStatusChange,
  statusReady,
}: {
  meta: ProviderMeta;
  status?: ProviderStatus;
  onStatusChange?: () => void | Promise<void>;
  /** False until the first getStatus completes — checkbox stays disabled so we
   *  never flash the wrong tick while the saved flag is still in flight. */
  statusReady: boolean;
}) {
  const [key, setKey] = useState("");
  const [region, setRegion] = useState("overseas");
  /** True when a key is stored in the backend — secrets never enter the webview. */
  const [hasSavedKey, setHasSavedKey] = useState(false);
  const [managementKey, setManagementKey] = useState("");
  const [hasSavedManagementKey, setHasSavedManagementKey] = useState(false);
  const [savedRegion, setSavedRegion] = useState("");
  // Optimistic override while a hide/show request is in flight. Falls back to
  // backend status, then the localStorage cache of the last known flag.
  const [hiddenOverride, setHiddenOverride] = useState<boolean | null>(null);
  const [cachedHidden] = useState(() => {
    const v = readHiddenCache()[meta.id];
    return typeof v === "boolean" ? v : null;
  });
  // Not registered (e.g. Claude with no Pro/Max session) can never appear on
  // the bar — force the checkbox off regardless of a stale "shown" preference.
  const forceHidden = statusReady && !!status && !status.eligible;
  const userHidden = hiddenOverride ?? status?.hidden ?? cachedHidden ?? false;
  const hidden = forceHidden || userHidden;
  // Only dim the card when the user intentionally hid a *registered* provider.
  // Unregistered (Claude free / not detected) used to set is-hidden too, which
  // greyed out Sign in / Sign out and looked like the controls were disabled.
  const dimSection = !forceHidden && userHidden;
  /** Shared status line for every provider card (OAuth + API-key). */
  const [status_, setStatus] = useState<{
    text: string;
    kind: "ok" | "err" | "";
    /** When true, do not auto-clear (e.g. device-code prompt still needed). */
    sticky?: boolean;
  }>({ text: "", kind: "" });
  const [busy, setBusy] = useState(false);
  const [oauth, setOauth] = useState<OAuthStart | null>(null);
  const [oauthCode, setOauthCode] = useState("");
  const [oauthBusy, setOauthBusy] = useState(false);
  /** Bumped on unmount / cancel / new sign-in so device-poll loops exit. */
  const oauthWatchGen = useRef(0);

  // Applies to ALL providers in this card component (Claude, Codex, Grok, Kimi,
  // MiniMax, Z.ai, OpenRouter): success banners auto-clear; errors stay until
  // the next action; sticky prompts (device code) stay until replaced.
  useEffect(() => {
    if (status_.kind !== "ok" || !status_.text.trim() || status_.sticky) return;
    const t = window.setTimeout(() => {
      setStatus({ text: "", kind: "" });
    }, 5000);
    return () => window.clearTimeout(t);
  }, [status_.kind, status_.text, status_.sticky]);

  async function onLoad() {
    try {
      // Presence only — full secrets stay in the Rust backend.
      setHasSavedKey(await loadKey(meta.id));
      setKey("");
      if (meta.id === "openrouter") {
        setHasSavedManagementKey(await loadOpenrouterManagementKey());
        setManagementKey("");
      }
      if (meta.hasRegion) {
        const r = await loadRegion(meta.id);
        const regionValue = r ?? "overseas";
        setRegion(regionValue);
        setSavedRegion(regionValue);
      }
    } catch (e) {
      console.error(e);
    }
  }

  useEffect(() => {
    void onLoad();
    return () => {
      // Invalidate any in-flight device-poll UI loop for this card.
      oauthWatchGen.current += 1;
    };
  }, []);

  // Restore an in-progress OAuth login after Settings closes/reopens.
  // Device codes live in the Rust process — not in React state — so reopening
  // the window can show the same code and keep watching for completion.
  useEffect(() => {
    if (!meta.oauth) return;
    let cancelled = false;
    let pollTimer: number | undefined;

    async function restoreAndWatch() {
      try {
        const active = await getActiveOauth(meta.id);
        if (cancelled || !active?.sessionId) return;

        const deviceCode = (active.userCode ?? "").trim();
        if (active.status === "complete") {
          // Login finished while Settings was closed (background poller).
          // Do not leave the paste/code panel open.
          void (async () => {
            if (cancelled) return;
            setOauth(null);
            setOauthCode("");
            setOauthBusy(false);
            setStatus({
              text: active.message || "Signed in.",
              kind: "ok",
            });
            try {
              await refreshNow();
              await onStatusChange?.();
              window.dispatchEvent(new CustomEvent("ai-usage-refresh"));
            } catch {
              /* best-effort */
            }
          })();
          return;
        }
        if (active.status === "error") {
          // Finished with an error — show message, hide the in-progress panel.
          setOauth(null);
          setOauthCode("");
          setOauthBusy(false);
          setStatus({
            text: active.message || "Sign-in failed.",
            kind: "err",
          });
          return;
        }

        // Pending only: restore the in-progress UI.
        setOauth(active);
        setStatus({
          text: deviceCode
            ? `Type this code in the browser: ${deviceCode}`
            : active.message || "Sign-in in progress…",
          kind: "",
        });

        if (active.kind !== "device") return;

        // Backend also polls; this loop only keeps the UI in sync.
        const sessionId = active.sessionId;
        const tick = async () => {
          if (cancelled) return;
          try {
            const poll = await pollOauthLogin(sessionId);
            if (cancelled) return;
            if (poll.status === "complete") {
              await finishOauthSuccess(poll.message ?? "Signed in.");
              return;
            }
            if (poll.status === "error") {
              setOauth(null);
              const msg = poll.message ?? "Sign-in failed.";
              if (/unknown or expired/i.test(msg)) {
                setStatus({ text: "Sign-in cancelled.", kind: "" });
              } else {
                setStatus({ text: msg, kind: "err" });
              }
              return;
            }
            setStatus({
              text: deviceCode
                ? `Type this code in the browser: ${deviceCode}  (waiting for approval…)`
                : poll.message || "Waiting for browser approval…",
              kind: "",
            });
            pollTimer = window.setTimeout(() => void tick(), 3000);
          } catch (e) {
            if (!cancelled) setStatus({ text: String(e), kind: "err" });
          }
        };
        pollTimer = window.setTimeout(() => void tick(), 1500);
      } catch (e) {
        console.error("restore oauth", e);
      }
    }

    void restoreAndWatch();
    return () => {
      cancelled = true;
      if (pollTimer !== undefined) window.clearTimeout(pollTimer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [meta.id, meta.oauth]);

  // Drop the optimistic override once backend status matches it, and keep the
  // localStorage cache warm so the next settings open paints correctly.
  useEffect(() => {
    if (status) writeHiddenCache(meta.id, status.hidden);
    if (hiddenOverride === null || !status) return;
    if (status.hidden === hiddenOverride) setHiddenOverride(null);
  }, [status?.hidden, hiddenOverride, meta.id]);

  // Mirror backend auto-hide: when a provider is not eligible, keep the
  // local cache/override on "hidden" so the checkbox doesn't sit ticked.
  useEffect(() => {
    if (!status || status.eligible) return;
    writeHiddenCache(meta.id, true);
    if (hiddenOverride === false) setHiddenOverride(true);
  }, [status?.eligible, status?.hidden, meta.id, hiddenOverride]);

  // Never leave a green "shown" banner under a provider that is forced off
  // (not detected) — that message is leftover from a failed/racy toggle.
  useEffect(() => {
    if (!forceHidden) return;
    if (
      status_.kind === "ok" &&
      /shown in overlay/i.test(status_.text)
    ) {
      setStatus({ text: "", kind: "" });
    }
  }, [forceHidden, status_.kind, status_.text]);

  async function onTest() {
    setBusy(true);
    setStatus({ text: "Testing…", kind: "" });
    try {
      const trimmed = key.trim();
      // Empty draft → backend tests the stored key (never sent to webview).
      const draft = trimmed || undefined;
      // MiniMax: only probe the region currently selected in the dropdown
      // (overseas = minimax.io, china = minimaxi.com) — never both.
      const msg = meta.hasRegion
        ? await testKey(meta.id, draft, region)
        : await testKey(meta.id, draft);
      setStatus({ text: msg, kind: "ok" });
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onSave() {
    setBusy(true);
    try {
      const trimmed = key.trim();
      if (needsKey && !trimmed && !hasSavedKey) {
        setStatus({ text: "Key is empty.", kind: "err" });
        return;
      }
      if (needsKey && trimmed) {
        await saveKey(meta.id, trimmed);
        setHasSavedKey(true);
        setKey("");
      }
      if (meta.id === "openrouter" && managementKey.trim()) {
        await saveOpenrouterManagementKey(managementKey.trim());
        setHasSavedManagementKey(true);
        setManagementKey("");
      }
      if (meta.hasRegion) {
        await saveRegion(meta.id, region);
        setSavedRegion(region);
      }
      setStatus({ text: "Saved.", kind: "ok" });
      await refreshNow();
      await onStatusChange?.();
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onClear() {
    setBusy(true);
    try {
      await deleteKey(meta.id);
      setKey("");
      setHasSavedKey(false);
      setStatus({ text: "Cleared.", kind: "ok" });
      await refreshNow();
      await onStatusChange?.();
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onTestManagementKey() {
    setBusy(true);
    setStatus({ text: "Testing Management key…", kind: "" });
    try {
      // Empty draft → backend tests the stored management key.
      const msg = await testOpenrouterManagementKey(managementKey.trim() || undefined);
      setStatus({ text: msg, kind: "ok" });
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onSaveManagementKey() {
    const trimmed = managementKey.trim();
    if (!trimmed) {
      setStatus({ text: "Management key is empty.", kind: "err" });
      return;
    }
    setBusy(true);
    try {
      await saveOpenrouterManagementKey(trimmed);
      setHasSavedManagementKey(true);
      setManagementKey("");
      setStatus({ text: "Management key saved.", kind: "ok" });
      await refreshNow();
      await onStatusChange?.();
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onClearManagementKey() {
    setBusy(true);
    try {
      await deleteOpenrouterManagementKey();
      setManagementKey("");
      setHasSavedManagementKey(false);
      setStatus({ text: "Management key cleared.", kind: "ok" });
      await refreshNow();
      await onStatusChange?.();
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onRebaseAccount() {
    if (meta.id !== "openrouter" || !hasSavedManagementKey) return;
    setBusy(true);
    try {
      const message = await rebaseOpenrouterAccount();
      setStatus({ text: `${message}.`, kind: "ok" });
      await refreshNow();
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function onToggleHidden(next: boolean) {
    // `next` is the new `hidden` value (true = hide from overlay).
    // Cannot show a provider that isn't registered / detected.
    if (!statusReady) return;
    if (!next && (!status || !status.eligible)) {
      setHiddenOverride(true);
      writeHiddenCache(meta.id, true);
      setStatus({
        text: "Not detected yet — use Recheck after signing in.",
        kind: "err",
      });
      return;
    }
    setHiddenOverride(next);
    writeHiddenCache(meta.id, next);
    try {
      // Backend persists the flag, mutates snapshots, and emits a Tauri event
      // so the standalone Settings window can update the minibar too.
      await setProviderHidden(meta.id, next);
      // Same-webview fallback for the embedded Settings popup (mirrors the
      // refresh-interval dual-path). Harmless double-pull when Tauri also fires.
      window.dispatchEvent(new CustomEvent("ai-usage-refresh"));
      // Await status refresh so `status.hidden` matches disk before we clear
      // the optimistic override — otherwise a slow getStatus can re-tick.
      await onStatusChange?.();
      // Re-read truth from disk/registration — unregistered providers are
      // auto-hidden again by the backend, so never claim "shown" for those.
      const latest = await getStatus();
      const mine = latest.find((s) => s.id === meta.id);
      writeHiddenCacheAll(latest);

      if (next) {
        setStatus({ text: "Provider hidden from overlay.", kind: "ok" });
      } else if (!mine?.eligible) {
        setHiddenOverride(true);
        writeHiddenCache(meta.id, true);
        setStatus({
          text: "Provider has no valid usage details yet.",
          kind: "err",
        });
      } else if (mine.hidden) {
        setHiddenOverride(true);
        setStatus({ text: "Provider is still hidden.", kind: "err" });
      } else {
        setHiddenOverride(false);
        setStatus({ text: "Provider shown in overlay.", kind: "ok" });
      }
    } catch (e: unknown) {
      setHiddenOverride(!next);
      writeHiddenCache(meta.id, !next);
      setStatus({ text: String(e), kind: "err" });
    }
  }

  async function onRecheck() {
    setBusy(true);
    setStatus({ text: "Rechecking…", kind: "" });
    try {
      await refreshNow();
      await onStatusChange?.();
      const latest = await getStatus();
      const mine = latest.find((s) => s.id === meta.id);
      writeHiddenCacheAll(latest);
      window.dispatchEvent(new CustomEvent("ai-usage-refresh"));
      if (!mine?.eligible) {
        setHiddenOverride(true);
        writeHiddenCache(meta.id, true);
        setStatus({
          text: mine?.health_reason ?? "Still no valid usage details. Recheck again after configuring the provider.",
          kind: "err",
        });
      } else if (mine.hidden) {
        setStatus({
          text: "Detected. Enable “Overlay” to add it to the bar.",
          kind: "ok",
        });
      } else {
        setStatus({ text: "Detected and shown on the overlay.", kind: "ok" });
      }
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setBusy(false);
    }
  }

  async function finishOauthSuccess(message: string) {
    setOauth(null);
    setOauthCode("");
    setOauthBusy(false);
    try {
      await refreshNow();
      await onStatusChange?.();
      const latest = await getStatus();
      writeHiddenCacheAll(latest);
      window.dispatchEvent(new CustomEvent("ai-usage-refresh"));
      const mine = latest.find((s) => s.id === meta.id);
      // Claude may sign in successfully but still not register (free / no Pro/Max).
      if (meta.id === "claude" && mine && !mine.registered) {
        setStatus({
          text:
            "Signed in, but no Pro/Max plan was detected — Claude stays off the bar (expected).",
          kind: "ok",
        });
      } else {
        setStatus({ text: message, kind: "ok" });
      }
    } catch {
      setStatus({ text: message, kind: "ok" });
    }
  }

  async function onOauthSignIn() {
    // Invalidate any prior device-poll loop for this card (restart / remount).
    const watchGen = ++oauthWatchGen.current;
    setOauthBusy(true);
    setStatus({ text: "Starting sign-in…", kind: "" });
    try {
      if (oauth?.sessionId) {
        try {
          await cancelOauthLogin(oauth.sessionId);
        } catch {
          /* ignore */
        }
      }
      const start = await startOauthLogin(meta.id);
      if (watchGen !== oauthWatchGen.current) return;
      if (!start.sessionId) {
        throw new Error("Sign-in failed: no session id from backend");
      }
      setOauth(start);
      setOauthCode("");

      const deviceCode = (start.userCode ?? "").trim();
      // Keep the device code visible (sticky) so the 5s success auto-clear
      // does not hide it while the user is still approving in the browser.
      setStatus({
        text: deviceCode
          ? `Type this code in the browser: ${deviceCode}  (copied if allowed)`
          : start.message || "Waiting for browser approval…",
        kind: deviceCode ? "ok" : "",
        sticky: !!deviceCode,
      });

      if (deviceCode && navigator.clipboard?.writeText) {
        try {
          await navigator.clipboard.writeText(deviceCode);
        } catch {
          /* clipboard may be denied in webview — code still shown */
        }
      }

      // Release buttons immediately so Cancel / Sign out stay usable.
      // Device polling continues below without holding oauthBusy.
      setOauthBusy(false);

      // Device flows: backend polls in the background even if Settings closes.
      // This loop only updates the UI while the panel stays mounted and this
      // generation is still current (cancel / remount / restart bumps it).
      if (start.kind === "device") {
        const sessionId = start.sessionId;
        const deadline = Date.now() + (start.expiresIn ?? 900) * 1000;
        await new Promise((r) => setTimeout(r, 50));
        while (Date.now() < deadline) {
          if (watchGen !== oauthWatchGen.current) return;
          const poll = await pollOauthLogin(sessionId);
          if (watchGen !== oauthWatchGen.current) return;
          if (poll.status === "complete") {
            await finishOauthSuccess(poll.message ?? "Signed in.");
            return;
          }
          if (poll.status === "error") {
            setOauth(null);
            setOauthCode("");
            const msg = poll.message ?? "Sign-in failed.";
            if (/unknown or expired/i.test(msg)) {
              setStatus({ text: "Sign-in cancelled.", kind: "" });
            } else {
              setStatus({ text: msg, kind: "err" });
            }
            return;
          }
          setStatus({
            text: deviceCode
              ? `Type this code in the browser: ${deviceCode}  (waiting for approval…)`
              : poll.message || "Waiting for browser approval…",
            kind: "",
          });
          await new Promise((r) => setTimeout(r, 3000));
        }
        if (watchGen === oauthWatchGen.current) {
          setStatus({ text: "Sign-in timed out. Try again.", kind: "err" });
        }
      }
    } catch (e: unknown) {
      if (watchGen !== oauthWatchGen.current) return;
      setOauth(null);
      setOauthCode("");
      setStatus({ text: String(e), kind: "err" });
      setOauthBusy(false);
    }
  }

  async function copyDeviceCode() {
    const code = (oauth?.userCode ?? "").trim();
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code);
      setStatus({ text: `Copied ${code}`, kind: "ok" });
    } catch {
      setStatus({ text: `Code: ${code} (copy failed — type it manually)`, kind: "" });
    }
  }

  async function onOauthComplete() {
    if (!oauth?.sessionId) return;
    setOauthBusy(true);
    setStatus({ text: "Exchanging code…", kind: "" });
    try {
      const poll = await completeOauthLogin(oauth.sessionId, oauthCode);
      if (poll.status === "complete") {
        await finishOauthSuccess(poll.message ?? "Signed in.");
      } else {
        // Keep panel open so the user can paste again, but unlock buttons.
        setStatus({ text: poll.message ?? "Sign-in failed.", kind: "err" });
      }
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setOauthBusy(false);
    }
  }

  async function onOauthCancel() {
    oauthWatchGen.current += 1;
    if (oauth?.sessionId) {
      try {
        await cancelOauthLogin(oauth.sessionId);
      } catch {
        /* ignore */
      }
    }
    setOauth(null);
    setOauthCode("");
    setOauthBusy(false);
    setStatus({ text: "Sign-in dismissed.", kind: "" });
  }

  async function onOauthLogout() {
    oauthWatchGen.current += 1;
    setOauthBusy(true);
    try {
      // Also dismiss any in-progress sign-in UI.
      if (oauth?.sessionId) {
        try {
          await cancelOauthLogin(oauth.sessionId);
        } catch {
          /* ignore */
        }
      }
      setOauth(null);
      setOauthCode("");
      const msg = await oauthLogout(meta.id);
      setStatus({ text: msg, kind: "ok" });
      await onStatusChange?.();
      window.dispatchEvent(new CustomEvent("ai-usage-refresh"));
    } catch (e: unknown) {
      setStatus({ text: String(e), kind: "err" });
    } finally {
      setOauthBusy(false);
    }
  }

  const needsKey = meta.placeholder !== "";
  // Draft fields stay empty when a secret is already stored; typing a new
  // value (or changing region) marks the form dirty.
  const dirty =
    key.trim() !== "" ||
    (meta.hasRegion && region !== savedRegion) ||
    (meta.id === "openrouter" && managementKey.trim() !== "");
  const emptySaved =
    needsKey &&
    !hasSavedKey &&
    !(meta.id === "openrouter" && hasSavedManagementKey);

  const savedBadge = dirty
    ? { text: "Unsaved changes", kind: "warn" as const }
    : emptySaved
      ? { text: "Not saved", kind: "warn" as const }
      : meta.id === "claude"
        ? status?.registered
          ? { text: "Pro/Max", kind: "ok" as const }
          : { text: "No Pro/Max", kind: "warn" as const }
        : meta.oauth
          ? status?.configured
            ? { text: "Signed in", kind: "ok" as const }
            : { text: "Not signed in", kind: "err" as const }
          : { text: "Saved", kind: "ok" as const };

  const unavailable = status && !status.registered;
  const claudeHint =
    meta.id === "claude" && status && !status.registered
      ? "Not on the bar yet — needs an active Pro or Max plan. You can still sign in; tracking turns on after Pro/Max is detected (use Recheck)."
      : null;
  // OAuth providers (Grok / Codex): when the badge is red, surface the
  // backend's reason so the user can tell missing-vs-expired-vs-malformed.
  const oauthNotSignedInReason =
    meta.oauth && status && !status.configured
      ? status.unavailable_reason ?? "auth not found — use Sign in"
      : null;

  const showCheckbox = statusReady || cachedHidden !== null || hiddenOverride !== null;
  const oauthVerificationHref = safeHttpsHref(oauth?.verificationUriComplete);
  const oauthAuthorizeHref = safeHttpsHref(oauth?.authorizeUrl);

  const overlayVisibilityToggle = showCheckbox ? (
    <label
      class={`visibility ${forceHidden ? "visibility-disabled" : ""}`}
      htmlFor={`overlay-switch-${meta.id}`}
    >
      <span class="visibility-label" id={`overlay-label-${meta.id}`}>
        Overlay
      </span>
      <button
        type="button"
        id={`overlay-switch-${meta.id}`}
        role="switch"
        aria-checked={!hidden}
        aria-labelledby={`overlay-label-${meta.id}`}
        class="visibility-switch"
        disabled={checkboxDisabled({ eligible: !forceHidden }) || (!statusReady && hiddenOverride === null)}
        title={
          forceHidden
            ? "Not available until this provider is detected"
            : undefined
        }
        onClick={() => void onToggleHidden(!hidden)}
      >
        <span class="visibility-switch-track" aria-hidden="true">
          <span class="visibility-switch-thumb" aria-hidden="true" />
        </span>
      </button>
    </label>
  ) : (
    <span class="visibility visibility-loading">Loading…</span>
  );

  const savedBadgeEl = (
    <span class={`badge badge-inline ${savedBadge.kind}`}>{savedBadge.text}</span>
  );

  return (
    <div class={`provider-section ${dimSection ? "is-hidden" : ""}`}>
      <div class="provider-section-head">
        <div>
          <h2>{meta.label}</h2>
          <div class="hint">{meta.hint}</div>
        </div>
        <div class="provider-section-head-right">
          {overlayVisibilityToggle}
        </div>
      </div>

      {claudeHint && <div class="hint warn">{claudeHint}</div>}
      {unavailable && meta.id !== "claude" && status?.unavailable_reason && (
        <div class="hint warn">Not registered: {status.unavailable_reason}</div>
      )}
      {status && !status.eligible && status.health_reason && !oauthNotSignedInReason && (
        <div class="hint warn">Unavailable: {status.health_reason}</div>
      )}
      {oauthNotSignedInReason && meta.id !== "claude" && (
        <div class="hint err">{oauthNotSignedInReason}</div>
      )}

      {needsKey && (
        <>
          <div class="field-row">
            <label>API Key</label>
            <input
              type="password"
              value={key}
              placeholder={
                hasSavedKey
                  ? "Saved — type a new key to replace"
                  : meta.placeholder
              }
              onInput={(e) => setKey((e.target as HTMLInputElement).value)}
              onKeyDown={(e) => {
                if ((e.ctrlKey || e.metaKey) && e.key === "s") {
                  e.preventDefault();
                  void onSave();
                }
              }}
            />
          </div>
          {meta.id === "openrouter" && (
            <>
              <div class="field-row">
                <label>Management API Key</label>
                <input
                  type="password"
                  value={managementKey}
                  placeholder={
                    hasSavedManagementKey
                      ? "Saved — type a new key to replace"
                      : "Management key for account credits (optional)"
                  }
                  onInput={(e) => setManagementKey((e.target as HTMLInputElement).value)}
                />
              </div>
              <div class="action-row">
                <button class="action" onClick={() => void onSaveManagementKey()} disabled={busy || !managementKey.trim()}>
                  <SaveIcon />
                  Save Management key
                </button>
                <button
                  class="action secondary"
                  onClick={() => void onTestManagementKey()}
                  disabled={busy || (!managementKey.trim() && !hasSavedManagementKey)}
                >
                  <PulseIcon />
                  Test Management key
                </button>
                <button class="action secondary" onClick={() => void onClearManagementKey()} disabled={busy || !hasSavedManagementKey}>
                  <TrashIcon />
                  Clear Management key
                </button>
                <button
                  class="action secondary"
                  onClick={() => void onRebaseAccount()}
                  disabled={busy || !hasSavedManagementKey}
                  title="Use the current account balance as the new local tracking budget"
                >
                  <PulseIcon />
                  Rebase account balance
                </button>
              </div>
            </>
          )}
          {meta.hasRegion && (
            <div class="field-row">
              <label>Region</label>
              <select
                value={region}
                onChange={(e) => setRegion((e.target as HTMLSelectElement).value)}
              >
                <option value="overseas">Overseas (minimax.io)</option>
                <option value="china">China (minimaxi.com)</option>
              </select>
            </div>
          )}
          <div class="action-row">
            <button
              class="action"
              onClick={onSave}
              disabled={
                busy ||
                (!dirty && !emptySaved) ||
                (needsKey && !key.trim() && !hasSavedKey)
              }
            >
              <SaveIcon />
              Save
            </button>
            <button
              class="action secondary"
              onClick={onTest}
              disabled={busy || (!key.trim() && !hasSavedKey)}
            >
              <PulseIcon />
              Test
            </button>
            <button class="action secondary" onClick={onClear} disabled={busy || !hasSavedKey}>
              <TrashIcon />
              Clear
            </button>
            {savedBadgeEl}
          </div>
        </>
      )}
      {!needsKey && (
        <>
          {meta.oauth && (
            <div class="oauth-block">
              <div class="action-row action-row-plain">
                <button
                  class="action"
                  onClick={() => void onOauthSignIn()}
                  disabled={busy || oauthBusy || (!oauth && !!status?.configured)}
                  title={
                    !oauth && status?.configured
                      ? "Already signed in — use Sign out first to switch accounts"
                      : "Open browser sign-in for this provider"
                  }
                >
                  {oauth ? "Restart sign-in" : "Sign in"}
                </button>
                <button
                  class="action secondary"
                  onClick={() => void onOauthLogout()}
                  disabled={busy || oauthBusy || !status?.configured}
                  title={
                    !status?.configured
                      ? "Not signed in"
                      : "Clear local OAuth tokens for this provider"
                  }
                >
                  Sign out
                </button>
                <button
                  class="action secondary"
                  onClick={() => void onRecheck()}
                  disabled={busy || oauthBusy || !status?.configured}
                  title={
                    !status?.configured
                      ? "Sign in first to re-detect local data / subscription"
                      : "Re-detect local data / subscription"
                  }
                >
                  <PulseIcon />
                  Recheck
                </button>
                {savedBadgeEl}
              </div>
              {oauth && (
                <div class="oauth-active">
                  {oauth.userCode && (
                    <div class="oauth-device">
                      <div class="oauth-device-label">
                        Enter this code in the browser
                      </div>
                      <div class="oauth-device-code" title="Device code">
                        {oauth.userCode}
                      </div>
                      <div class="oauth-device-actions">
                        <button
                          type="button"
                          class="action secondary"
                          onClick={() => void copyDeviceCode()}
                        >
                          Copy code
                        </button>
                        {oauthVerificationHref && (
                          <a
                            class="oauth-link"
                            href={oauthVerificationHref}
                            target="_blank"
                            rel="noreferrer"
                          >
                            Open login page
                          </a>
                        )}
                      </div>
                    </div>
                  )}
                  {oauth.kind === "manual_code" && (
                    <div class="oauth-manual">
                      {oauthAuthorizeHref && (
                        <div class="oauth-device-actions">
                          <a
                            class="oauth-link"
                            href={oauthAuthorizeHref}
                            target="_blank"
                            rel="noreferrer"
                          >
                            Open authorize page
                          </a>
                        </div>
                      )}
                      <div class="field-row oauth-paste">
                        <label>Paste CODE#STATE</label>
                        <input
                          type="text"
                          value={oauthCode}
                          placeholder="abc123#state…"
                          onInput={(e) => setOauthCode((e.target as HTMLInputElement).value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              e.preventDefault();
                              void onOauthComplete();
                            }
                          }}
                        />
                        <button
                          class="action"
                          onClick={() => void onOauthComplete()}
                          disabled={oauthBusy || !oauthCode.trim()}
                        >
                          Complete
                        </button>
                      </div>
                    </div>
                  )}
                  <button
                    class="action secondary"
                    onClick={() => void onOauthCancel()}
                    disabled={oauthBusy}
                    title="Close this panel without finishing sign-in"
                  >
                    Dismiss
                  </button>
                </div>
              )}
            </div>
          )}
          {!meta.oauth && (
            <div class="action-row action-row-plain">
              <button
                class="action secondary"
                onClick={() => void onRecheck()}
                disabled={busy || oauthBusy || !status?.eligible}
                title={
                  !status?.eligible
                    ? "Sign in first to re-detect local data / subscription"
                    : "Re-detect local data / subscription"
                }
              >
                <PulseIcon />
                Recheck
              </button>
              {savedBadgeEl}
            </div>
          )}
        </>
      )}
      <div class={`status-line ${status_.kind}`}>{status_.text}</div>
    </div>
  );
}

/** Hide / Quit controls for the Settings header (top-right). */
export function SettingsHeaderActions() {
  return (
    <div class="settings-header-actions">
      <button
        type="button"
        class="settings-header-btn"
        title="Hide the bar; keep running in the tray"
        onClick={() => {
          void hideToTray().catch((e) => console.error(e));
        }}
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" aria-hidden="true">
          <path d="M5 17h14" />
        </svg>
        Hide to tray
      </button>
      <button
        type="button"
        class="settings-header-btn settings-header-btn-danger"
        title="Fully exit AI Usage Tracker"
        onClick={() => {
          void quitApp().catch((e) => console.error(e));
        }}
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M18 6L6 18M6 6l12 12" />
        </svg>
        Quit
      </button>
    </div>
  );
}

/**
 * The Settings form, decoupled from any page wrapper. Used by the
 * standalone Settings window (settings.tsx) and by the overlay's
 * embedded settings popup.
 */
export function SettingsPanel() {
  const [statuses, setStatuses] = useState<ProviderStatus[]>([]);
  const [statusReady, setStatusReady] = useState(false);
  const [autostart, setAutostart] = useState<boolean>(false);
  const [autostartBusy, setAutostartBusy] = useState(false);
  const [notificationsEnabled, setNotificationsEnabledState] = useState<boolean>(false);
  const [notificationsBusy, setNotificationsBusy] = useState(false);
  const [refreshInterval, setRefreshIntervalState] = useState<number>(60);
  const [refreshIntervalBusy, setRefreshIntervalBusy] = useState(false);

  async function onLoad() {
    try {
      const s = await getStatus();
      setStatuses(s);
      writeHiddenCacheAll(s);
      setStatusReady(true);
    } catch (e) {
      console.error(e);
      // Still unlock the UI — checkboxes fall back to cache / defaults.
      setStatusReady(true);
    }
    try {
      const a = await getAutostartEnabled();
      setAutostart(a);
    } catch (e) {
      console.error(e);
    }
    try {
      const n = await getNotificationsEnabled();
      setNotificationsEnabledState(n);
    } catch (e) {
      console.error(e);
    }
    try {
      const r = await getRefreshInterval();
      setRefreshIntervalState(r);
    } catch (e) {
      console.error(e);
    }
  }

  useEffect(() => {
    void onLoad();
  }, []);

  // Re-fetch statuses after a hide/show toggle so ProviderSection's
  // `status.hidden` matches what was just saved to disk.
  async function refreshStatuses() {
    try {
      const s = await getStatus();
      setStatuses(s);
      writeHiddenCacheAll(s);
      setStatusReady(true);
    } catch (e) {
      console.error(e);
    }
  }

  async function onToggleAutostart(next: boolean) {
    setAutostartBusy(true);
    setAutostart(next);
    try {
      await setAutostartEnabled(next);
    } catch (e) {
      console.error(e);
      setAutostart(!next);
    } finally {
      setAutostartBusy(false);
    }
  }

  async function onToggleNotifications(next: boolean) {
    setNotificationsBusy(true);
    setNotificationsEnabledState(next);
    try {
      await setNotificationsEnabled(next);
    } catch (e) {
      console.error(e);
      setNotificationsEnabledState(!next);
    } finally {
      setNotificationsBusy(false);
    }
  }

  async function onChangeRefreshInterval(next: number) {
    const prev = refreshInterval;
    setRefreshIntervalBusy(true);
    setRefreshIntervalState(next);
    try {
      await setRefreshInterval(next);
      // Notify the overlay in the same webview immediately (in addition to the
      // Tauri `refresh-interval-changed` event for other windows).
      window.dispatchEvent(
        new CustomEvent("ai-usage-refresh-interval", { detail: next }),
      );
    } catch (e) {
      console.error(e);
      setRefreshIntervalState(prev);
    } finally {
      setRefreshIntervalBusy(false);
    }
  }

  const byId = useMemo(() => {
    const m = new Map<ProviderId, ProviderStatus>();
    for (const s of statuses) m.set(s.id, s);
    return m;
  }, [statuses]);

  return (
    <>
      <div class="provider-section updates-card">
        <UpdatesSection />
      </div>

      <div class="provider-section">
        <div class="provider-section-head">
          <div class="section-title-row">
            <SettingsIcon />
            <h2>General</h2>
          </div>
        </div>
        <label class="visibility">
          <input
            type="checkbox"
            checked={autostart}
            disabled={autostartBusy}
            onChange={(e) => void onToggleAutostart((e.target as HTMLInputElement).checked)}
          />
          Start AI Usage Tracker when I sign in to Windows
        </label>
        <div class="hint" style="margin-top:4px">
          Starts with Windows and stays in the tray.
        </div>
        <div style="margin-top:6px;display:flex;align-items:center;gap:8px">
          <span style="font-size:11px;color:var(--fg)">Refresh interval</span>
          <select
            value={refreshInterval}
            disabled={refreshIntervalBusy}
            onChange={(e) =>
              void onChangeRefreshInterval(
                Number((e.target as HTMLSelectElement).value),
              )
            }
            style="background:var(--bg-base);border:1px solid var(--border-strong);border-radius:5px;padding:3px 8px;color:var(--fg);font-size:11px;height:24px;cursor:pointer"
          >
            <option value={30}>30s</option>
            <option value={45}>45s</option>
            <option value={60}>60s</option>
            <option value={90}>90s</option>
            <option value={120}>2m</option>
            <option value={180}>3m</option>
            <option value={300}>5m</option>
          </select>
        </div>
        <div class="hint" style="margin-top:4px">
          How often provider usage reloads. Default 60s.
        </div>
        <label class="visibility" style="margin-top:6px">
          <input
            type="checkbox"
            checked={notificationsEnabled}
            disabled={notificationsBusy}
            onChange={(e) => void onToggleNotifications((e.target as HTMLInputElement).checked)}
          />
          Notify when a quota crosses its red line or runs out
        </label>
        <div class="hint" style="margin-top:4px">
          Windows notification when a usage window drops below its red pace line or hits 0% left.
        </div>
      </div>

      {PROVIDERS.map((p) => (
        <ProviderSection
          key={p.id}
          meta={p}
          status={byId.get(p.id)}
          onStatusChange={refreshStatuses}
          statusReady={statusReady}
        />
      ))}
    </>
  );
}

function formatUpdateDate(value: string | null | undefined): string {
  if (!value) return "Never";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function ReleaseNotes({ notes }: { notes: string }) {
  return (
    <div class="update-notes" aria-label="Release notes">
      {notes.split(/\r?\n/).map((line, index) => {
        const heading = line.match(/^#{1,6}\s+(.+)$/);
        if (heading) return <h4 key={index}>{heading[1]}</h4>;
        const bullet = line.match(/^\s*[-*]\s+(.+)$/);
        if (bullet) return <div key={index} class="update-note-bullet">• {bullet[1]}</div>;
        return line.trim() ? <p key={index}>{line}</p> : null;
      })}
    </div>
  );
}

function UpdatesSection() {
  const state = useUpdateState();
  const [actionError, setActionError] = useState<string | null>(null);
  const [channelBusy, setChannelBusy] = useState(false);
  const phase = state?.phase ?? "idle";
  // Keep all backend phases explicit so additions cannot silently inherit an
  // unsafe control state.
  const busy = phase === "checking" || phase === "downloading" || phase === "installing";
  const channel = state?.channel ?? "stable";
  const statusText: Record<typeof phase, string> = {
    "idle": "Updates are checked automatically once a day.",
    "checking": "Checking for updates…",
    "up_to_date": "You’re up to date.",
    "available": "An update is ready when you choose to install it.",
    "downloading": "Downloading update…",
    "installing": "Installing update and restarting…",
    "error": "The update action did not complete.",
  };

  async function onCheck() {
    setActionError(null);
    try { await checkForUpdate(true); }
    catch (error) {
      console.error("Manual update check failed", error);
      setActionError("Could not check for updates. Please try again.");
    }
  }

  async function onInstall() {
    setActionError(null);
    try { await installUpdate(); }
    catch (error) {
      console.error("Update installation failed", error);
      setActionError("Could not install the update. Please try again.");
    }
  }

  async function onChannelChange(next: UpdateChannel) {
    if (next === channel) return;
    setActionError(null);
    setChannelBusy(true);
    try {
      await setUpdateChannel(next);
    } catch (error) {
      console.error("Update channel change failed", error);
      setActionError("Could not change update channel. Please try again.");
    } finally {
      setChannelBusy(false);
    }
  }

  const shownError = actionError ?? state?.error;
  const progressPercent = clampUpdateProgressPercent(
    state?.downloaded_bytes ?? 0,
    state?.total_bytes ?? 0,
  );
  const progressValue = state?.total_bytes && state.total_bytes > 0
    ? progressPercent
    : undefined;

  return (
    <section class="updates-section" aria-labelledby="updates-heading">
      <h3 id="updates-heading">Updates</h3>
      <dl class="update-facts">
        <div><dt>Installed version</dt><dd>{state?.current_version ?? __APP_VERSION__}</dd></div>
        <div><dt>Last successful check</dt><dd>{formatUpdateDate(state?.last_checked_at)}</dd></div>
        {state?.available_version && <div><dt>Available version</dt><dd>{state.available_version}</dd></div>}
      </dl>
      <div class="update-channel-row">
        <label class="update-channel-label" for="update-channel">Update channel</label>
        <select
          id="update-channel"
          class="update-channel-select"
          aria-label="Update channel"
          value={channel}
          disabled={busy || channelBusy}
          onChange={(e) =>
            void onChannelChange((e.target as HTMLSelectElement).value as UpdateChannel)
          }
        >
          <option value="stable">Stable releases</option>
          <option value="prerelease">Prerelease builds</option>
        </select>
      </div>
      <div class="hint update-channel-hint">
        Stable releases: main releases only. Prerelease builds: prerelease builds only.
      </div>
      <div class="hint" role="status" aria-live="polite">{statusText[phase]}</div>
      {(phase === "downloading" || phase === "installing") && (
        <div class="update-progress">
          <progress max={100} value={progressValue} />
          {state?.total_bytes ? `${Math.round(progressPercent)}%` : "Working…"}
        </div>
      )}
      {state?.notes && phase === "available" && <ReleaseNotes notes={state.notes} />}
      {shownError && <div class="update-error" role="alert">{shownError}</div>}
      <div class="action-row update-actions">
        <button class="action secondary" disabled={busy || channelBusy} onClick={() => void onCheck()}>
          {phase === "error" ? "Retry" : "Check for updates"}
        </button>
        {phase === "available" && (
          <button class="action" disabled={busy || channelBusy} onClick={() => void onInstall()}>
            Install update and restart
          </button>
        )}
        <a
          class="update-gh-link"
          href="https://github.com/GimpMan/AI-Usage-Tracker"
          target="_blank"
          rel="noreferrer"
          aria-label="GitHub repository"
          title="GitHub repository"
          onClick={(e) => {
            e.preventDefault();
            void openExternal("https://github.com/GimpMan/AI-Usage-Tracker").catch((err) =>
              console.error("open_external failed", err),
            );
          }}
        >
          GitHub
        </a>
      </div>
    </section>
  );
}

function SettingsIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <circle cx="12" cy="12" r="3" fill="#3b82f6" />
      <path
        d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z"
        stroke="#3b82f6"
        stroke-width="1.6"
        stroke-linejoin="round"
      />
    </svg>
  );
}

function SaveIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M5 3h11l3 3v15a0 0 0 0 1 0 0H5a0 0 0 0 1 0 0V3Z"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linejoin="round"
      />
      <path d="M7 3v6h8V3" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
      <path d="M7 14h10v7H7z" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    </svg>
  );
}

function PulseIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M3 12h4l2-6 4 12 2-6h6"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}

function TrashIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M4 7h16M9 7V4h6v3M6 7l1 13a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-13"
        stroke="currentColor"
        stroke-width="1.8"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}
