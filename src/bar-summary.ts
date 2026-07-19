import type { UsageWindow } from "./types";
import {
  isFiveHourWindow,
  isWeeklyWindow,
  resolveEvenPace,
  type EvenPace,
} from "./weekly-pace.ts";

export function collapsedBarRemaining(windows: UsageWindow[]): number {
  const visible = windows.filter((window) => window.bar_visible);
  const primary =
    visible.find((window) => isFiveHourWindow(window.label)) ?? visible[0];
  if (!primary) return 100;

  return Math.max(0, Math.min(100, 100 - primary.used_percent));
}

function aggregateColorPercent(
  visible: UsageWindow[],
  providerLabel: string,
  nowMs: number,
): number {
  let aheadPercent = 0;
  let behindPercent = 100;
  for (const w of visible) {
    const remaining = Math.max(0, 100 - w.used_percent);
    if (!w.is_unlimited) {
      const cp =
        resolveEvenPace(w, providerLabel, nowMs)?.gradientPercent ?? remaining;
      if (cp > 100) {
        aheadPercent = Math.max(aheadPercent, cp);
      } else {
        behindPercent = Math.min(behindPercent, cp);
      }
    }
  }
  return aheadPercent > 0 ? aheadPercent : behindPercent;
}

export function collapsedBarColorPercent(
  windows: UsageWindow[],
  providerLabel: string,
  nowMs: number = Date.now(),
): number {
  const visible = windows.filter((w) => w.bar_visible);
  if (visible.length === 0) return 100;

  if (providerLabel.toLowerCase().includes("openrouter")) {
    return aggregateColorPercent(visible, providerLabel, nowMs);
  }

  // Mini bar fill color reflects only the primary (5h) window. The weekly
  // under-red-line warning is surfaced separately by the pulsing red ring
  // (isWeeklyUnderRedLine), so it must not recolor the bar fill here.
  const primary =
    visible.find((w) => isFiveHourWindow(w.label)) ?? visible[0];
  if (primary.is_unlimited) return 100;
  return (
    resolveEvenPace(primary, providerLabel, nowMs)?.gradientPercent ??
    Math.max(0, 100 - primary.used_percent)
  );
}

function windowUnderRedLinePace(
  window: UsageWindow,
  providerLabel: string,
  nowMs: number,
): EvenPace | null {
  if (providerLabel.toLowerCase().includes("openrouter")) return null;
  if (window.is_unlimited) return null;
  const pace = resolveEvenPace(window, providerLabel, nowMs);
  // ponytail: depleted (0% left) is always critical. Guard kept so a 0%-
  // remaining window near reset can never read as "on pace", regardless of
  // where the dynamic sub-target lands.
  if (pace && pace.remainingPercent <= 0) return pace;
  if (!pace || pace.remainingPercent >= pace.subTargetRemainingPercent) {
    return null;
  }
  return pace;
}

function weeklyWindowOverridePace(
  window: UsageWindow,
  providerLabel: string,
  nowMs: number,
): EvenPace | null {
  if (!isWeeklyWindow(window.label)) return null;
  return windowUnderRedLinePace(window, providerLabel, nowMs);
}

function weeklyOverridePace(
  visible: UsageWindow[],
  providerLabel: string,
  nowMs: number,
): EvenPace | null {
  const weekly = visible.find(
    (w) => weeklyWindowOverridePace(w, providerLabel, nowMs) !== null,
  );
  return weekly ? weeklyWindowOverridePace(weekly, providerLabel, nowMs) : null;
}

export function isWeeklyUnderRedLine(
  windows: UsageWindow[],
  providerLabel: string,
  nowMs: number = Date.now(),
): boolean {
  const visible = windows.filter((w) => w.bar_visible);
  return weeklyOverridePace(visible, providerLabel, nowMs) !== null;
}

export function isWeeklyWindowUnderRedLine(
  window: UsageWindow,
  providerLabel: string,
  nowMs: number = Date.now(),
): boolean {
  return weeklyWindowOverridePace(window, providerLabel, nowMs) !== null;
}

export function isFiveHourWindowUnderRedLine(
  window: UsageWindow,
  providerLabel: string,
  nowMs: number = Date.now(),
): boolean {
  if (!isFiveHourWindow(window.label)) return false;
  return windowUnderRedLinePace(window, providerLabel, nowMs) !== null;
}

/** True when any visible 5h window is under its red line — drives the pulsing
 *  red ring on the mini bar (alongside isWeeklyUnderRedLine). */
export function isFiveHourUnderRedLine(
  windows: UsageWindow[],
  providerLabel: string,
  nowMs: number = Date.now(),
): boolean {
  const visible = windows.filter((w) => w.bar_visible);
  return visible.some(
    (w) => isFiveHourWindowUnderRedLine(w, providerLabel, nowMs),
  );
}
