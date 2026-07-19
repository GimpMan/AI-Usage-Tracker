export const SETTINGS_PANEL_FALLBACK_HEIGHT = 640;
const SETTINGS_PANEL_MIN_HEIGHT = 320;
const POPUP_GAP = 6;
const DESKTOP_MARGIN = 16;

export function settingsPanelMaxHeight(
  workAreaPhysicalHeight: number,
  scaleFactor: number,
  barHeight: number,
  reservedWindowPhysicalHeight = workAreaPhysicalHeight,
): number {
  if (
    !Number.isFinite(workAreaPhysicalHeight) ||
    workAreaPhysicalHeight <= 0 ||
    !Number.isFinite(scaleFactor) ||
    scaleFactor <= 0 ||
    !Number.isFinite(barHeight) ||
    barHeight < 0 ||
    !Number.isFinite(reservedWindowPhysicalHeight) ||
    reservedWindowPhysicalHeight <= 0
  ) {
    return SETTINGS_PANEL_FALLBACK_HEIGHT;
  }

  const availablePhysicalHeight = Math.min(
    workAreaPhysicalHeight,
    reservedWindowPhysicalHeight,
  );
  const available =
    availablePhysicalHeight / scaleFactor -
    barHeight -
    POPUP_GAP -
    DESKTOP_MARGIN;
  return Math.max(SETTINGS_PANEL_MIN_HEIGHT, Math.floor(available));
}
