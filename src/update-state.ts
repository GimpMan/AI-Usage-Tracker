import { createContext, createElement, type ComponentChildren } from "preact";
import { useContext, useEffect, useState } from "preact/hooks";
import { listen } from "@tauri-apps/api/event";
import { getUpdateState } from "./api";
import type { UpdateState } from "./types";
import { shouldApplyInitialUpdateState } from "./update-state-logic";

const UpdateStateContext = createContext<UpdateState | null>(null);

export function UpdateStateProvider({ children }: { children: ComponentChildren }) {
  const [state, setState] = useState<UpdateState | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let eventVersion = 0;

    const loadInitialState = async () => {
      const versionAtRequest = eventVersion;
      try {
        const next = await getUpdateState();
        if (!disposed && shouldApplyInitialUpdateState(versionAtRequest, eventVersion)) {
          setState(next);
        }
      } catch (error) {
        console.error("Could not load update state", error);
      }
    };

    // Establish the event stream before requesting the snapshot. If an event
    // arrives while IPC is pending, its increment prevents stale overwrite.
    void listen<UpdateState>("update-state-changed", (event) => {
      eventVersion += 1;
      if (!disposed) setState(event.payload);
    }).then((stop) => {
      if (disposed) stop();
      else {
        unlisten = stop;
        void loadInitialState();
      }
    }).catch((error) => {
      console.error("Could not subscribe to update state", error);
      void loadInitialState();
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return createElement(UpdateStateContext.Provider, { value: state }, children);
}

export function useUpdateState(): UpdateState | null {
  return useContext(UpdateStateContext);
}
