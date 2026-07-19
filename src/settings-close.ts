export type SettingsClosePhase = "open" | "closing" | "closed";
export type SettingsCloseEvent =
  | "close-requested"
  | "animation-finished"
  | "opened";

export interface SettingsCloseEffect {
  phase: SettingsClosePhase;
  startAnimation: boolean;
  unmount: boolean;
}

export function reduceSettingsClose(
  phase: SettingsClosePhase,
  event: SettingsCloseEvent,
): SettingsCloseEffect {
  if (event === "opened") {
    return { phase: "open", startAnimation: false, unmount: false };
  }

  if (event === "close-requested") {
    if (phase !== "open") {
      return { phase, startAnimation: false, unmount: false };
    }
    return { phase: "closing", startAnimation: true, unmount: false };
  }

  if (phase !== "closing") {
    return { phase, startAnimation: false, unmount: false };
  }
  return { phase: "closed", startAnimation: false, unmount: true };
}
