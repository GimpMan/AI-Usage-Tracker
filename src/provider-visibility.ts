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
