import { render } from "preact";
import type { Ref, RefObject } from "preact";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import {
  currentMonitor,
  getCurrentWindow,
  monitorFromPoint,
} from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import type { BurnBucket, ProviderBurnHistory, UsageSnapshot } from "./types";
import { listen } from "@tauri-apps/api/event";
import {
  getBurnHistory,
  getRefreshInterval,
  getUsage,
  refreshProvider,
  saveOverlayPosition,
} from "./api";
import type { ProviderId } from "./types";
import { collapsedBarRemaining, collapsedBarColorPercent, isWeeklyUnderRedLine, isWeeklyWindowUnderRedLine, isFiveHourWindowUnderRedLine, isFiveHourUnderRedLine } from "./bar-summary";
import { burnBarHeights, burnBarTitle, bucketSecsForLabel, burnedTodayPercent, hasBurnHistory } from "./burn-bars.ts";
import { formatDollarWindow, hasDisplayableWindows } from "./provider-visibility";
import { normalizeProviderOrder, reorderProviderOrder } from "./provider-order";
import {
  normalizeRefreshIntervalSecs,
  refreshCountdownLabel,
  refreshRemainingFraction,
  refreshRingDash,
} from "./refresh-countdown";
import {
  DEFAULT_REFRESH_INTERVAL_SECS,
  isStaleSnapshot,
  staleThresholdMs,
} from "./stale-snapshot";
import {
  reduceSettingsClose,
  type SettingsClosePhase,
} from "./settings-close";
import { SettingsHeaderActions, SettingsPanel } from "./settings-panel";
import { UpdateStateProvider, useUpdateState } from "./update-state";
import { formatDurationApprox, isFiveHourWindow, isWeeklyWindow, resolveEvenPace, recentProjectionRate, dollarMonthlyProjectionNote } from "./weekly-pace";
import {
  EXPANDED_PANEL_MIN_WIDTH,
  overlayWindowWidth,
} from "./overlay-width";
import {
  SETTINGS_PANEL_FALLBACK_HEIGHT,
  settingsPanelMaxHeight,
} from "./overlay-height";
import glmLogo from "./assets/glm-logo.svg";
import minimaxLogo from "./assets/minimax-logo.png";
import openaiLogo from "./assets/openai-logo.svg";
import grokLogo from "./assets/grok-logo.png";
import openrouterLogo from "./assets/openrouter-logo.png";
import kimiLogo from "./assets/kimi-logo.png";
import brandLogo from "./assets/brand-logo.png";

const currentWindow = getCurrentWindow();

/**
 * Reasons the bar treats as "soft empty" (no data on disk yet, not an error).
 * These render with neutral styling instead of the loud red error segment.
 * The popup still surfaces the exact reason in its details panel.
 *
 * F7: backend produces these exact strings (see `REASON_*` consts in
 * `providers/codex.rs`). Soft-empty reasons may carry a suffix like
 * `"stale local log — <detail>"`; `isSoftEmptyReason` matches by prefix so
 * the muted styling still kicks in.
 */
const SOFT_EMPTY_REASONS = new Set<string>([
  "no local claude data",
  "no recent usage",
  "no rate-limit data yet",
  "codex logs not found",
  "no Codex auth found",
  "stale local log",
  "no Grok auth found",
  "session expired — run grok login",
  "no usage data yet",
  "no auth found — sign in with Kimi",
]);

/**
 * F7: match a reason exactly OR by prefix delimiter so suffixes like
 * `"stale local log — <detail>"` still count as soft-empty.
 */
function isSoftEmptyReason(reason: string): boolean {
  if (SOFT_EMPTY_REASONS.has(reason)) return true;
  for (const base of SOFT_EMPTY_REASONS) {
    if (
      reason === base ||
      reason.startsWith(base + " ") ||
      reason.startsWith(base + " —") ||
      reason.startsWith(base + " (")
    ) {
      return true;
    }
  }
  return false;
}

// ============================================================
// Helpers
// ============================================================

/** Bar height in logical px. Matches `.bar { height }` in styles.css.
 *  The overlay measures the live DOM, so this is only the fallback used
 *  before the first measurement. */
const COMPACT_BAR_H_LOGICAL = 24;

/** Gap between popup bottom edge and bar top. MUST match `.popup` /
 *  `.settings-popup { margin-bottom }` and POPUP_GAP_LOGICAL in commands.rs. */
const POPUP_GAP_LOGICAL = 6;

/** Floor for the embedded Settings panel height (logical px). Provider popups
 *  are ~150–280px; Settings is much taller. The panel measures about 878px
 *  with all provider rows, so preallocate above that while the HWND is hidden;
 *  a visible SetWindowPos during the first open paints a white frame. */
const SETTINGS_MIN_POPUP_H = 900;

/** Slide/slide-out animation duration for the embedded Settings popup. MUST
 *  stay in sync with `animation: settings-popup-out <N>ms` in src/styles.css;
 *  the close timer uses this number to schedule the unmount exactly when the
 *  CSS keyframes finish. */
const SETTINGS_CLOSE_MS = 180;

/** Duration of the smooth bar-growth animation (native window width
 *  interpolation via rAF). Tuned to match the segment entrance animation
 *  so the bar background and the segment reveals feel synchronized. */
const WIDTH_ANIM_MS = 280;

// The transparent HWND is created at this height before it is first shown.
// Keeping a small amount of headroom above the Settings floor also covers the
// normal bar height and popup gap without a visible SetWindowPos on first open.
const OVERLAY_PREALLOCATED_H_LOGICAL =
  SETTINGS_MIN_POPUP_H + COMPACT_BAR_H_LOGICAL + POPUP_GAP_LOGICAL;

// Initial overlay width (logical px). Wide enough for a full multi-provider
// minibar so the first paint cannot clip trackers before JS measures content.
// Keep in sync with `src-tauri/src/overlay.rs` open_overlay `(w, h)`.
const OVERLAY_PREALLOCATED_W_LOGICAL = 800;

/** Map a provider display label to a stable icon id. */
function iconIdFor(label: string): string {
  const l = label.toLowerCase();
  // Match plan names and legacy model labels so icons stay correct after renames.
  if (l.includes("glm") || l.includes("z.ai") || l.includes("zai")) return "glm";
  if (l.includes("minimax")) return "minimax";
  if (l.includes("codex") || l.includes("openai")) return "codex";
  if (l.includes("claude") || l.includes("anthropic")) return "claude";
  if (l.includes("grok") || l.includes("xai") || l.includes("x.ai")) return "grok";
  if (l.includes("kimi") || l.includes("moonshot")) return "kimi";
  if (l.includes("openrouter")) return "openrouter";
  return "default";
}

const PROVIDER_IDS: readonly ProviderId[] = [
  "glm",
  "minimax",
  "codex",
  "claude",
  "grok",
  "kimi",
  "openrouter",
];

/** Map a snapshot provider label to the backend provider id for refresh. */
function providerIdFor(label: string): ProviderId | null {
  const id = iconIdFor(label);
  return (PROVIDER_IDS as readonly string[]).includes(id)
    ? (id as ProviderId)
    : null;
}

function RefreshIcon() {
  // Paths centered in a 24×24 box so the glyph aligns with the countdown ring.
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path
        d="M4.5 12a7.5 7.5 0 0 1 12.4-5.7"
        stroke="currentColor"
        stroke-width="1.9"
        stroke-linecap="round"
      />
      <path
        d="M19.5 12a7.5 7.5 0 0 1-12.4 5.7"
        stroke="currentColor"
        stroke-width="1.9"
        stroke-linecap="round"
      />
      <path
        d="M16.2 3.8v4.2h4.2"
        stroke="currentColor"
        stroke-width="1.9"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
      <path
        d="M7.8 20.2v-4.2H3.6"
        stroke="currentColor"
        stroke-width="1.9"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  );
}

/** Circular countdown around the popup refresh button icon (1 = just refreshed).
 *  Uses pathLength=1 so progress is a simple 0–1 dash, driven by the live
 *  Settings refresh interval. */
function RefreshCountdownRing({
  remaining,
  title,
}: {
  remaining: number;
  title?: string;
}) {
  const size = 28;
  const stroke = 2.25;
  const r = (size - stroke) / 2;
  const dash = refreshRingDash(remaining);
  return (
    <svg
      class="popup-refresh-ring"
      // No width/height attrs: WebView2 lets them win over CSS, which prevented
      // the gear button from shrinking the ring to 17×17. The viewBox stays
      // 0..size so internal coordinates are unchanged; CSS owns rendered size
      // (.popup-refresh-ring = 28px for the popup, 17px on .bar-btn-settings).
      viewBox={`0 0 ${size} ${size}`}
      preserveAspectRatio="xMidYMid meet"
      aria-hidden="true"
    >
      {title && <title>{title}</title>}
      <circle
        class="popup-refresh-ring-track"
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        stroke-width={stroke}
        pathLength={1}
      />
      <circle
        class="popup-refresh-ring-prog"
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        stroke-width={stroke}
        pathLength={1}
        // Unit circle: visible arc length = remaining (1=full, 0=empty).
        stroke-dasharray={`${dash} 1`}
        stroke-linecap="round"
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
      />
    </svg>
  );
}

/** Collapsed-by-default "More details" dropdown for a window's extra detail.
 *  The collapsed row is just a "More details" toggle; expanding reveals the
 *  pace-gap line, pace notes, projection, absolute used/limit numbers (when
 *  the provider reports them, e.g. Kimi), and (for weekly windows with burn
 *  history) the "used today" summary. Every row is optional — the dropdown
 *  renders whenever at least one row has content. */
function PaceNotesDropdown({
  note,
  gapNote,
  timeNote,
  projectionNote,
  absoluteNote,
  extraNote,
  usedTodayNote,
}: {
  note: string | null;
  gapNote: string | null;
  timeNote: string | null;
  projectionNote: string | null;
  absoluteNote: string | null;
  extraNote: string | null;
  usedTodayNote: string | null;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div class={`pace-notes-dropdown${open ? " is-open" : ""}`}>
      <button
        type="button"
        class="pace-notes-toggle"
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        <span class="pace-notes-toggle-text">More details</span>
        <span class="pace-notes-toggle-caret" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div class="pace-notes-body">
          {gapNote && (
            <div class="weekly-pace-note" aria-live="polite">
              {gapNote}
            </div>
          )}
          {note && (
            <div class="weekly-pace-note" aria-live="polite">
              {note}
            </div>
          )}
          {timeNote && (
            <div class="weekly-pace-time" aria-live="polite">
              {timeNote}
            </div>
          )}
          {projectionNote && (
            <div class="weekly-pace-note" aria-live="polite">
              {projectionNote}
            </div>
          )}
          {absoluteNote && (
            <div class="weekly-pace-note" aria-live="polite">
              {absoluteNote}
            </div>
          )}
          {extraNote && (
            <div class="weekly-pace-note" aria-live="polite">
              {extraNote}
            </div>
          )}
          {usedTodayNote && (
            <div class="weekly-pace-note" aria-live="polite">
              {usedTodayNote}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** One bar per bucket; height = burn relative to this window's max bucket.
 *  Heavy buckets trend red via fillColor; resets render as a green marker. */
function BurnBars({ buckets }: { buckets: BurnBucket[] }) {
  const heights = burnBarHeights(buckets);
  return (
    <div class="burn-bars">
      {buckets.map((b, i) =>
        b.reset ? (
          <div key={i} class="burn-bar-reset" title={burnBarTitle(b)} />
        ) : (
          <div
            key={i}
            class="burn-bar"
            style={`height:${heights[i]}%;background-color:${fillColor(100 - heights[i])}`}
            title={burnBarTitle(b)}
          />
        ),
      )}
    </div>
  );
}

/** Short label for inline bar display: "weekly" -> "wk", "5h" -> "5h". */
function shortLabel(label: string): string {
  const l = label.toLowerCase();
  if (l === "weekly" || l === "wk") return "wk";
  if (l === "daily") return "day";
  if (l === "monthly") return "mo";
  if (l === "session") return "5h";
  return l;
}

/** Pretty heading for popup: "5h" -> "5-Hour Window", "weekly" -> "Weekly".
 *  F3: handles both `"weekly"` (legacy) and `"wk"` (current backend shorthand).
 *  "monthly" is provider-aware: Z.ai's monthly window is a tool-use quota,
 *  while Grok/OpenRouter monthly windows are plain usage pools. */
function prettyLabel(label: string, provider?: string): string {
  const l = label.toLowerCase();
  if (l.startsWith("balance ")) return "Account Balance";
  if (l.startsWith("total ")) return "Total Credit Limit";
  if (l.startsWith("today ")) return "Today";
  if (l.startsWith("this week ")) return "This Week";
  if (l.startsWith("this month ")) return "This Month";
  if (l === "weekly" || l === "wk") return "Weekly";
  if (l === "daily") return "Daily";
  if (l === "monthly" || l === "mo")
    return provider === "Z.ai Coding Plan" ? "Monthly Tool Use" : "Monthly";
  if (l === "5h") return "5-Hour Window";
  if (l === "3h") return "3-Hour Window";
  return label;
}

// RGB stops for the continuous fill gradient (extra-bright neon tones).
const FILL_GREEN: readonly [number, number, number] = [0, 255, 100]; // #00ff64
const FILL_AMBER: readonly [number, number, number] = [255, 200, 0]; // #ffc800
const FILL_RED: readonly [number, number, number] = [255, 70, 70]; // #ff4646
const FILL_PURPLE: readonly [number, number, number] = [200, 30, 255]; // #c81eff

/** Smooth color interpolation: bright green (at or above pace) → bright
 *  amber (~30%) → bright red (0%). When usage is ahead of the blue pace
 *  line, the gradient shifts from bright green into bright purple as the
 *  remaining % approaches 100% (colorPercent > 100). Values 100–200 come
 *  from `paceGradientPercent` when a window is ahead of its pace target.
 *  A CSS gloss overlay on the fill element adds depth so the bar is not
 *  flat. */
function fillColor(colorPercent: number): string {
  // Ahead of pace: green → bright purple. A square-root curve so the
  // purple kicks in fast (small leads read clearly as purple) and the G
  // channel drops below B quickly, avoiding a teal intermediate.
  if (colorPercent > 100) {
    const t = Math.sqrt(Math.min(1, (colorPercent - 100) / 100));
    const r = Math.round(FILL_GREEN[0] + (FILL_PURPLE[0] - FILL_GREEN[0]) * t);
    const g = Math.round(FILL_GREEN[1] + (FILL_PURPLE[1] - FILL_GREEN[1]) * t);
    const b = Math.round(FILL_GREEN[2] + (FILL_PURPLE[2] - FILL_GREEN[2]) * t);
    return `rgb(${r},${g},${b})`;
  }

  const pct = Math.max(0, Math.min(100, colorPercent));
  let r: number, g: number, b: number;
  if (pct >= 30) {
    // Green (100%) → Amber (30%)
    const t = (100 - pct) / 70;
    r = Math.round(FILL_GREEN[0] + (FILL_AMBER[0] - FILL_GREEN[0]) * t);
    g = Math.round(FILL_GREEN[1] + (FILL_AMBER[1] - FILL_GREEN[1]) * t);
    b = Math.round(FILL_GREEN[2] + (FILL_AMBER[2] - FILL_GREEN[2]) * t);
  } else {
    // Amber (30%) → Red (0%)
    const t = (30 - pct) / 30;
    r = Math.round(FILL_AMBER[0] + (FILL_RED[0] - FILL_AMBER[0]) * t);
    g = Math.round(FILL_AMBER[1] + (FILL_RED[1] - FILL_AMBER[1]) * t);
    b = Math.round(FILL_AMBER[2] + (FILL_RED[2] - FILL_AMBER[2]) * t);
  }
  return `rgb(${r},${g},${b})`;
}

/** "99% 5h · 1% wk" style summary for the collapsed bar. Only windows with
 *  `bar_visible` are included (e.g. GLM's monthly tool quota is popup-only). */
function summary(snap: UsageSnapshot): {
  text: string;
  worstRemaining: number;
  colorPercent: number;
} {
  const visible = snap.windows.filter((w) => w.bar_visible);
  if (visible.length === 0) {
    return { text: "—", worstRemaining: 100, colorPercent: 100 };
  }
  const parts: string[] = [];
  const worst = collapsedBarRemaining(visible);
  for (const w of visible) {
    const remaining = Math.max(0, 100 - w.used_percent);
    parts.push(
      formatDollarWindow(w) ??
        `${w.is_unlimited ? "∞" : `${Math.round(remaining)}%`} ${shortLabel(w.label)}`,
    );
  }
  const colorPercent = collapsedBarColorPercent(snap.windows, snap.provider);
  return { text: parts.join(" · "), worstRemaining: worst, colorPercent };
}

/** Natural content width of the status bar (logical px) — every non-spacer
 *  child, plus flex gaps, margins, and padding. Used to size the OS window
 *  so it always fits the current card set (add/remove providers resizes it).
 *
 *  Important: do **not** use `barEl.scrollWidth` when the bar already fills a
 *  wide window — that equals the window width and locks in empty space on the
 *  right. Only trust scrollWidth when it exceeds clientWidth (true overflow). */
function naturalBarWidth(barEl: HTMLElement): number {
  const cs = getComputedStyle(barEl);
  const gap = parseFloat(cs.columnGap || cs.gap || "0") || 0;
  let w = 0;
  let n = 0;
  for (const child of Array.from(barEl.children) as HTMLElement[]) {
    if (child.classList.contains("bar-spacer")) continue;
    const childCs = getComputedStyle(child);
    // Prefer offsetWidth (laid-out size). Use scrollWidth only when a child
    // is actively truncating its own content (text overflow).
    let childW = child.offsetWidth;
    if (child.scrollWidth > child.clientWidth + 1) {
      childW = Math.max(childW, child.scrollWidth);
    }
    w += childW;
    const intrinsicMarginLeft = child.classList.contains("bar-btn-settings")
      ? 0
      : parseFloat(childCs.marginLeft || "0") || 0;
    w += intrinsicMarginLeft + (parseFloat(childCs.marginRight || "0") || 0);
    n += 1;
  }
  if (n > 1) w += gap * (n - 1);
  w += parseFloat(cs.paddingLeft || "0") + parseFloat(cs.paddingRight || "0");
  // Border + tiny pad so the gear isn't flush against the edge.
  w += 6;
  // True overflow only: content is wider than the current bar box.
  if (barEl.scrollWidth > barEl.clientWidth + 1) {
    w = Math.max(w, barEl.scrollWidth + 6);
  }
  return Math.ceil(w);
}

/** Measure compact provider content independently from the live flexed bar.
 * The off-screen clone uses fixed 36px tracks, so provider additions made
 * while a popup is open cannot inherit a stale one-provider baseline. */
function compactBarWidth(barEl: HTMLElement): number {
  const clone = barEl.cloneNode(true) as HTMLElement;
  clone.classList.add("measuring-compact");
  clone.style.position = "fixed";
  clone.style.left = "-10000px";
  clone.style.top = "0";
  clone.style.width = "max-content";
  clone.style.visibility = "hidden";
  clone.style.pointerEvents = "none";
  document.body.appendChild(clone);
  const width = naturalBarWidth(clone);
  clone.remove();
  return width;
}

/** Persisted provider order (user-rearranged bar segments). Stored by
 *  provider label so it survives restarts. */
const ORDER_KEY = "ai-usage-tracker:order";
function loadOrder(): string[] {
  try {
    const raw = localStorage.getItem(ORDER_KEY);
    if (raw) {
      const arr = JSON.parse(raw);
      if (Array.isArray(arr)) return arr.filter((x) => typeof x === "string");
    }
  } catch {
    /* ignore corrupt storage */
  }
  return [];
}
function saveOrder(order: string[]): void {
  try {
    localStorage.setItem(ORDER_KEY, JSON.stringify(order));
  } catch {
    /* storage unavailable */
  }
}

function formatReset(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  const diffMs = d.getTime() - Date.now();
  if (diffMs <= 0) return "resetting";
  const mins = Math.round(diffMs / 60000);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  const remMin = mins % 60;
  if (hrs < 48) return remMin ? `${hrs}h ${remMin}m` : `${hrs}h`;
  const days = Math.floor(hrs / 24);
  return `${days}d ${hrs % 24}h`;
}

// ============================================================
// Provider icons (inline SVG)
// ============================================================
function ProviderIcon({ id, size = 14 }: { id: string; size?: number }) {
  const common = { width: size, height: size, viewBox: "0 0 24 24", class: "icon-svg" };
  switch (id) {
    case "glm":
      // Z.ai official logo (bundled locally to avoid CDN hotlink/cold-start 404s).
      return (
        <img
          src={glmLogo}
          class="icon-svg"
          style="object-fit:contain"
          draggable={false}
          alt="GLM"
        />
      );
    case "codex":
      // OpenAI logo (Codex runs on OpenAI).
      return (
        <img
          src={openaiLogo}
          class="icon-svg"
          style="object-fit:contain"
          draggable={false}
          alt="Codex"
        />
      );
    case "claude":
      // Anthropic asterisk
      return (
        <svg {...common} fill="none">
          <path
            d="M12 3v18M5 7l14 10M5 17L19 7"
            stroke="#d97757"
            stroke-width="2.2"
            stroke-linecap="round"
          />
        </svg>
      );
    case "grok":
      // Official Grok mark (inverted for dark UI).
      return (
        <img
          src={grokLogo}
          class="icon-svg"
          style="object-fit:contain"
          draggable={false}
          alt="Grok"
        />
      );
    case "kimi":
      // User-supplied transparent Kimi mark (bundled locally).
      return (
        <img
          src={kimiLogo}
          class="icon-svg"
          style="object-fit:contain"
          draggable={false}
          alt="Kimi Code"
        />
      );
    case "openrouter":
      // Official OpenRouter mark (inverted for dark UI).
      return (
        <img
          src={openrouterLogo}
          class="icon-svg"
          style="object-fit:contain;filter:invert(1)"
          draggable={false}
          alt="OpenRouter"
        />
      );
    case "minimax":
      // Official MiniMax logo (bundled locally so it renders in the bar too).
      return (
        <img
          src={minimaxLogo}
          class="icon-svg"
          style="object-fit:contain"
          draggable={false}
          alt="MiniMax"
        />
      );
    default:
      return (
        <svg {...common} fill="none">
          <circle cx="12" cy="12" r="8" stroke="#6c7086" stroke-width="2" />
        </svg>
      );
  }
}

// ============================================================
// Collapsed status bar
// ============================================================
function Bar({
  snaps,
  activeId,
  onPick,
  onSettingsClick,
  barRef,
  staleThreshold,
  nowMs,
  refreshIntervalSecs,
  resetFlash,
}: {
  snaps: UsageSnapshot[];
  activeId: string | null;
  onPick: (label: string | null) => void;
  onSettingsClick: () => void;
  barRef: RefObject<HTMLDivElement>;
  /** Max snapshot age before the mini bar hides a segment. */
  staleThreshold: number;
  /** Live clock tick (250 ms) shared with the popup countdown ring. */
  nowMs: number;
  /** Refresh cycle interval for the gear countdown ring. */
  refreshIntervalSecs: number | null;
  /** Providers whose quota window just reset (green segment flash). */
  resetFlash: ReadonlySet<string>;
}) {
  const updateState = useUpdateState();
  const updateAvailable = updateState?.phase === "available";
  // Countdown ring on the gear button mirrors the popup refresh ring. The
  // scheduler fetches providers sequentially, so the next cycle is timed
  // from the earliest fetch across all snapshots.
  let gearFetchedAt: string | null = null;
  let gearFetchedAtMs = Infinity;
  for (const s of snaps) {
    const ms = Date.parse(s.fetched_at);
    if (Number.isFinite(ms) && ms < gearFetchedAtMs) {
      gearFetchedAtMs = ms;
      gearFetchedAt = s.fetched_at;
    }
  }
  const gearIntervalSecs = refreshIntervalSecs ?? DEFAULT_REFRESH_INTERVAL_SECS;
  const gearCountdownRemaining = gearFetchedAt
    ? refreshRemainingFraction(gearFetchedAt, gearIntervalSecs, nowMs)
    : 0;
  const [order, setOrder] = useState<string[]>(() => loadOrder());
  const [dragging, setDragging] = useState<string | null>(null);
  // Pointer-based drag state. HTML5 drag-and-drop is unreliable in the
  // embedded webview, so we implement reorder with pointer events directly.
  const dragState = useRef<{
    id: string;
    pointerId: number;
    startX: number;
    startY: number;
    // Re-anchored reference for the held card's translateX. The card visually
    // tracks the pointer as translateX = clientX - anchorX; whenever a reorder
    // shifts the card's home slot, anchorX is adjusted by the same delta so
    // the held card never jumps. startX/startY stay fixed as the press point.
    anchorX: number;
    moved: boolean;
  } | null>(null);
  // FLIP reorder animation: snapshot each segment's left position right
  // before a reorder, then after the DOM commits the new order, translate
  // every moved segment back to its old spot and animate it to the new one
  // so cards glide instead of teleporting. Consumed (set null) per reorder.
  const flipPrevLeftsRef = useRef<Map<string, number> | null>(null);
  // Latest pointer clientX, so the layout effect can keep the held card glued
  // to the cursor immediately after a reorder commits.
  const lastPointerXRef = useRef(0);

  useEffect(() => {
    saveOrder(order);
  }, [order]);

  function captureSegmentLefts(): Map<string, number> {
    const map = new Map<string, number>();
    const bar = barRef.current;
    if (!bar) return map;
    bar
      .querySelectorAll<HTMLElement>(".bar-segment[data-provider]")
      .forEach((el) => {
        const segId = el.dataset.provider;
        if (segId) map.set(segId, el.getBoundingClientRect().left);
      });
    return map;
  }

  // After a reorder commits, FLIP-slide every moved neighbour from its old
  // position to its new one, and re-anchor the held card so it stays pinned to
  // the pointer with no one-frame jump when its home slot shifts. Runs in a
  // layout effect (before paint) precisely to avoid that flicker.
  useLayoutEffect(() => {
    const prevLefts = flipPrevLeftsRef.current;
    if (!prevLefts) return;
    flipPrevLeftsRef.current = null;
    const bar = barRef.current;
    if (!bar) return;
    const ds = dragState.current;
    const draggedId = ds?.id;
    bar
      .querySelectorAll<HTMLElement>(".bar-segment[data-provider]")
      .forEach((el) => {
        const segId = el.dataset.provider;
        if (!segId) return;
        if (segId === draggedId && ds) {
          // Held card: shift anchorX by how far its home slot moved, then keep
          // it under the pointer. No transition — it must track the cursor
          // instantly (the .dragging class also forces transition: none).
          const prev = prevLefts.get(segId);
          if (prev !== undefined) {
            const shift = el.getBoundingClientRect().left - prev;
            if (Math.abs(shift) > 0.5) ds.anchorX += shift;
          }
          el.style.transition = "none";
          el.style.transform = `translateX(${lastPointerXRef.current - ds.anchorX}px) scale(1.12)`;
          return;
        }
        // Neighbour: invert to its old position, then play to the new one.
        const prev = prevLefts.get(segId);
        if (prev === undefined) return;
        const delta = prev - el.getBoundingClientRect().left;
        if (Math.abs(delta) < 0.5) return;
        el.style.transition = "none";
        el.style.transform = `translateX(${delta}px)`;
        requestAnimationFrame(() => {
          el.style.transition = "transform 170ms cubic-bezier(0.2, 0.8, 0.2, 1)";
          el.style.transform = "";
        });
      });
  }, [order]);

  // Drop: when a drag ends, the .dragging class is removed on this render
  // (restoring the base transition). Clearing the held card's inline transform
  // now lets it glide back into its home slot instead of snapping.
  useLayoutEffect(() => {
    if (dragging) return; // only when a drag ends
    const bar = barRef.current;
    if (!bar) return;
    bar
      .querySelectorAll<HTMLElement>(".bar-segment[data-provider]")
      .forEach((el) => {
        if (el.style.transform || el.style.transition) {
          el.style.transition = "";
          el.style.transform = "";
        }
      });
  }, [dragging]);

  // Render in the user's saved order; any provider not yet in the saved
  // order (e.g. a newly added one) appends at the end.
  const ordered = useMemo(() => {
    const present = snaps.map((s) => s.provider);
    return normalizeProviderOrder(order, present)
      .map((p) => snaps.find((s) => s.provider === p))
      .filter((s): s is UsageSnapshot => !!s);
  }, [order, snaps]);

  function onSegPointerDown(providerId: string, e: PointerEvent) {
    if (e.button !== 0) return;
    dragState.current = {
      id: providerId,
      pointerId: e.pointerId,
      startX: e.clientX,
      startY: e.clientY,
      anchorX: e.clientX,
      moved: false,
    };
    // NOTE: do NOT capture the pointer here — capturing on press reroutes the
    // subsequent click to the bar, which would break segment expand. Capture
    // is taken in onBarPointerMove once a real drag (past the threshold) starts.
  }

  function onBarPointerMove(e: PointerEvent) {
    const ds = dragState.current;
    if (!ds || ds.pointerId !== e.pointerId) return;
    const dist = Math.hypot(e.clientX - ds.startX, e.clientY - ds.startY);
    if (!ds.moved && dist < 4) return; // threshold: tell click apart from drag
    lastPointerXRef.current = e.clientX;
    const bar = barRef.current;
    if (!bar) return;
    if (!ds.moved) {
      ds.moved = true;
      setDragging(ds.id);
      // Now that it's a real drag (not a click), capture the pointer on the
      // bar so move/up keep firing even if the pointer leaves the bar.
      if (bar.setPointerCapture) {
        try {
          bar.setPointerCapture(e.pointerId);
        } catch {
          /* ignore */
        }
      }
    }
    // "Pick up": the held card follows the pointer horizontally (translateX)
    // and is scaled to look lifted. anchorX absorbs home-slot shifts on
    // reorder (see the layout effect) so the card never jumps. The .dragging
    // class sets transition: none so tracking is instant, and pointer-events:
    // none so elementFromPoint below still sees the real neighbour underneath.
    const dragEl = bar.querySelector<HTMLElement>(
      `.bar-segment[data-provider="${ds.id}"]`,
    );
    if (dragEl) {
      dragEl.style.transition = "none";
      dragEl.style.transform = `translateX(${e.clientX - ds.anchorX}px) scale(1.12)`;
    }
    // Reorder onto whichever segment is under the pointer.
    const under = document.elementFromPoint(e.clientX, e.clientY) as HTMLElement | null;
    const targetEl = under ? (under.closest(".bar-segment") as HTMLElement | null) : null;
    if (!targetEl) return;
    const targetId = targetEl.dataset.provider;
    if (!targetId || targetId === ds.id) return;
    const rect = targetEl.getBoundingClientRect();
    const after = e.clientX - rect.left > rect.width / 2;
    const from = ds.id;
    flipPrevLeftsRef.current = captureSegmentLefts();
    setOrder((prev) =>
      reorderProviderOrder(
        prev,
        snaps.map((snap) => snap.provider),
        from,
        targetId,
        after,
      ),
    );
  }

  function onBarPointerUp(e: PointerEvent) {
    const ds = dragState.current;
    if (!ds || ds.pointerId !== e.pointerId) return;
    dragState.current = null;
    setDragging(null);
    const bar = barRef.current;
    if (bar && bar.releasePointerCapture) {
      try {
        bar.releasePointerCapture(e.pointerId);
      } catch {
        /* ignore */
      }
    }
    // Inline-transform cleanup is handled by the drop layout effect, which
    // runs after the .dragging class is removed so the held card animates
    // back to its home slot instead of snapping.
  }

  return (
    <div
      class={`bar compact width-fluid ${activeId ? "provider-expanded" : ""} ${dragging ? "dragging-active" : ""}`}
      ref={barRef}
      onPointerMove={onBarPointerMove}
      onPointerUp={onBarPointerUp}
    >
      <div
        class="bar-drag"
        title="AI Usage Tracker"
        onMouseDown={(e) => e.button === 0 && currentWindow.startDragging()}
      >
        <svg width="18" height="18" viewBox="0 0 9 9" fill="currentColor" aria-hidden="true">
          <circle cx="1" cy="1" r="0.95" />
          <circle cx="4.5" cy="1" r="0.95" />
          <circle cx="8" cy="1" r="0.95" />
          <circle cx="1" cy="4.5" r="0.95" />
          <circle cx="4.5" cy="4.5" r="0.95" />
          <circle cx="8" cy="4.5" r="0.95" />
          <circle cx="1" cy="8" r="0.95" />
          <circle cx="4.5" cy="8" r="0.95" />
          <circle cx="8" cy="8" r="0.95" />
        </svg>
      </div>

      {snaps.length === 0 && (
        <div class="bar-empty">
          No providers configured
        </div>
      )}

      {ordered.filter((s) => !isStaleSnapshot(s, staleThreshold) && hasDisplayableWindows(s)).map((snap) => {
        const id = snap.provider;
        const iconId = iconIdFor(id);
        const isActive = activeId === id;
        const sum = summary(snap);
        const redLineCritical =
          isWeeklyUnderRedLine(snap.windows, snap.provider) ||
          isFiveHourUnderRedLine(snap.windows, snap.provider);
        const hasWindows = snap.windows.length > 0;
        const isSoftEmpty =
          !!snap.unavailable_reason && !hasWindows &&
          isSoftEmptyReason(snap.unavailable_reason);
        const isStale = hasWindows && !!snap.unavailable_reason;
        const inner = hasWindows ? (
          <>
            <div class="bar-track-mini">
              <div
                class="bar-fill-mini"
                style={`width:${Math.max(3, sum.worstRemaining)}%;background-color:${fillColor(sum.colorPercent)}`}
              />
            </div>
            <div class="bar-segment-text">
              {sum.text.split(" · ").map((part, i) => (
                <>
                  {i > 0 && <span style="color:var(--fg-dim)"> · </span>}
                  <span class="pct">{part.split(" ")[0]}</span>
                  <span class="lbl"> {part.split(" ").slice(1).join(" ")}</span>
                </>
              ))}
            </div>
            {isStale && (
              <span
                class="bar-segment-stale"
                title={snap.unavailable_reason ?? ""}
                aria-label="stale local data"
              >
                stale
              </span>
            )}
          </>
        ) : isSoftEmpty ? (
          <span class="bar-segment-empty">{snap.unavailable_reason}</span>
        ) : (
          <span class="bar-segment-error">{snap.unavailable_reason}</span>
        );
        return (
          <div
            key={id}
            class={`bar-segment ${isActive ? "active" : ""} ${dragging === id ? "dragging" : ""} ${redLineCritical ? "red-line-critical" : ""} ${resetFlash.has(id) ? "reset-flash" : ""}`}
            data-provider={id}
            style="touch-action:none"
            onPointerDown={(e) => onSegPointerDown(id, e)}
            onClick={() => onPick(isActive ? null : id)}
            title={`${id} — click for details, drag to reorder`}
          >
            <div class="bar-segment-icon">
              <ProviderIcon id={iconId} />
            </div>
            {inner}
          </div>
        );
      })}

      <div class="bar-spacer" aria-hidden="true" />

      <button
        class="bar-btn bar-btn-settings"
        aria-label="Settings"
        onClick={(e) => {
          e.stopPropagation();
          onSettingsClick();
        }}
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
        {gearFetchedAt && (
          <RefreshCountdownRing remaining={gearCountdownRemaining} />
        )}
        {updateAvailable && <span class="update-badge" aria-hidden="true" />}
      </button>
    </div>
  );
}

// ============================================================
// Expanded popup
// ============================================================
function IntroPanel({ onDismiss }: { onDismiss: () => void }) {
  return (
    <div class="popup-intro">
      <ul>
        <li>
          <strong>Percent</strong> shows how much of each rate-limit window is
          still available. 100% = full quota, lower = more consumed.
        </li>
        <li>
          <strong>Colors</strong> shift smoothly from bright green (healthy)
          through orange to bright red (nearly exhausted) as your quota is
          used up.
        </li>
        <li>
          <strong>Blue line</strong> marks the even-pace target — where your
          remaining % should be if you spread usage evenly across the whole
          window.
        </li>
        <li>
          <strong>Red line</strong> shows the near-term target: one day
          (weekly) or one hour (5-hour) of your currently available %/day
          or %/hour below the blue line, tightening as quota runs low.
        </li>
        <li>
          <strong>Click a provider</strong> in the bar to see reset times,
          a burn-rate chart, and detailed usage (per-model splits, USD
          counters, per-product rows where the provider reports them).
        </li>
        <li>
          <strong>Drag a provider segment</strong> sideways to reorder it.
        </li>
        <li>
          Open <strong>Settings</strong> from the gear icon to add keys,
          sign in with OAuth, change the refresh interval, or hide
          providers.
        </li>
        <li>
          Models appear on the bar only when fresh data is available — on
          startup they load in as each provider responds.
        </li>
      </ul>
      <button class="popup-intro-dismiss" onClick={onDismiss}>
        Got it
      </button>
    </div>
  );
}

/** Live status for the per-provider popup refresh control. */
export type ProviderRefreshPhase =
  | "waiting"
  | "fetching"
  | "applying"
  | "done"
  | "error";

export interface ProviderRefreshState {
  providerLabel: string;
  phase: ProviderRefreshPhase;
  message: string;
  /** Busy-wait attempt (1-based) while phase is waiting. */
  attempt?: number;
  /** Seconds left in the current 2s retry delay. */
  retryInSecs?: number;
}

function Popup({
  snap,
  popupRef,
  hidden,
  closing,
  switching,
  showIntro,
  setShowIntro,
  onRefresh,
  refreshState,
  refreshIntervalSecs,
  settingsIntervalSecs,
  nowMs,
  burnLookup,
}: {
  snap: UsageSnapshot;
  popupRef: Ref<HTMLDivElement>;
  hidden: boolean;
  closing: boolean;
  switching: boolean;
  showIntro: boolean;
  setShowIntro: (v: boolean) => void;
  /** Optional: omitted on the hidden measure-layer clone. */
  onRefresh?: () => void;
  /** Progress for this card's refresh (null when idle / other provider). */
  refreshState?: ProviderRefreshState | null;
  /** Interval for the current countdown cycle (frozen mid-cycle). */
  refreshIntervalSecs?: number;
  /** Live Settings interval (may differ until the next refresh). */
  settingsIntervalSecs?: number;
  nowMs?: number;
  /** Burn history buckets indexed by provider id then window label. */
  burnLookup: Map<string, Map<string, BurnBucket[]>>;
}) {
  const iconId = iconIdFor(snap.provider);
  const canRefresh = !!onRefresh && providerIdFor(snap.provider) !== null;
  const refreshBusy =
    !!refreshState &&
    (refreshState.phase === "waiting" ||
      refreshState.phase === "fetching" ||
      refreshState.phase === "applying");
  const showProgress = !!refreshState;
  const countdownNow = nowMs ?? Date.now();
  // Ring uses the interval frozen for this cycle (not a mid-cycle Settings change).
  const countdownInterval = refreshIntervalSecs ?? DEFAULT_REFRESH_INTERVAL_SECS;
  const countdownRemaining = refreshRemainingFraction(
    snap.fetched_at,
    countdownInterval,
    countdownNow,
  );
  const countdownTitle = refreshCountdownLabel(
    snap.fetched_at,
    countdownInterval,
    countdownNow,
    settingsIntervalSecs,
  );
  const refreshTitle = `Refresh ${snap.provider} — ${countdownTitle}`;
  return (
    <div
      class={`popup ${hidden ? "preparing" : closing ? "closing" : "ready"} ${switching ? "switching" : ""} ${refreshBusy ? "is-refreshing" : ""}`}
      ref={popupRef}
      aria-hidden={hidden ? "true" : undefined}
      aria-busy={refreshBusy ? "true" : undefined}
    >
      <div class="popup-header">
        <div style="width:18px;height:18px">
          <ProviderIcon id={iconId} size={18} />
        </div>
        <div style="flex:1">
          <div class="popup-title">{snap.provider}</div>
          {snap.level && <div class="popup-subtitle">{snap.level}</div>}
        </div>
        {canRefresh && (
          <div class="popup-header-actions">
            <button
              type="button"
              class={`popup-refresh ${refreshBusy ? "is-busy" : ""}`}
              title={refreshTitle}
              aria-label={refreshTitle}
              disabled={!!refreshBusy}
              onClick={(e) => {
                e.stopPropagation();
                onRefresh?.();
              }}
            >
              <RefreshCountdownRing
                remaining={countdownRemaining}
                title={countdownTitle}
              />
              <span class="popup-refresh-icon">
                <RefreshIcon />
              </span>
            </button>
          </div>
        )}
      </div>

      {showProgress && refreshState && (
        <div
          class={`popup-refresh-status popup-refresh-status--${refreshState.phase}`}
          role="status"
          aria-live="polite"
        >
          <div class="popup-refresh-status-row">
            <span class="popup-refresh-status-text">{refreshState.message}</span>
            {refreshBusy && (
              <span class="popup-refresh-status-hint" aria-hidden="true">
                {refreshState.phase === "waiting"
                  ? refreshState.retryInSecs != null && refreshState.retryInSecs > 0
                    ? `1/3 · ${refreshState.retryInSecs}s`
                    : refreshState.attempt != null
                      ? `1/3 · #${refreshState.attempt}`
                      : "1/3"
                  : refreshState.phase === "fetching"
                    ? "2/3"
                    : "3/3"}
              </span>
            )}
          </div>
          {/*
            Three checks on every refresh (manual + auto):
            1/3 waiting — provider busy → wait 2s (shown) and retry
            2/3 fetching — live provider call
            3/3 applying — pull snapshot into the UI
          */}
          {refreshBusy && (
            <div
              class="popup-refresh-progress"
              role="progressbar"
              aria-valuetext={refreshState.message}
              aria-label="Refresh progress"
            >
              <div class="popup-refresh-steps" aria-hidden="true">
                <span
                  class={`popup-refresh-step ${
                    refreshState.phase === "waiting" ||
                    refreshState.phase === "fetching" ||
                    refreshState.phase === "applying"
                      ? "is-done"
                      : ""
                  } ${refreshState.phase === "waiting" ? "is-active" : ""}`}
                  title="1/3 Busy check / wait"
                />
                <span
                  class={`popup-refresh-step ${
                    refreshState.phase === "fetching" ||
                    refreshState.phase === "applying"
                      ? "is-done"
                      : ""
                  } ${refreshState.phase === "fetching" ? "is-active" : ""}`}
                  title="2/3 Fetch"
                />
                <span
                  class={`popup-refresh-step ${
                    refreshState.phase === "applying" ? "is-done is-active" : ""
                  }`}
                  title="3/3 Apply"
                />
              </div>
              {refreshState.phase === "waiting" &&
              refreshState.retryInSecs != null &&
              refreshState.retryInSecs > 0 ? (
                <div
                  class="popup-refresh-wait-track"
                  aria-hidden="true"
                >
                  <div
                    key={`wait-${refreshState.attempt ?? 0}-${refreshState.retryInSecs}`}
                    class="popup-refresh-wait-fill"
                    style={{
                      animationDuration: "1s",
                    }}
                  />
                </div>
              ) : (
                <div
                  key={`${refreshState.phase}-${refreshState.message}`}
                  class={`popup-refresh-progress-bar ${
                    refreshState.phase === "waiting"
                      ? "phase-wait"
                      : refreshState.phase === "fetching"
                        ? "phase-fetch"
                        : "phase-apply"
                  }`}
                />
              )}
            </div>
          )}
        </div>
      )}

      <div class="popup-divider" />

      {snap.windows.length === 0 && (
              <div style="color:#b8bfd8;font-size:11px;padding:6px 0">
                No usage windows available.
              </div>
            )}

      {snap.windows.map((w) => {
        const unlimited = w.is_unlimited;
        const dollarDetail = formatDollarWindow(w);
        const popupOnlyDollarDetail = !w.bar_visible && dollarDetail !== null;
        const remaining = Math.max(0, 100 - w.used_percent);
        const fillWidth = Math.max(2, remaining);
        const weeklyW = isWeeklyWindow(w.label);
        const fiveHourW = isFiveHourWindow(w.label);
        const burnBuckets =
          w.bar_visible && (weeklyW || fiveHourW)
            ? burnLookup.get(providerIdFor(snap.provider) ?? "")?.get(w.label)
            : undefined;
        // Projection extrapolates the recent burn rate (last hour for 5h,
        // last 6h for weekly) when history exists, else the period average.
        const recentRate = recentProjectionRate(w, burnBuckets);
        const evenPace = !unlimited
          ? resolveEvenPace(w, snap.provider, Date.now(), recentRate)
          : null;
        const colorPercent = evenPace ? evenPace.gradientPercent : remaining;
        const fill = fillColor(colorPercent);
        const rowCritical =
          isWeeklyWindowUnderRedLine(w, snap.provider) ||
          isFiveHourWindowUnderRedLine(w, snap.provider);
        // Absolute used/limit counters, shown inside "More details". Kimi
        // reports plain counters; OpenRouter reports USD — format as currency.
        // Claude's per-model rows carry only used_absolute (exact tokens).
        // A /100 limit only restates the percentage already on the bar
        // ("43 / 100 used" ≡ "43% used") — hide that mirror on the paced
        // 5h/weekly windows; real counters (e.g. 250 / 1,000) stay.
        const providerId = providerIdFor(snap.provider);
        const openrouterW = providerId === "openrouter";
        const percentMirror = (weeklyW || fiveHourW) && w.limit_absolute === 100;
        const absoluteNote =
          w.used_absolute != null && w.limit_absolute != null
            ? openrouterW
              ? `$${w.used_absolute.toFixed(2)} / $${w.limit_absolute.toFixed(2)} used`
              : percentMirror
                ? null
                : `${Math.round(w.used_absolute).toLocaleString()} / ${Math.round(w.limit_absolute).toLocaleString()} used`
            : w.used_absolute != null && providerId === "claude"
              ? `${Math.round(w.used_absolute).toLocaleString()} tokens`
              : null;
        // OpenRouter monthly cap: linear month-end dollar projection.
        const extraNote = openrouterW ? dollarMonthlyProjectionNote(w) : null;
        return (
          <div class={`popup-section ${rowCritical ? "red-line-critical" : ""}`}>
            {popupOnlyDollarDetail ? (
              <>
                <div class="popup-section-head">
                  <span class="popup-section-label">{prettyLabel(w.label, snap.provider)}</span>
                  <span class="popup-section-meta">Credit usage</span>
                </div>
                <div class="popup-section-foot">
                  <span>{dollarDetail}</span>
                </div>
              </>
            ) : (
              <>
                <div class="popup-section-head">
                  <span class="popup-section-label">{prettyLabel(w.label, snap.provider)}</span>
                  <span class="popup-section-meta">
                    {w.is_unlimited ? "∞ Unlimited" : w.label.toLowerCase().startsWith("total ")
                      ? "Lifetime limit"
                      : w.label.toLowerCase().startsWith("balance ") && !w.reset_at
                      ? "Resets in ∞"
                      : w.reset_at && new Date(w.reset_at).getTime() <= Date.now()
                      ? "Resetting…"
                      : `Resets in ${formatReset(w.reset_at)}`}
                  </span>
                </div>
                <div class="bar-track-full">
                  {unlimited ? (
                    <span class="bar-unlimited" aria-label="Unlimited weekly usage">∞</span>
                  ) : (
                    <div class="bar-fill-full" style={`width:${fillWidth}%;background-color:${fill}`} />
                  )}
                  {!unlimited && evenPace?.tickPercentages.map((left) => (
                    <span
                      key={`${evenPace.targetLabel}-${left}`}
                      class="bar-pace-tick"
                      style={`left:${left}%`}
                      aria-hidden="true"
                    />
                  ))}
                  {!unlimited && evenPace && (
                    <span
                      class="bar-pace-target"
                      style={`left:${evenPace.targetRemainingPercent}%`}
                      title={`Even ${evenPace.targetLabel} pace target: ${Math.round(evenPace.targetRemainingPercent)}% remaining`}
                      aria-label={`Even ${evenPace.targetLabel} pace target at ${Math.round(evenPace.targetRemainingPercent)}% remaining`}
                    />
                  )}
                  {!unlimited && evenPace && (
                    <span
                      class="bar-pace-target-sub"
                      style={`left:${evenPace.subTargetRemainingPercent}%`}
                      title={`${evenPace.subTargetKind === "daily" ? "Today's" : "This hour's"} pace target: ${Math.round(evenPace.subTargetRemainingPercent)}% remaining`}
                      aria-label={`${evenPace.subTargetKind === "daily" ? "Today's" : "This hour's"} pace target at ${Math.round(evenPace.subTargetRemainingPercent)}% remaining`}
                    />
                  )}
                </div>
                <div class="popup-section-foot">
                  {unlimited ? (
                    <span>Unlimited weekly usage</span>
                  ) : (
                    <>
                      <span>{dollarDetail ?? `${Math.round(remaining)}% left`}</span>
                      <span style="color:#9ba3bd">{Math.round(w.used_percent)}% used</span>
                    </>
                  )}
                </div>
                {hasBurnHistory(burnBuckets) && (
                  <>
                    <BurnBars buckets={burnBuckets ?? []} />
                    <div class="burn-bars-caption">
                      <span>{weeklyW ? "7d ago" : "5h ago"}</span>
                      <span>now · 1 bar ≈ {formatDurationApprox(bucketSecsForLabel(w.label, weeklyW) * 1000)}</span>
                    </div>
                  </>
                )}
                {evenPace && (
                  <PaceNotesDropdown
                    note={evenPace.note}
                    gapNote={evenPace.gapNote}
                    timeNote={evenPace.timeNote}
                    projectionNote={evenPace.projectionNote}
                    absoluteNote={absoluteNote}
                    extraNote={extraNote}
                    usedTodayNote={
                      weeklyW && hasBurnHistory(burnBuckets)
                        ? `${Math.round(burnedTodayPercent(burnBuckets) ?? 0)}% of weekly quota burned today`
                        : null
                    }
                  />
                )}
                {!evenPace && absoluteNote && (
                  <PaceNotesDropdown
                    note={null}
                    gapNote={null}
                    timeNote={null}
                    projectionNote={null}
                    absoluteNote={absoluteNote}
                    extraNote={extraNote}
                    usedTodayNote={null}
                  />
                )}
              </>
            )}
          </div>
        );
      })}

      <div class="popup-footer">
        <div class="popup-help">
          Click a provider for details. Drag providers to reorder.
        </div>
        <a
          class="popup-intro-link"
          href="#"
          onClick={(e) => {
            e.preventDefault();
            setShowIntro(!showIntro);
          }}
        >
          {showIntro ? "Hide intro" : "What is this?"}
        </a>
      </div>
      {showIntro && <IntroPanel onDismiss={() => setShowIntro(false)} />}
    </div>
  );
}

// Embedded settings popup, attached to the overlay window. Slides up on
// open and slides down on close. The inner content is the shared
// SettingsPanel so the standalone Settings window and this popup stay in
// lockstep.
function SettingsPopup({
  popupRef,
  hidden,
  closing,
}: {
  popupRef: Ref<HTMLDivElement>;
  hidden: boolean;
  closing: boolean;
}) {
  const [maxHeight, setMaxHeight] = useState(SETTINGS_PANEL_FALLBACK_HEIGHT);
  const [canScrollUp, setCanScrollUp] = useState(false);
  const [canScrollDown, setCanScrollDown] = useState(false);
  const [scrollPosition, setScrollPosition] = useState({
    visible: false,
    size: 0,
    offset: 0,
  });
  const scrollRef = useRef<HTMLDivElement>(null);

  function updateScrollCues() {
    const el = scrollRef.current;
    if (!el) return;
    const range = Math.max(0, el.scrollHeight - el.clientHeight);
    const visible = range > 2;
    const railHeight = Math.max(0, el.clientHeight - 8);
    const size = visible
      ? Math.max(24, Math.round(railHeight * (el.clientHeight / el.scrollHeight)))
      : railHeight;
    const offset = visible
      ? Math.round((railHeight - size) * (el.scrollTop / range))
      : 0;
    setCanScrollUp(el.scrollTop > 2);
    setCanScrollDown(el.scrollTop + el.clientHeight < el.scrollHeight - 2);
    setScrollPosition((previous) =>
      previous.visible === visible &&
      previous.size === size &&
      previous.offset === offset
        ? previous
        : { visible, size, offset },
    );
  }

  useEffect(() => {
    let cancelled = false;
    void Promise.all([
      currentMonitor(),
      currentWindow.outerPosition(),
      currentWindow.outerSize(),
    ])
      .then(async ([fallbackMonitor, position, size]) => {
        // The transparent preallocated window can span monitors. Resolve the
        // monitor under its bottom edge, where the minibar actually lives.
        const barMonitor = await monitorFromPoint(
          position.x + size.width / 2,
          position.y + size.height - 1,
        );
        const monitor = barMonitor ?? fallbackMonitor;
        if (cancelled || !monitor) return;
        setMaxHeight(
          settingsPanelMaxHeight(
            monitor.workArea.size.height,
            monitor.scaleFactor,
            COMPACT_BAR_H_LOGICAL,
            size.height,
          ),
        );
      })
      .catch((e) => console.error("current monitor lookup failed", e));
    return () => {
      cancelled = true;
    };
  }, []);

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = 0;
    const raf = requestAnimationFrame(() => {
      el.scrollTop = 0;
      updateScrollCues();
    });
    const observer =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(updateScrollCues)
        : null;
    observer?.observe(el);
    if (el.firstElementChild) observer?.observe(el.firstElementChild);
    return () => {
      cancelAnimationFrame(raf);
      observer?.disconnect();
    };
  }, [maxHeight]);

  // `preparing` = mounted & painting while HWND region is still bar-only
  // (user cannot see it). `ready` = region expanded onto painted pixels.
  return (
    <div
      class={`settings-popup ${closing ? "closing" : hidden ? "preparing" : "ready"}`}
      ref={popupRef}
      aria-hidden={hidden ? "true" : undefined}
      style={`--settings-max-height:${maxHeight}px`}
    >
      <div class="settings-popup-header">
        <img class="settings-popup-brand-logo" src={brandLogo} alt="AI Usage Tracker" />
        <SettingsHeaderActions />
      </div>
      <div class="settings-popup-divider" />
      <div class={`settings-scroll-shell ${canScrollUp ? "can-scroll-up" : ""} ${canScrollDown ? "can-scroll-down" : ""}`}>
        <div class="settings-scroll-content" ref={scrollRef} onScroll={updateScrollCues}>
          <SettingsPanel />
        </div>
        <div
          class={`settings-scroll-position ${scrollPosition.visible ? "visible" : ""}`}
          aria-hidden="true"
        >
          <span
            style={`height:${scrollPosition.size}px;transform:translateY(${scrollPosition.offset}px)`}
          />
        </div>
        <div class="settings-scroll-cue settings-scroll-cue-top" aria-hidden="true">
          <span />
        </div>
        <div class="settings-scroll-cue settings-scroll-cue-bottom" aria-hidden="true">
          <span />
        </div>
      </div>
    </div>
  );
}

// ============================================================
// Root overlay — manages state + window resize on expand
// ============================================================
export function Overlay() {
  const [snaps, setSnaps] = useState<UsageSnapshot[]>([]);
  /** True after the first get_usage attempt settles (ok or error). First-show
   *  sizing waits on this so we never lock the minibar to the empty chrome
   *  width before rehydrated / live trackers are known. */
  const [usageReady, setUsageReady] = useState(false);
  /** Live Settings → refresh interval (may change mid-cycle). */
  const [settingsIntervalSecs, setSettingsIntervalSecs] = useState(
    DEFAULT_REFRESH_INTERVAL_SECS,
  );
  /**
   * Interval driving the popup countdown ring for the *current* cycle.
   * Stays on the previous value when Settings changes mid-cycle; adopts the
   * new setting only after a real provider fetch advances `fetched_at`.
   */
  const [cycleIntervalSecs, setCycleIntervalSecs] = useState(
    DEFAULT_REFRESH_INTERVAL_SECS,
  );
  const settingsIntervalRef = useRef(DEFAULT_REFRESH_INTERVAL_SECS);
  const cycleIntervalRef = useRef(DEFAULT_REFRESH_INTERVAL_SECS);
  const prevFetchedAtRef = useRef<Map<string, string>>(new Map());
  const intervalBootstrappedRef = useRef(false);
  const [nowMs, setNowMs] = useState(() => Date.now());
  /** Providers whose quota window just reset — bar segment flashes green. */
  const [resetFlash, setResetFlash] = useState<ReadonlySet<string>>(new Set());
  const [providerRefresh, setProviderRefresh] =
    useState<ProviderRefreshState | null>(null);
  const providerRefreshTimerRef = useRef<number | null>(null);
  /** True while the user-triggered popup refresh button is running. */
  const manualRefreshInFlightRef = useRef(false);
  const [activeLabel, setActiveLabel] = useState<string | null>(null);
  /** Burn history snapshot for popup burn bars (weekly + 5h windows). */
  const [burnHist, setBurnHist] = useState<ProviderBurnHistory[]>([]);
  /** Memoized provider-id → label → burn buckets lookup for fast popup access. */
  const burnLookup = useMemo(() => {
    const map = new Map<string, Map<string, BurnBucket[]>>();
    for (const entry of burnHist) {
      const inner = new Map<string, BurnBucket[]>();
      for (const w of entry.windows) {
        inner.set(w.label, w.buckets);
      }
      map.set(entry.id, inner);
    }
    return map;
  }, [burnHist]);
  const [hasExpanded, setHasExpanded] = useState(false);
  // Per-provider popup is mounted as soon as a segment is picked, but stays
  // hidden until the window has finished resizing to fit it. Otherwise the
  // popup's CSS fade-in runs visibly while the window is still at the
  // collapsed size — a brief flash before the window grows around it.
  const [popupReady, setPopupReady] = useState(false);
  // Slide-out animation flag for the per-provider popup. Set to true on
  // close, then cleared 180ms later when the popup actually unmounts so
  // the keyframes have time to play.
  const [providerClosing, setProviderClosing] = useState(false);
  // Brief crossfade flag used when switching directly from one provider's
  // popup to another (without fully closing first). The popup content dims
  // and slides slightly, swaps, then restores — all via CSS transition.
  const [providerSwitching, setProviderSwitching] = useState(false);
  // Embedded settings popup, opened from the ⋮ button on the bar. Mutually
  // exclusive with a provider popup so the overlay never expands twice.
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsClosing, setSettingsClosing] = useState(false);
  /** Bumped on each open so SettingsPanel remounts and status banners reset. */
  const [settingsInstance, setSettingsInstance] = useState(0);
  const settingsPopupRef = useRef<HTMLDivElement>(null);
  // One-shot lifecycle reducer for the Settings close path. The reducer is
  // synchronous and ref-backed so duplicate DOM `blur` + Tauri focus-loss
  // notifications for one outside-click collapse into a single close
  // animation and a single unmount.
  const settingsClosePhaseRef = useRef<SettingsClosePhase>("closed");
  const settingsCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(
    null,
  );
  // Opt-in intro panel, toggled from "What is this?" in the popup footer.
  // Always starts hidden; no persistence needed since the link toggles it.
  const [showIntro, setShowIntro] = useState<boolean>(false);
  const popupRef = useRef<HTMLDivElement>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const widgetRef = useRef<HTMLDivElement>(null);
  const hitHeightRef = useRef(COMPACT_BAR_H_LOGICAL);
  // Monotonic token so a stale `invoke` resolve from a prior click doesn't
  // reveal the popup before the latest resize has committed.
  const resizeTokenRef = useRef(0);
  // Last popup height (logical px) we asked Rust to size the window to, and
  // the tallest popup height seen so far. The overlay is transparent, so
  // every SetWindowPos repaints the surface for a frame and flashes. We
  // therefore pre-size ONCE (while still hidden) to the tallest popup and
  // then only grow further if needed — open/close/switch never shrink.
  // `maxHeightRef` is monotonic for the session; `appliedHeightRef` starts at
  // the native preallocated height and skips IPC for smaller targets.
  const appliedHeightRef = useRef<number | null>(
    OVERLAY_PREALLOCATED_H_LOGICAL,
  );
  const maxHeightRef = useRef(0);
  // Hidden measurement layer: one ref per provider popup, used to read each
  // popup's natural height so we can pre-size the window. Settings is NOT
  // measured here — mounting SettingsPanel twice (measure + open) caused
  // stale checkbox state and double getStatus fetches. Settings grows the
  // window on first open via the expand path instead.
  const measureRefs = useRef<Record<string, HTMLDivElement | null>>({});
  // Guards the one-time `show()` after the window has been pre-sized.
  const shownRef = useRef(false);
  // A real outside click can arrive while the popup is still painting behind
  // the bar-only HWND region. Latch that focus loss and resolve it after the
  // region is ready instead of dropping the only foreground transition.
  const pendingOutsideCloseRef = useRef(false);
  // Acts-once guard for the outside-close paths. A window deactivate can fan
  // out into several Tauri `Focused(false)` events, and a click in the native
  // reserve can also reach the document listener; without this, each one
  // re-runs the synchronous close.
  const outsideCloseFiredRef = useRef(false);
  // Unlike the Preact `popupReady` render flag, this becomes true only after
  // the native content region has expanded. Focus loss before then is deferred.
  const popupReadyRef = useRef(false);
  // Mirrors of the popup-open state, read by the single mount-time
  // focus-loss listener so it never closes against a stale render's state.
  const activeLabelRef = useRef(activeLabel);
  const settingsOpenRef = useRef(settingsOpen);
  useEffect(() => {
    activeLabelRef.current = activeLabel;
  }, [activeLabel]);
  useEffect(() => {
    settingsOpenRef.current = settingsOpen;
  }, [settingsOpen]);

  // Clear the Settings close timer if the overlay unmounts mid-animation so
  // a stale callback cannot call `setSettingsOpen(false)` on a freshly opened
  // popup (the user would see Settings silently disappear).
  useEffect(() => {
    return () => {
      if (settingsCloseTimerRef.current !== null) {
        window.clearTimeout(settingsCloseTimerRef.current);
        settingsCloseTimerRef.current = null;
      }
      if (providerRefreshTimerRef.current !== null) {
        window.clearTimeout(providerRefreshTimerRef.current);
        providerRefreshTimerRef.current = null;
      }
    };
  }, []);

  /** Read the live bar height from the DOM. Falls back to the configured
   *  default for the current view if the bar isn't mounted yet. The native
   *  window's hit-strip and content geometry use this value so the
   *  transparent space above the bar never captures clicks and the window
   *  height tracks whatever the user picked. */
  function barHeight(): number {
    return barRef.current?.offsetHeight ?? COMPACT_BAR_H_LOGICAL;
  }

  function currentHitWidth(): number {
    const collapsed =
      collapsedBarWidthRef.current ??
      barRef.current?.getBoundingClientRect().width ??
      EXPANDED_PANEL_MIN_WIDTH;
    const expanded =
      activeLabelRef.current !== null || settingsOpenRef.current;
    return overlayWindowWidth(collapsed, expanded);
  }

  /** Report the interactive bottom-right content rectangle to Rust. Empty
   *  reserve above and to the left remains genuinely click-through. */
  function setHitHeight(
    logicalPx: number,
    logicalWidth = currentHitWidth(),
  ): Promise<void> {
    hitHeightRef.current = logicalPx;
    return invoke("set_overlay_hit_height", { height: logicalPx, width: logicalWidth }).catch((e) => {
      console.error("set_overlay_hit_height failed", e);
    }) as Promise<void>;
  }

  function setHitWidth(logicalWidth: number): Promise<void> {
    const logicalPx = hitHeightRef.current;
    return invoke("set_overlay_hit_height", { height: logicalPx, width: logicalWidth }).catch((e) => {
      console.error("set_overlay_hit_height failed", e);
    }) as Promise<void>;
  }

  function beginOutsideCloseCycle(
    nextActiveLabel: string | null,
    nextSettingsOpen: boolean,
  ) {
    pendingOutsideCloseRef.current = false;
    outsideCloseFiredRef.current = false;
    activeLabelRef.current = nextActiveLabel;
    settingsOpenRef.current = nextSettingsOpen;
    popupReadyRef.current = false;
  }

  /** Update Settings interval. Only adopts into the countdown on bootstrap
   *  or when `adoptForCountdown` is true (after a real fetch cycle). */
  function applySettingsInterval(
    value: unknown,
    opts?: { adoptForCountdown?: boolean },
  ) {
    const secs = normalizeRefreshIntervalSecs(value);
    if (secs == null) return;
    settingsIntervalRef.current = secs;
    setSettingsIntervalSecs(secs);
    if (opts?.adoptForCountdown || !intervalBootstrappedRef.current) {
      intervalBootstrappedRef.current = true;
      cycleIntervalRef.current = secs;
      setCycleIntervalSecs(secs);
    }
  }

  /** After a refresh actually lands, the ring starts a new cycle using the
   *  current Settings interval (which may have changed mid-cycle). */
  function adoptCycleIntervalFromSettings() {
    const secs = settingsIntervalRef.current;
    if (cycleIntervalRef.current === secs) return;
    cycleIntervalRef.current = secs;
    setCycleIntervalSecs(secs);
  }

  async function pull() {
    try {
      const [data, interval, burn] = await Promise.all([
        getUsage(),
        getRefreshInterval().catch(() => null),
        // Burn history is decorative; never let its failure fail get_usage.
        getBurnHistory().catch((e) => {
          console.error("get_burn_history failed", e);
          return [] as ProviderBurnHistory[];
        }),
      ]);
      // Settings value can change anytime; do not re-scale the ring mid-cycle.
      if (interval != null) applySettingsInterval(interval);

      let anyFetchAdvanced = false;
      for (const s of data) {
        const prev = prevFetchedAtRef.current.get(s.provider);
        if (prev != null && prev !== s.fetched_at) {
          anyFetchAdvanced = true;
        }
        prevFetchedAtRef.current.set(s.provider, s.fetched_at);
      }
      if (anyFetchAdvanced) {
        adoptCycleIntervalFromSettings();
      }

      setSnaps(data);
      setBurnHist(burn);
      setUsageReady(true);
    } catch (e) {
      console.error("get_usage failed", e);
      // Still unblock first-show so a failed pull cannot leave the HWND hidden.
      setUsageReady(true);
    }
  }

  function clearProviderRefreshTimer() {
    if (providerRefreshTimerRef.current !== null) {
      window.clearTimeout(providerRefreshTimerRef.current);
      providerRefreshTimerRef.current = null;
    }
  }

  function finishProviderRefresh(
    next: ProviderRefreshState,
    clearAfterMs = 1600,
  ) {
    setProviderRefresh(next);
    clearProviderRefreshTimer();
    providerRefreshTimerRef.current = window.setTimeout(() => {
      setProviderRefresh((cur) =>
        cur && cur.phase === next.phase && cur.providerLabel === next.providerLabel
          ? null
          : cur,
      );
      providerRefreshTimerRef.current = null;
    }, clearAfterMs);
  }

  async function onProviderRefresh(providerLabel: string) {
    const id = providerIdFor(providerLabel);
    if (!id) return;
    if (
      providerRefresh &&
      (providerRefresh.phase === "waiting" ||
        providerRefresh.phase === "fetching" ||
        providerRefresh.phase === "applying")
    ) {
      return;
    }
    manualRefreshInFlightRef.current = true;
    clearProviderRefreshTimer();
    setProviderRefresh({
      providerLabel,
      phase: "waiting",
      message: "1/3 Checking if provider is free…",
    });
    try {
      // Backend emits `waiting` (busy → sleep 2s → retry) and `started` events
      // for the open-card progress strip while this invoke runs.
      const result = await refreshProvider(id);
      setProviderRefresh({
        providerLabel,
        phase: "applying",
        message: result.ok
          ? "3/3 Updating card…"
          : "3/3 Applying result…",
      });
      // Always re-pull so the bar/popup reflect last-good holds, removals, or
      // stale badges even when the provider call failed.
      await pull();
      if (result.ok) {
        finishProviderRefresh({
          providerLabel,
          phase: "done",
          message: result.message || "Updated",
        });
      } else {
        finishProviderRefresh(
          {
            providerLabel,
            phase: "error",
            message: result.message || "Refresh failed",
          },
          3200,
        );
      }
    } catch (e) {
      console.error("refresh_provider failed", e);
      // Still try to sync UI if the invoke failed after a partial update.
      try {
        await pull();
      } catch {
        /* ignore */
      }
      const detail =
        e instanceof Error
          ? e.message
          : typeof e === "string"
            ? e
            : "Refresh failed";
      finishProviderRefresh(
        {
          providerLabel,
          phase: "error",
          message: detail,
        },
        3200,
      );
    } finally {
      manualRefreshInFlightRef.current = false;
    }
  }

  // Poll for snapshot updates every 5s — the scheduler pushes fresh data
  // into the in-memory map on its configured interval; save_key triggers an
  // instant refresh. The popup's manual refresh button (`refreshProvider`)
  // fetches on demand; this poll just keeps the bar current between presses.
  useEffect(() => {
    void pull();
    const id = setInterval(() => void pull(), 5000);
    // Settings "Show in overlay" dispatches this so the bar updates without
    // waiting for the poll interval.
    const onRefresh = () => {
      void pull();
    };
    window.addEventListener("ai-usage-refresh", onRefresh);
    return () => {
      clearInterval(id);
      window.removeEventListener("ai-usage-refresh", onRefresh);
    };
  }, []);

  // Cross-webview: standalone Settings cannot dispatch DOM events into the
  // overlay. Backend emits this after set_provider_hidden mutates snapshots.
  // Embedded Settings also dispatches `ai-usage-refresh` (same-webview fallback).
  useEffect(() => {
    let cancelled = false;
    let unlistenVisibility: (() => void) | undefined;
    void listen("provider-visibility-changed", () => {
      void pull();
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenVisibility = fn;
      })
      .catch((e) => console.error("listen provider-visibility-changed failed", e));
    return () => {
      cancelled = true;
      unlistenVisibility?.();
    };
  }, []);

  // Keep countdown + stale threshold aligned with Settings → refresh interval.
  useEffect(() => {
    let cancelled = false;
    let unlistenInterval: (() => void) | undefined;
    void getRefreshInterval()
      .then((secs) => {
        // First load: Settings and countdown cycle start aligned.
        if (!cancelled) applySettingsInterval(secs, { adoptForCountdown: true });
      })
      .catch((e) => console.error("get_refresh_interval failed", e));
    void listen("refresh-interval-changed", (event) => {
      // Mid-cycle Settings change: keep ring on the current cycle interval.
      applySettingsInterval(event.payload);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenInterval = fn;
      })
      .catch((e) => console.error("listen refresh-interval-changed failed", e));
    // Same-webview fallback when Settings is embedded in the overlay (Tauri
    // emit is still used for the standalone settings window).
    const onLocalInterval = (ev: Event) => {
      const detail = (ev as CustomEvent).detail;
      applySettingsInterval(detail);
    };
    window.addEventListener("ai-usage-refresh-interval", onLocalInterval);
    return () => {
      cancelled = true;
      unlistenInterval?.();
      window.removeEventListener("ai-usage-refresh-interval", onLocalInterval);
    };
  }, []);

  // Scheduled auto-refresh: when the open popup's provider is fetched, show
  // the same progress / success / error strip as the manual refresh button.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    type AutoRefreshEvent = {
      provider: string;
      phase: string;
      ok?: boolean | null;
      message?: string | null;
      health?: string | null;
      attempt?: number | null;
      retry_in_secs?: number | null;
    };
    void listen<AutoRefreshEvent>("provider-refresh", (event) => {
      const payload = event.payload;
      if (!payload || typeof payload.provider !== "string") return;
      const openLabel = activeLabelRef.current;
      if (!openLabel) return;
      const openId = providerIdFor(openLabel);
      if (!openId || openId !== payload.provider) return;

      // Step 1/3 — busy check / wait countdown / retry (stay visible while waiting).
      if (payload.phase === "waiting") {
        clearProviderRefreshTimer();
        const raw =
          (payload.message && String(payload.message).trim()) ||
          "Provider busy — waiting, then retry…";
        const attempt =
          typeof payload.attempt === "number" ? payload.attempt : undefined;
        const retryInSecs =
          typeof payload.retry_in_secs === "number"
            ? payload.retry_in_secs
            : undefined;
        setProviderRefresh({
          providerLabel: openLabel,
          phase: "waiting",
          message: raw.startsWith("1/3") ? raw : `1/3 ${raw}`,
          attempt,
          retryInSecs,
        });
        return;
      }

      // Step 2/3 — live provider fetch.
      if (payload.phase === "started") {
        clearProviderRefreshTimer();
        const raw =
          (payload.message && String(payload.message).trim()) ||
          "Fetching latest usage…";
        setProviderRefresh({
          providerLabel: openLabel,
          phase: "fetching",
          message: raw.startsWith("2/3") ? raw : `2/3 ${raw}`,
        });
        return;
      }

      // Manual refresh applies finished state from the invoke result.
      if (manualRefreshInFlightRef.current) return;

      // Step 3/3 — apply snapshot into the UI.
      if (payload.phase === "finished") {
        const ok = payload.ok === true;
        const message =
          (payload.message && String(payload.message).trim()) ||
          (ok ? "Updated" : "Refresh failed");
        // Keep the indeterminate bar visible while we re-pull snapshots.
        setProviderRefresh({
          providerLabel: openLabel,
          phase: "applying",
          message: ok ? "3/3 Updating card…" : "3/3 Applying result…",
        });
        void (async () => {
          // Minimum paint time so a fast auto-refresh still shows the bar.
          const painted = new Promise<void>((r) => window.setTimeout(r, 280));
          try {
            await Promise.all([pull(), painted]);
          } catch (e) {
            console.error("pull after auto-refresh failed", e);
            await painted;
          }
          if (cancelled) return;
          if (ok) {
            finishProviderRefresh({
              providerLabel: openLabel,
              phase: "done",
              message,
            });
          } else {
            finishProviderRefresh(
              {
                providerLabel: openLabel,
                phase: "error",
                message,
              },
              3200,
            );
          }
        })();
      }
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("listen provider-refresh failed", e));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Flash a provider's bar segment green for ~3s when the backend reports one
  // of its quota windows reset (a fresh period started).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const timers = new Set<number>();
    void listen<{ provider: string; windows: string[] }>(
      "quota-window-reset",
      (event) => {
        const payload = event.payload;
        if (!payload || typeof payload.provider !== "string") return;
        const provider = payload.provider;
        setResetFlash((prev) => new Set(prev).add(provider));
        const timer = window.setTimeout(() => {
          timers.delete(timer);
          setResetFlash((prev) => {
            if (!prev.has(provider)) return prev;
            const next = new Set(prev);
            next.delete(provider);
            return next;
          });
        }, 3200);
        timers.add(timer);
      },
    )
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error("listen quota-window-reset failed", e));
    return () => {
      cancelled = true;
      unlisten?.();
      for (const timer of timers) window.clearTimeout(timer);
      timers.clear();
    };
  }, []);

  // Drive the popup countdown ring off the live clock and the *cycle* interval.
  // 250ms keeps the arc visibly moving even on longer intervals (2m / 5m).
  useEffect(() => {
    setNowMs(Date.now());
    const id = window.setInterval(() => setNowMs(Date.now()), 250);
    return () => window.clearInterval(id);
  }, [cycleIntervalSecs]);

  // Stale hide uses the longer of Settings vs current cycle so mid-cycle
  // interval changes cannot blank cards early.
  const staleThreshold = useMemo(
    () =>
      staleThresholdMs(
        Math.max(settingsIntervalSecs, cycleIntervalSecs),
      ),
    [settingsIntervalSecs, cycleIntervalSecs],
  );

  /** Keep the transparent native reserve aligned to the visible provider set.
   *  Provider additions grow it; removals shrink it so a short minibar can
   *  still be dragged all the way to a screen edge. Must complete before the
   *  CSS bar width is applied — otherwise `.bar { overflow: hidden }` clips
   *  newly enabled trackers while the HWND is still at the previous size. */
  async function ensureNativeReserve(pass: number): Promise<number> {
    const el = barRef.current;
    const fallback =
      nativeReserveWidthRef.current ?? OVERLAY_PREALLOCATED_W_LOGICAL;
    if (!el) return fallback;
    const measured = compactBarWidth(el);
    // Expanded floor so Settings never needs a second native width jump.
    const reserve = overlayWindowWidth(measured, true);
    if (nativeReserveWidthRef.current === reserve) return reserve;
    try {
      await invoke<void>("set_overlay_width", { width: reserve });
    } catch (e) {
      console.error("set_overlay_width failed", e);
    }
    // Drop a stale pass so an older short measurement cannot shrink the HWND
    // after a newer multi-tracker pass already grew it.
    if (pass !== widthPassRef.current) return nativeReserveWidthRef.current ?? reserve;
    nativeReserveWidthRef.current = reserve;
    return reserve;
  }

  /** Single sizing pass. Concurrent callers coalesce via applyBarWidth(). */
  async function applyBarWidthOnce(pass: number) {
    const el = barRef.current;
    if (!el) return;
    // Grow/shrink the HWND first so content never paints past the client area.
    await ensureNativeReserve(pass);
    if (pass !== widthPassRef.current) return;

    const measuredBarW = compactBarWidth(el);
    collapsedBarWidthRef.current = measuredBarW;
    const expanded =
      activeLabelRef.current !== null || settingsOpenRef.current;
    const w = overlayWindowWidth(measuredBarW, expanded);
    // The native reserve stays wide, so `width: 100%` would leave Settings at
    // 420px after the live minibar shrinks. Keep the card on the same target
    // width as the bar (its own max-width still caps wider multi-provider bars).
    widgetRef.current?.style.setProperty("--expanded-content-width", `${w}px`);
    // Ignore tiny jitter (sub-pixel / font settle) so we don't thrash SetWindowPos.
    if (
      lastBarWidthRef.current != null &&
      Math.abs(lastBarWidthRef.current - w) <= 1
    ) {
      return;
    }

    const startW = el.getBoundingClientRect().width;
    lastBarWidthRef.current = w;
    const token = ++widthResizeTokenRef.current;
    widthAnimationRef.current?.cancel();
    widthAnimationRef.current = null;

    // Startup happens while the HWND is hidden; percentage-label jitter is
    // too small to animate. Retain an explicit width so the bar never stretches
    // across the transparent native reserve.
    if (!shownRef.current || Math.abs(w - startW) <= 3) {
      el.style.width = `${w}px`;
      await setHitWidth(w);
      return;
    }

    // align-self:flex-end keeps the gear at the same screen x throughout.
    // Grow the hit region up front, but retain its old width during a shrink so
    // the simultaneously animating Settings card cannot be clipped from the left.
    el.style.width = `${startW}px`;
    if (expanded && w > startW) await setHitWidth(w);
    if (pass !== widthPassRef.current || token !== widthResizeTokenRef.current) {
      return;
    }

    const animation = el.animate(
      [{ width: `${startW}px` }, { width: `${w}px` }],
      {
        duration: WIDTH_ANIM_MS,
        easing: "cubic-bezier(0.2, 0.8, 0.2, 1)",
        fill: "forwards",
      },
    );
    widthAnimationRef.current = animation;
    try {
      await animation.finished;
    } catch {
      return;
    }
    if (pass !== widthPassRef.current || token !== widthResizeTokenRef.current) {
      return;
    }

    animation.cancel();
    widthAnimationRef.current = null;
    el.style.width = `${w}px`;
    await setHitWidth(w);
  }

  /** Animate only the right-aligned visible minibar. The native HWND remains
   *  at its stable reserve width, so popup clicks cannot generate resize focus
   *  loss and routine snapshot updates cannot move the desktop overlay.
   *  Concurrent calls coalesce: only the latest layout is applied, so a short
   *  first-paint pass cannot overwrite a later multi-tracker measurement. */
  async function applyBarWidth() {
    widthPassRef.current += 1;
    if (widthApplyInFlightRef.current) {
      widthApplyQueuedRef.current = true;
      return;
    }
    widthApplyInFlightRef.current = true;
    try {
      do {
        widthApplyQueuedRef.current = false;
        const pass = widthPassRef.current;
        await applyBarWidthOnce(pass);
      } while (widthApplyQueuedRef.current);
    } finally {
      widthApplyInFlightRef.current = false;
    }
  }

  function showOverlay() {
    void currentWindow.show()
      .then(() => invoke("enforce_overlay_borderless"))
      .catch((e) => console.error("show overlay failed", e));
  }

  // Pre-size the window BEFORE first show (visible:false in overlay.rs).
  // Floor is SETTINGS_MIN_POPUP_H so the first Settings open never needs
  // SetWindowPos on a visible transparent HWND (that flash is the glitch).
  // Provider popups are shorter; measure layer may raise the floor further.
  // After show we only expand the hit-strip region — never shrink/grow the
  // outer window for open/close.
  //
  // Wait for the first get_usage attempt (`usageReady`) so first paint is not
  // measured against the empty chrome bar, then locked short while trackers
  // load. Re-measure when the *visible* provider set changes (see
  // providerSignature) — rehydrated snapshots often start stale and only
  // paint segments after the first live fetch.
  useEffect(() => {
    if (shownRef.current) return;
    if (!usageReady) return;

    const raf = requestAnimationFrame(() => {
      let max = SETTINGS_MIN_POPUP_H;
      for (const s of snaps) {
        const el = measureRefs.current[s.provider];
        if (el) max = Math.max(max, Math.ceil(el.getBoundingClientRect().height));
      }
      maxHeightRef.current = Math.max(maxHeightRef.current, max);
      const target = maxHeightRef.current;

      // Already visible: keep tracker warm; never SetWindowPos (would flash).
      if (shownRef.current) return;

      const finishShow = () => {
        void (async () => {
          lastBarWidthRef.current = null;
          // Await native width first so a full multi-provider bar is not
          // clipped to the preallocated (or previous) client width on show.
          await applyBarWidth();
          await setHitHeight(barHeight());
          // Second pass after layout/fonts; logos also re-trigger via onload.
          requestAnimationFrame(() => {
            lastBarWidthRef.current = null;
            void applyBarWidth();
          });
          if (!shownRef.current) {
            shownRef.current = true;
            showOverlay();
          }
        })();
      };
      // Only grow the native window. A visible shrink here is the white-frame
      // glitch this overlay is designed to avoid.
      if (target > (appliedHeightRef.current ?? 0)) {
        appliedHeightRef.current = target;
        void invoke("set_overlay_geometry", {
          expanded: true,
          popupHeight: target,
          barHeight: barHeight(),
        })
          .catch((e) => console.error("pre-size set_overlay_geometry failed", e))
          .finally(finishShow);
      } else {
        finishShow();
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [snaps, usageReady]);

  // Safety net: if get_usage hangs, still reveal the bar so the overlay
  // isn't stuck invisible. Size from whatever is currently painted.
  useEffect(() => {
    const id = setTimeout(() => {
      if (shownRef.current) return;
      void (async () => {
        lastBarWidthRef.current = null;
        await applyBarWidth();
        await setHitHeight(barHeight());
        if (shownRef.current) return;
        shownRef.current = true;
        showOverlay();
      })();
    }, 1500);
    return () => clearTimeout(id);
  }, []);

  /** Grow outer window only when content exceeds the current pre-size.
   *  Returns the popup height (logical) to use for the hit strip. */
  async function ensureWindowTallEnough(
    measuredPopupH: number,
  ): Promise<number> {
    const target = Math.max(maxHeightRef.current, measuredPopupH);
    maxHeightRef.current = target;
    if (
      appliedHeightRef.current != null &&
      appliedHeightRef.current >= target
    ) {
      return target;
    }
    appliedHeightRef.current = target;
    try {
      await invoke("set_overlay_geometry", {
        expanded: true,
        popupHeight: target,
        barHeight: barHeight(),
      });
    } catch (e) {
      console.error("set_overlay_geometry failed", e);
    }
    return target;
  }

  function contentStripH(popupH: number): number {
    return barHeight() + POPUP_GAP_LOGICAL + popupH;
  }

  function waitFrames(n: number): Promise<void> {
    return new Promise((resolve) => {
      const step = (left: number) => {
        if (left <= 0) {
          resolve();
          return;
        }
        requestAnimationFrame(() => step(left - 1));
      };
      step(n);
    });
  }

  /**
   * Open sequence (paint → unclip). Expanding the HWND region *before* the
   * popup has painted is what produced the hollow frame every time.
   *
   *  1. Keep hit-strip at the bar (popup is region-clipped / invisible).
   *  2. Mount + paint popup at full opacity into the tall client buffer.
   *  3. Expand hit-strip → already-painted pixels appear, no empty shell.
   */
  useEffect(() => {
    const isExpanded = activeLabel !== null || settingsOpen;
    if (!isExpanded) {
      popupReadyRef.current = false;
      setPopupReady(false);
      setHasExpanded(false);
      void setHitHeight(barHeight());
      return;
    }

    const switchingProviders = !settingsOpen && hasExpanded && !!activeLabel;
    if (!switchingProviders) {
      // Stay clipped to the bar until paint finishes.
      popupReadyRef.current = false;
      setPopupReady(false);
      void setHitHeight(barHeight());
    }

    const token = ++resizeTokenRef.current;
    let cancelled = false;

    const openPaintThenUnclip = async (kind: "settings" | "provider") => {
      // Bar-only region while we measure/paint.
      if (!switchingProviders) {
        await setHitHeight(barHeight());
      }
      if (cancelled || resizeTokenRef.current !== token) return;

      // Let the panel mount and lay out (still clipped).
      await waitFrames(2);
      if (cancelled || resizeTokenRef.current !== token) return;

      const el =
        kind === "settings" ? settingsPopupRef.current : popupRef.current;
      let measured = el ? Math.ceil(el.getBoundingClientRect().height) : 0;
      if (kind === "settings") {
        measured ||= SETTINGS_PANEL_FALLBACK_HEIGHT;
        // Async status rows: give SettingsPanel a beat to fill in.
        await new Promise((r) => setTimeout(r, 40));
        if (cancelled || resizeTokenRef.current !== token) return;
        if (el) {
          measured = Math.max(
            measured,
            Math.ceil(el.getBoundingClientRect().height),
          );
        }
      } else if (measured < 40) {
        // Provider popup not ready yet — try one more frame.
        await waitFrames(2);
        if (cancelled || resizeTokenRef.current !== token) return;
        measured = el ? Math.ceil(el.getBoundingClientRect().height) : 0;
      }

      let visiblePopupH = measured || SETTINGS_MIN_POPUP_H;
      const popupH = await ensureWindowTallEnough(visiblePopupH);
      if (cancelled || resizeTokenRef.current !== token) return;

      // Paint at full opacity while still region-clipped to the bar.
      setHasExpanded(true);
      setPopupReady(true);

      // Allow WebView to composite the full client area (including the
      // off-region part above the bar) before we unclip.
      await waitFrames(2);
      if (cancelled || resizeTokenRef.current !== token) return;

      // Re-measure after paint (fonts/async rows).
      if (el) {
        const h2 = Math.ceil(el.getBoundingClientRect().height);
        visiblePopupH = h2 || visiblePopupH;
        if (h2 > popupH) {
          await ensureWindowTallEnough(h2);
        }
      }
      if (cancelled || resizeTokenRef.current !== token) return;

      // Unclip last — reveals already-painted content. No hollow frame.
      await setHitHeight(contentStripH(visiblePopupH));
      popupReadyRef.current = true;
      await resolvePendingOutsideClose();
    };

    if (settingsOpen) {
      maxHeightRef.current = Math.max(
        maxHeightRef.current,
        SETTINGS_MIN_POPUP_H,
      );
      void openPaintThenUnclip("settings");
      return () => {
        cancelled = true;
      };
    }

    // Provider popup
    void openPaintThenUnclip("provider");
    return () => {
      cancelled = true;
    };
  }, [activeLabel, settingsOpen]);

  // After open, if Settings grows (async OAuth / status), grow hit strip only.
  useEffect(() => {
    if (!popupReady) return;
    const el = settingsOpen ? settingsPopupRef.current : popupRef.current;
    if (!el) return;

    let debounce: ReturnType<typeof setTimeout> | undefined;
    const onResize = () => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        const h = Math.ceil(el.getBoundingClientRect().height);
        if (h <= 0) return;
        maxHeightRef.current = Math.max(maxHeightRef.current, h);
        void (async () => {
          await ensureWindowTallEnough(h);
          await setHitHeight(contentStripH(h));
        })();
      }, 48);
    };

    const ro =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(onResize)
        : null;
    ro?.observe(el);
    // The monitor-derived cap can settle before this observer is attached.
    // Sync the hit strip once immediately so the panel is never clipped to
    // the earlier fallback height.
    onResize();
    return () => {
      clearTimeout(debounce);
      ro?.disconnect();
    };
  }, [popupReady, settingsOpen, activeLabel, showIntro]);

  // Size the OS window to exactly fit the bar content (drag handle + cards +
  // refresh + settings). Grows/shrinks leftward as providers are added or
  // removed so the gear is never clipped and the bar stays tight. Native
  // width is awaited inside applyBarWidth so a sixth+ tracker never paints
  // into a too-narrow HWND.
  const lastBarWidthRef = useRef<number | null>(null);
  const collapsedBarWidthRef = useRef<number | null>(null);
  const widthAnimationRef = useRef<Animation | null>(null);
  const widthResizeTokenRef = useRef(0);
  const nativeReserveWidthRef = useRef<number | null>(null);
  /** Monotonic pass id so an older applyBarWidthOnce cannot clobber a newer one. */
  const widthPassRef = useRef(0);
  const widthApplyInFlightRef = useRef(false);
  const widthApplyQueuedRef = useRef(false);
  // Key off *bar-visible* trackers (same filter as <Bar>), not raw snap ids.
  // Rehydrated state.json is often older than the stale threshold, so first
  // paint has zero segments; when live fetches land the same providers become
  // visible and this signature must change so we remeasure and expand.
  const providerSignature = snaps
    .filter(
      (snap) =>
        !isStaleSnapshot(snap, staleThreshold) && hasDisplayableWindows(snap),
    )
    .map((snap) => snap.provider)
    .join("\u001f");
  useEffect(() => {
    const barEl = barRef.current;
    if (!barEl) return;

    // Double-rAF so flex layout + fonts have settled before measuring.
    let cancelled = false;
    let raf2 = 0;
    let lateTimer: ReturnType<typeof setTimeout> | undefined;
    const raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        if (cancelled) return;
        lastBarWidthRef.current = null;
        void applyBarWidth();
        // Logos (PNG) can decode after first paint and nudge text width;
        // re-measure once more so first-startup doesn't leave the gear clipped.
        lateTimer = setTimeout(() => {
          if (cancelled) return;
          lastBarWidthRef.current = null;
          void applyBarWidth();
        }, 120);
      });
    });

    // When provider logos finish loading, re-fit (decode can change metrics).
    const imgs = Array.from(barEl.querySelectorAll("img"));
    const onImg = () => {
      lastBarWidthRef.current = null;
      void applyBarWidth();
    };
    for (const img of imgs) {
      if (!img.complete) img.addEventListener("load", onImg, { once: true });
    }

    return () => {
      cancelled = true;
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      clearTimeout(lateTimer);
      for (const img of imgs) img.removeEventListener("load", onImg);
    };
  }, [providerSignature, activeLabel, settingsOpen]);

  // Keep the bar out from under the taskbar: after a native drag settles, ask
  // Rust to snap the window back inside the monitor work area, then persist the
  // (now-clamped) position so the next open restores it. Debounced so we only
  // act once the drag ends, never mid-drag. Position normalization to the
  // preallocated window footprint happens in Rust (see `persist_overlay_position`).
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unlisten = currentWindow.onMoved(() => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        try {
          await invoke("clamp_overlay_position");
        } catch (e) {
          console.error("clamp_overlay_position failed", e);
          return;
        }
        try {
          await saveOverlayPosition();
        } catch (e) {
          console.error("save_overlay_position failed", e);
        }
      }, 150);
    });
    return () => {
      void unlisten.then((fn) => fn());
      clearTimeout(timer);
    };
  }, []);

  // Escape closes whichever popup is open. Both popups use a closing-flag +
  // setTimeout so the slide-down animation can finish before the element
  // is unmounted.
  useEffect(() => {
    if (!activeLabel && !settingsOpen) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (settingsOpen) {
        requestSettingsClose();
      } else if (activeLabel) {
        closeProvider();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [activeLabel, settingsOpen]);

  // Clicking outside the visible content closes whichever popup is open. A
  // click in the preallocated transparent reserve stays inside this webview,
  // so it cannot produce Tauri focus loss; the document listener handles that
  // case. A normal app uses the Tauri focus signal, while the Windows taskbar
  // uses the native foreground bridge.
  //
  // All three signals are registered ONCE on mount (not per open/close), and
  // all route through the same ref-backed close helper. We deliberately avoid
  // a DOM `blur` listener: it races the Tauri focus signal on external clicks.
  // Close is synchronous (no slide-out / setTimeout) so there is no timer to
  // stall on the just-unfocused window; the collapse effect re-clips the
  // native region to the bar once the popup is gone.
  function closePopupsImmediately() {
    const al = activeLabelRef.current;
    const so = settingsOpenRef.current;
    if (!al && !so) {
      pendingOutsideCloseRef.current = false;
      return;
    }
    // Keep the only early taskbar/foreground transition instead of losing it
    // while the popup is still painting behind the bar-only region.
    if (!popupReadyRef.current) {
      pendingOutsideCloseRef.current = true;
      return;
    }
    // A single click can produce both a document pointer event and one or
    // more Focused(false) events. Close exactly once per open cycle.
    if (outsideCloseFiredRef.current) return;
    outsideCloseFiredRef.current = true;
    // Cancel paint/unclip continuations and delayed Settings/provider swaps so
    // none can reopen content after this outside close.
    resizeTokenRef.current += 1;
    pendingOutsideCloseRef.current = false;
    popupReadyRef.current = false;
    activeLabelRef.current = null;
    settingsOpenRef.current = false;
    // Clip before state updates so a full-height Settings hit region cannot
    // remain interactive while the popup is being removed.
    void setHitHeight(barHeight());
    if (al) {
      setActiveLabel(null);
      setProviderClosing(false);
    }
    if (so) {
      cancelSettingsCloseTimer();
      settingsClosePhaseRef.current = "closed";
      setSettingsClosing(false);
      setSettingsOpen(false);
    }
  }

  async function resolvePendingOutsideClose() {
    if (!pendingOutsideCloseRef.current) return;

    // A region/frame transition can emit a transient focus loss. Only close
    // after preparation if focus did not return; a query failure fails closed
    // so the full-height overlay cannot remain stuck and interactive.
    let focused = false;
    try {
      focused = await currentWindow.isFocused();
    } catch (e) {
      console.error("overlay focus check failed", e);
    }

    if (focused) {
      pendingOutsideCloseRef.current = false;
      return;
    }
    closePopupsImmediately();
  }

  useEffect(() => {
    const onDocumentPointerDown = (event: PointerEvent) => {
      // The visible bar and popup are the only interactive part of the
      // overlay. Empty space in the preallocated window is an outside click.
      if (widgetRef.current?.contains(event.target as Node)) return;
      closePopupsImmediately();
    };
    document.addEventListener("pointerdown", onDocumentPointerDown, true);

    const onTauriFocus = ({ payload: focused }: { payload: boolean }) => {
      if (focused) return;
      closePopupsImmediately();
    };
    const unlistenPromise = currentWindow.onFocusChanged(onTauriFocus);
    const onNativeFocusLost = () => {
      closePopupsImmediately();
    };
    window.addEventListener("overlay-focus-lost", onNativeFocusLost);
    return () => {
      document.removeEventListener("pointerdown", onDocumentPointerDown, true);
      void unlistenPromise.then((fn) => fn());
      window.removeEventListener("overlay-focus-lost", onNativeFocusLost);
    };
  }, []);

  // Re-arm the once-per-cycle outside-close guard whenever a popup is open.
  useEffect(() => {
    if (activeLabel || settingsOpen) outsideCloseFiredRef.current = false;
  }, [activeLabel, settingsOpen]);

// Click-through is now driven by a native cursor-position poll in Rust (see
  // win32::ensure_click_through_poll): the overlay passes mouse events through
  // except while the cursor is actually over the bar/popup. The old
  // frontend mouseenter/mouseleave toggle was unreliable on a click-through
  // window and could leave the whole transparent surface swallowing events.

  // If the active provider disappears (e.g. key deleted), collapse.
  useEffect(() => {
    if (activeLabel && !snaps.some((s) => s.provider === activeLabel)) {
      setActiveLabel(null);
    }
  }, [snaps, activeLabel]);

  const activeSnap = !settingsOpen ? snaps.find((s) => s.provider === activeLabel) || null : null;

  /** Cancel the in-flight close animation timer (if any) and clear the ref.
   *  Called when Settings is being opened so a stale timer cannot unmount
   *  a freshly mounted popup. */
  function cancelSettingsCloseTimer() {
    if (settingsCloseTimerRef.current !== null) {
      window.clearTimeout(settingsCloseTimerRef.current);
      settingsCloseTimerRef.current = null;
    }
  }

  /** Open the embedded Settings popup. Used by the gear click and when
   *  swapping away from a provider popup. Resets the lifecycle reducer so a
   *  duplicate outside-click that arrives just after the swap cannot start
   *  a phantom close. */
  function openSettingsPopup() {
    cancelSettingsCloseTimer();
    settingsClosePhaseRef.current = reduceSettingsClose(
      settingsClosePhaseRef.current,
      "opened",
    ).phase;
    beginOutsideCloseCycle(null, true);
    setPopupReady(false);
    setProviderClosing(false);
    setActiveLabel(null);
    setSettingsClosing(false);
    // Fresh mount each open: clears leftover green/red status-line banners
    // (especially if Settings was reopened mid close-animation without unmounting).
    setSettingsInstance((n) => n + 1);
    setSettingsOpen(true);
  }

  /** One-shot close coordinator. The reducer ref decides whether the first
   *  notification should start the slide-out animation; subsequent focus-loss
   *  notifications for the same outside-click are no-ops. Clip the native
   *  region to the bar before animating so the preallocated transparent window
   *  cannot flash while its popup fades out. Do not resize or refocus here.
   *  (Outside-click / focus-loss close is handled directly in the mount-time
   *  focus listener above, which unmounts synchronously without animating.) */
  function requestSettingsClose() {
    const started = reduceSettingsClose(
      settingsClosePhaseRef.current,
      "close-requested",
    );
    settingsClosePhaseRef.current = started.phase;
    if (!started.startAnimation) return;

    void setHitHeight(barHeight());
    setSettingsClosing(true);
    cancelSettingsCloseTimer();
    settingsCloseTimerRef.current = window.setTimeout(() => {
      settingsCloseTimerRef.current = null;
      const finished = reduceSettingsClose(
        settingsClosePhaseRef.current,
        "animation-finished",
      );
      settingsClosePhaseRef.current = finished.phase;
      if (!finished.unmount) return;
      setSettingsOpen(false);
      setSettingsClosing(false);
    }, SETTINGS_CLOSE_MS);
  }

  function onSettingsToggle() {
    if (settingsOpen) {
      requestSettingsClose();
      return;
    }
    openSettingsPopup();
  }

  function closeProvider() {
    if (!activeLabel) return;
    void setHitHeight(barHeight());
    setProviderClosing(true);
    window.setTimeout(() => {
      setActiveLabel(null);
      setProviderClosing(false);
    }, 180);
  }

  // Segment click handler. When the user clicks the active segment to toggle
  // it off, route through closeProvider() so the slide-out animation plays.
  // Clicking a different segment crossfades: dim + slide the current content,
  // swap, then restore — no full close/open cycle.
  //
  // If settings is open, a tracker click should close settings and open the
  // clicked tracker's popup — chained through the 180ms slide-out animation.
  function handlePick(label: string | null) {
    if (label === null) {
      closeProvider();
      return;
    }
    if (settingsOpen) {
      void setHitHeight(barHeight());
      setSettingsClosing(true);
      const settingsSwapToken = ++resizeTokenRef.current;
      window.setTimeout(() => {
        if (resizeTokenRef.current !== settingsSwapToken) return;
        beginOutsideCloseCycle(label, false);
        setSettingsOpen(false);
        setSettingsClosing(false);
        setActiveLabel(label);
        setProviderClosing(false);
      }, 180);
      return;
    }
    if (activeLabel && activeLabel !== label) {
      // Switching providers — brief crossfade via CSS transition.
      beginOutsideCloseCycle(label, false);
      setProviderSwitching(true);
      const providerSwitchToken = ++resizeTokenRef.current;
      window.setTimeout(() => {
        if (resizeTokenRef.current !== providerSwitchToken) return;
        setActiveLabel(label);
        setProviderClosing(false);
        setProviderSwitching(false);
      }, 150);
    } else {
      beginOutsideCloseCycle(label, false);
      setActiveLabel(label);
      setProviderClosing(false);
    }
  }

  return (
    <div class="widget">
      {/* Hidden measurement layer: renders every provider popup once so the
          window can be pre-sized to the tallest before the user opens any.
          Settings is measured on first open (grow-only) to avoid a second
          always-mounted SettingsPanel fighting the checkbox state. */}
      <div class="measure-layer" aria-hidden="true">
        {snaps.map((s) => (
          <Popup
            key={s.provider}
            snap={s}
            popupRef={(el) => {
              measureRefs.current[s.provider] = el;
            }}
            hidden
            closing={false}
            switching={false}
            showIntro={false}
            setShowIntro={() => {}}
            burnLookup={burnLookup}
          />
        ))}
      </div>
      {/* Real content. Bottom-anchored so the pre-sized transparent space
          above the bar stays empty; Rust's content-strip hit test keeps that
          region click-through. */}
      <div class="widget-content" ref={widgetRef}>
        {activeSnap && (
          <Popup
            snap={activeSnap}
            popupRef={popupRef}
            hidden={!popupReady}
            closing={providerClosing}
            switching={providerSwitching}
            showIntro={showIntro}
            setShowIntro={setShowIntro}
            onRefresh={() => void onProviderRefresh(activeSnap.provider)}
            refreshState={
              providerRefresh &&
              providerRefresh.providerLabel === activeSnap.provider
                ? providerRefresh
                : null
            }
            refreshIntervalSecs={cycleIntervalSecs}
            settingsIntervalSecs={settingsIntervalSecs}
            nowMs={nowMs}
            burnLookup={burnLookup}
          />
        )}
        {settingsOpen && (
          <SettingsPopup
            key={settingsInstance}
            popupRef={settingsPopupRef}
            hidden={!popupReady}
            closing={settingsClosing}
          />
        )}
        <Bar
          snaps={snaps}
          activeId={activeLabel}
          onPick={handlePick}
          onSettingsClick={onSettingsToggle}
          barRef={barRef}
          staleThreshold={staleThreshold}
          nowMs={nowMs}
          refreshIntervalSecs={cycleIntervalSecs}
          resetFlash={resetFlash}
        />
      </div>
    </div>
  );
}

render(<UpdateStateProvider><Overlay /></UpdateStateProvider>, document.getElementById("app")!);
