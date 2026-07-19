export const EXPANDED_PANEL_MIN_WIDTH = 320;
export const EXPANDED_PANEL_MAX_WIDTH = 420;

export function expandedPanelWidth(barWidth: number): number {
  return Math.min(
    EXPANDED_PANEL_MAX_WIDTH,
    Math.max(EXPANDED_PANEL_MIN_WIDTH, barWidth),
  );
}

export function overlayWindowWidth(
  barWidth: number,
  expanded: boolean,
): number {
  return expanded
    ? Math.max(barWidth, expandedPanelWidth(barWidth))
    : barWidth;
}

/** Keep flex-expanded bar content from feeding its laid-out width back into
 * the native window calculation. Collapsed measurements remain authoritative;
 * the live measurement is only a fallback before one has been captured. */
export function stableNaturalBarWidth(
  measuredWidth: number,
  expanded: boolean,
  collapsedWidth: number | null,
): number {
  return expanded && collapsedWidth !== null ? collapsedWidth : measuredWidth;
}
