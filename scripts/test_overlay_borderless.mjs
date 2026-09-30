import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(
  new URL("../src-tauri/src/overlay.rs", import.meta.url),
  "utf8",
);
const commandsSource = readFileSync(
  new URL("../src-tauri/src/commands.rs", import.meta.url),
  "utf8",
);
const win32Source = readFileSync(
  new URL("../src-tauri/src/win32.rs", import.meta.url),
  "utf8",
);
const frontendSource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);

const existingWindowPath = source.slice(
  source.indexOf('if let Some(win) = app.get_webview_window(OVERLAY_LABEL)'),
  source.indexOf("// Preallocate the tallest normal popup"),
);
const newWindowPath = source.slice(
  source.indexOf("let _win = WebviewWindowBuilder::new"),
  source.indexOf("// Persist the bar's position"),
);

assert.equal(
  existingWindowPath.includes("crate::win32::enforce_borderless(&win)"),
  true,
  "re-shown overlay windows must enforce the borderless Win32 style",
);
// Restoring from the tray: fix the style while still hidden, then keep
// re-applying while WebView2 paints, or a native title bar stays visible
// until the next click.
const reshowEnforceIndex = existingWindowPath.indexOf("crate::win32::enforce_borderless(&win)");
const reshowShowIndex = existingWindowPath.indexOf("win.show()?;");
const reshowSettledIndex = existingWindowPath.indexOf(
  "crate::win32::enforce_borderless_after_show(&win)",
);
assert.ok(
  reshowEnforceIndex !== -1 && reshowEnforceIndex < reshowShowIndex,
  "re-shown overlay must strip the caption style before show()",
);
assert.ok(
  reshowSettledIndex > reshowShowIndex,
  "re-shown overlay must schedule delayed borderless refreshes after show()",
);
assert.equal(
  newWindowPath.includes("crate::win32::enforce_borderless(&_win)"),
  true,
  "new overlay windows must enforce the borderless Win32 style after build",
);
assert.equal(
  commandsSource.includes("pub async fn enforce_overlay_borderless"),
  true,
  "frontend must have a native command that enforces the style after show",
);
assert.equal(
  frontendSource.includes('invoke("enforce_overlay_borderless")'),
  true,
  "the first visible overlay frame must enforce its borderless native style",
);

const rectStart = win32Source.indexOf("pub fn set_window_rect(");
const rectEnd = win32Source.indexOf("/// Convenience: look up a window", rectStart);
assert.notEqual(rectStart, -1, "native rectangle helper must exist");
assert.notEqual(rectEnd, -1, "native rectangle helper must have a bounded section");
const rectSource = win32Source.slice(rectStart, rectEnd);
const rectEnforceIndex = rectSource.indexOf("enforce_borderless(window)?;");
const rectMoveIndex = rectSource.indexOf("SetWindowPos(hwnd");
assert.notEqual(
  rectEnforceIndex,
  -1,
  "native geometry changes must enforce borderless style before moving the window",
);
assert.ok(
  rectEnforceIndex < rectMoveIndex,
  "borderless enforcement must precede SetWindowPos",
);

const regionStart = win32Source.indexOf("fn apply_content_region");
const regionEnd = win32Source.indexOf("/// Store the content-strip height", regionStart);
assert.notEqual(regionStart, -1, "content-region helper must exist");
assert.notEqual(regionEnd, -1, "content-region helper must have a bounded section");
const regionSource = win32Source.slice(regionStart, regionEnd);
const firstEnforceIndex = regionSource.indexOf("enforce_borderless(window)?;");
const lastReshapeIndex = regionSource.lastIndexOf("SetWindowRgn(hwnd");
const secondEnforceIndex = regionSource.indexOf(
  "enforce_borderless(window)",
  firstEnforceIndex + 1,
);
assert.notEqual(
  firstEnforceIndex,
  -1,
  "region mutation must strip stale frame styles first",
);
assert.ok(
  lastReshapeIndex !== -1,
  "region mutation must call SetWindowRgn",
);
assert.notEqual(
  secondEnforceIndex,
  -1,
  "region mutation must refresh the frame afterward",
);
assert.ok(
  firstEnforceIndex < lastReshapeIndex,
  "style enforcement must precede SetWindowRgn",
);
assert.ok(
  lastReshapeIndex < secondEnforceIndex,
  "frame refresh must follow SetWindowRgn",
);

const enforceStart = win32Source.indexOf("pub fn enforce_borderless(");
const enforceEnd = win32Source.indexOf("/// Atomically set a window", enforceStart);
assert.notEqual(enforceStart, -1, "borderless helper must exist");
assert.notEqual(enforceEnd, -1, "borderless helper must have a bounded section");
const enforceSource = win32Source.slice(enforceStart, enforceEnd);
const styleConditionalIndex = enforceSource.indexOf("if borderless != current {");
const styleWriteIndex = enforceSource.indexOf("SetWindowLongPtrW", styleConditionalIndex);
const styleConditionalEnd = enforceSource.indexOf("\n            }", styleWriteIndex);
const frameRefreshIndex = enforceSource.indexOf("SetWindowPos(", styleConditionalEnd);
assert.ok(
  styleConditionalIndex >= 0 &&
    styleWriteIndex > styleConditionalIndex &&
    styleConditionalEnd > styleWriteIndex &&
    frameRefreshIndex > styleConditionalEnd,
  "SWP_FRAMECHANGED must run even when the style bits were already borderless",
);

// WebView2 can repaint its cached non-client frame after SetWindowRgn and
// after the synchronous SWP_FRAMECHANGED returns. A coalesced delayed refresh
// must win that race without stacking stale refreshes from rapid region edits.
const delayedStart = win32Source.indexOf("fn schedule_borderless_refresh(");
const delayedEnd = win32Source.indexOf("/// Update the interactive content strip", delayedStart);
assert.notEqual(delayedStart, -1, "delayed borderless refresh helper must exist");
assert.notEqual(delayedEnd, -1, "delayed refresh helper must have a bounded section");
const delayedSource = win32Source.slice(delayedStart, delayedEnd);
assert.match(
  delayedSource,
  /BORDERLESS_REFRESH_EPOCH\s*\.fetch_add/,
  "delayed borderless refresh must advance its coalescing epoch",
);
for (const required of [
  "tauri::async_runtime::spawn",
  "Duration::from_millis(",
  "schedule_borderless_refresh_at(window, &[50])",
  "enforce_borderless(&window)",
]) {
  assert.notEqual(
    delayedSource.indexOf(required),
    -1,
    `delayed borderless refresh must contain ${required}`,
  );
}
const settledStart = win32Source.indexOf("pub fn enforce_borderless_after_show(");
assert.notEqual(settledStart, -1, "post-show borderless helper must exist");
const settledSource = win32Source.slice(settledStart, settledStart + 400);
assert.match(
  settledSource,
  /enforce_borderless\(window\)\?;[\s\S]*schedule_borderless_refresh_at\(window\.clone\(\), &\[50, 250, 700\]\)/,
  "post-show helper must enforce immediately and again after WebView2 paints",
);
assert.notEqual(
  regionSource.indexOf("schedule_borderless_refresh(window.clone())"),
  -1,
  "every region mutation must schedule the post-WebView2 frame refresh",
);

// After a tray restore Windows repainted a stale native title bar on every
// activation change (focus moved away: bar appears; overlay clicked: gone).
// The overlay has no frame by design, so a window-proc hook must stop
// WM_NCACTIVATE / WM_NCPAINT from painting one.
const hookStart = win32Source.indexOf("unsafe extern \"system\" fn no_frame_subclass_proc");
assert.notEqual(hookStart, -1, "no-frame window-proc hook must exist");
const hookSource = win32Source.slice(hookStart, hookStart + 900);
assert.match(
  hookSource,
  /WM_NCACTIVATE\s*=>\s*DefSubclassProc\(hwnd,\s*msg,\s*wparam,\s*LPARAM\(-1\)\)/,
  "WM_NCACTIVATE must be forwarded with lParam = -1 so the frame is not repainted",
);
assert.match(hookSource, /WM_NCPAINT\s*=>\s*LRESULT\(0\)/, "WM_NCPAINT must be swallowed");
assert.equal(
  win32Source.includes("pub fn suppress_frame_repaint("),
  true,
  "helper that installs the hook must exist",
);
assert.equal(
  source.includes("crate::win32::suppress_frame_repaint(&_win)"),
  true,
  "new overlay windows must install the no-frame hook",
);
assert.ok(
  existingWindowPath.includes("crate::win32::suppress_frame_repaint(&win)") &&
    existingWindowPath.indexOf("crate::win32::suppress_frame_repaint(&win)") <
      existingWindowPath.indexOf("win.show()?;"),
  "re-shown overlay windows must have the no-frame hook before show()",
);
assert.match(
  source,
  /WindowEvent::Focused\(_\)\s*=>\s*\{[\s\S]*?enforce_borderless_after_show/,
  "focus changes must re-apply borderless enforcement",
);

console.log("overlay borderless enforcement tests passed");
