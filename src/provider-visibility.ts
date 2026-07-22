import { isStaleSnapshot } from "./stale-snapshot.ts";
import type { UsageSnapshot } from "./types";

export interface VisibilityState {
  eligible: boolean;
  hidden: boolean;
}

export function checkboxChecked(state: VisibilityState): boolean {
  return state.eligible && !state.hidden;
}

export function checkboxDisabled(state: Pick<VisibilityState, "eligible">): boolean {
  return !state.eligible;
}

export function hasDisplayableWindows(snapshot: {
  windows?: Array<{ bar_visible?: boolean }>;
}): boolean {
  return (snapshot.windows ?? []).some((window) => window.bar_visible !== false);
}

/** Whether a provider snapshot gets a segment on the mini bar.
 *
 *  The stale-age hide exists for startup rehydrate only: state.json holds last
 *  session's values, which must not flash on the bar before the first live
 *  fetch. Snapshots stamped with an `unavailable_reason` are exempt — the
 *  backend never persists those (`persist` writes only successful snapshots),
 *  so a reason proves the snapshot is this session's last-good view of a
 *  provider whose fetch is currently failing. Hiding those made a selected
 *  provider silently vanish from the bar during any sustained outage; keeping
 *  them lets the bar render its "stale" badge (with the reason as tooltip)
 *  until the fetch recovers. */
export function barSegmentVisible(
  snap: Pick<UsageSnapshot, "fetched_at" | "unavailable_reason" | "windows">,
  thresholdMs: number,
): boolean {
  if (!hasDisplayableWindows(snap)) return false;
  if (snap.unavailable_reason) return true;
  return !isStaleSnapshot(snap, thresholdMs);
}

export function formatBalanceWindow(window: { label: string }): string {
  const prefix = "balance ";
  return window.label.toLowerCase().startsWith(prefix)
    ? window.label.slice(prefix.length)
    : window.label;
}

/** Strip a trailing " left" from OpenRouter dollar amounts (e.g. "$1.06 left" → "$1.06"). */
function stripTrailingLeft(amount: string): string {
  return amount.replace(/\s+left$/i, "");
}

/** Format OpenRouter's dollar-denominated windows for compact UI surfaces. */
export function formatDollarWindow(window: { label: string }): string | null {
  const label = window.label.toLowerCase();
  if (label.startsWith("balance ")) return formatBalanceWindow(window);
  if (label.startsWith("total ")) {
    return stripTrailingLeft(window.label.slice("total ".length));
  }
  if (label.startsWith("today ")) return window.label.slice("today ".length);
  if (label.startsWith("this week ")) return window.label.slice("this week ".length);
  if (label.startsWith("this month ")) return window.label.slice("this month ".length);
  return null;
}
