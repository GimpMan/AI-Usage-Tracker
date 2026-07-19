import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const overlaySource = readFileSync(
  new URL("../src/overlay.tsx", import.meta.url),
  "utf8",
);
const win32Source = readFileSync(
  new URL("../src-tauri/src/win32.rs", import.meta.url),
  "utf8",
);

assert.match(
  overlaySource,
  /invoke\("set_overlay_hit_height", \{ height: logicalPx, width: logicalWidth \}\)/,
  "frontend must report both hit-region dimensions",
);
assert.match(win32Source, /static CONTENT_HIT_WIDTH: AtomicI32/);
assert.match(win32Source, /CreateRectRgn\(left, top, win_w, win_h\)/);
assert.match(
  win32Source,
  /pt\.x >= rect\.right - content_width/,
  "native cursor hit testing must exclude transparent width on the left",
);

const section = (start, end) => {
  const startIndex = overlaySource.indexOf(start);
  const endIndex = overlaySource.indexOf(end, startIndex);
  assert.notEqual(startIndex, -1, `${start} must exist`);
  assert.notEqual(endIndex, -1, `${end} must exist after ${start}`);
  return overlaySource.slice(startIndex, endIndex);
};

const settingsClose = section(
  "function requestSettingsClose()",
  "function onSettingsToggle()",
);
const providerClose = section("function closeProvider()", "function handlePick(");

// Animated close (Escape / gear / segment toggle) must clip the native hit
// strip to the bar BEFORE the slide-out so the shrinking popup never exposes
// the tall transparent window mid-fade.
for (const [name, closePath] of [
  ["settings", settingsClose],
  ["provider", providerClose],
]) {
  const clipIndex = closePath.indexOf("void setHitHeight(barHeight())");
  const animationIndex = closePath.indexOf("setSettingsClosing(true)") >= 0
    ? closePath.indexOf("setSettingsClosing(true)")
    : closePath.indexOf("setProviderClosing(true)");
  assert.notEqual(clipIndex, -1, `${name} animated close must clip to the bar`);
  assert.notEqual(
    animationIndex,
    -1,
    `${name} animated close animation must exist`,
  );
  assert.ok(
    clipIndex < animationIndex,
    `${name} animated close must clip before its closing animation exposes the tall window`,
  );
}

// Outside-click close is driven by mount-time listeners only: document
// pointerdown for clicks captured by the reserve, Tauri focus for ordinary
// external windows, and a native foreground bridge for the Windows taskbar.
// The DOM `blur` listener is intentionally absent because it raced Tauri focus
// and could leave the popup mounted until a refocus. The close is synchronous
// (no setTimeout) so nothing stalls on the just-unfocused window.
const focusListenerEffect = section(
  "const onTauriFocus",
  "void unlistenPromise.then((fn) => fn());",
);
assert.ok(
  focusListenerEffect.indexOf("if (focused)") !== -1,
  "focus listener must ignore focus-gain events",
);
assert.equal(
  focusListenerEffect.indexOf("setTimeout("),
  -1,
  "outside-click close must unmount synchronously (no setTimeout in the listener)",
);
assert.notEqual(
  section(
    "function closePopupsImmediately()",
    "const onDocumentPointerDown",
  ).indexOf("activeLabelRef.current"),
  -1,
  "shared close helper must read activeLabel from a ref",
);
assert.notEqual(
  section(
    "function closePopupsImmediately()",
    "const onDocumentPointerDown",
  ).indexOf("settingsOpenRef.current"),
  -1,
  "shared close helper must read settingsOpen from a ref",
);
assert.notEqual(
  section(
    "function closePopupsImmediately()",
    "const onDocumentPointerDown",
  ).indexOf("outsideCloseFiredRef"),
  -1,
  "shared close helper must be guarded to close once per open cycle",
);
assert.notEqual(
  focusListenerEffect.indexOf("closePopupsImmediately()"),
  -1,
  "Tauri focus loss must use the shared close helper",
);

// Focus loss during the paint/unclip sequence must be deferred, never dropped
// behind an elapsed-time grace period. Once the native region is ready, the
// pending request is resolved against the HWND's current focus state.
assert.equal(
  overlaySource.includes("ignoreOutsideCloseUntilRef"),
  false,
  "outside focus loss must not be discarded by an elapsed-time guard",
);
assert.equal(
  overlaySource.includes("armOutsideCloseGuard"),
  false,
  "popup opening must not arm a time-based outside-close suppression window",
);
assert.notEqual(
  overlaySource.indexOf("const pendingOutsideCloseRef = useRef(false)"),
  -1,
  "focus loss during popup preparation must be latched",
);

const beginCycle = section(
  "function beginOutsideCloseCycle(",
  "function applySettingsInterval(",
);
for (const required of [
  "pendingOutsideCloseRef.current = false",
  "outsideCloseFiredRef.current = false",
  "activeLabelRef.current = nextActiveLabel",
  "settingsOpenRef.current = nextSettingsOpen",
  "popupReadyRef.current = false",
]) {
  assert.notEqual(
    beginCycle.indexOf(required),
    -1,
    `open-cycle sync must contain ${required}`,
  );
}

const closeHelper = section(
  "function closePopupsImmediately()",
  "async function resolvePendingOutsideClose()",
);
assert.notEqual(
  closeHelper.indexOf("pendingOutsideCloseRef.current = true"),
  -1,
  "an outside close received before readiness must be deferred",
);

const pendingResolver = section(
  "async function resolvePendingOutsideClose()",
  "const onDocumentPointerDown",
);
assert.notEqual(
  pendingResolver.indexOf("currentWindow.isFocused()"),
  -1,
  "deferred focus loss must be resolved against current native focus",
);
assert.notEqual(
  pendingResolver.indexOf("closePopupsImmediately()"),
  -1,
  "an unresolved outside focus state must use the shared close path",
);

const openSequence = section(
  'const openPaintThenUnclip = async (kind: "settings" | "provider")',
  "if (settingsOpen) {",
);
const unclipIndex = openSequence.indexOf(
  "await setHitHeight(contentStripH(visiblePopupH))",
);
const readyIndex = openSequence.indexOf(
  "popupReadyRef.current = true",
  unclipIndex,
);
const resolveIndex = openSequence.indexOf(
  "await resolvePendingOutsideClose()",
  readyIndex,
);
assert.ok(
  unclipIndex >= 0 && readyIndex > unclipIndex && resolveIndex > readyIndex,
  "pending focus loss must resolve only after region expansion is ready",
);

assert.notEqual(
  section("function openSettingsPopup()", "function requestSettingsClose()")
    .indexOf("beginOutsideCloseCycle(null, true)"),
  -1,
  "Settings must synchronize refs in its click handler",
);
assert.notEqual(
  section("function handlePick(", "return (")
    .indexOf("beginOutsideCloseCycle(label, false)"),
  -1,
  "provider opens must synchronize refs in their click handler",
);

// Settings→provider and provider→provider swaps run on short animation timers.
// An outside close during that delay must invalidate the callback so it cannot
// reopen a popup after the taskbar has already taken focus.
assert.notEqual(
  closeHelper.indexOf("resizeTokenRef.current += 1"),
  -1,
  "outside close must invalidate pending popup transition callbacks",
);
const pickHandler = section("function handlePick(", "return (");
for (const [name, declaration, guard] of [
  [
    "Settings-to-provider",
    "const settingsSwapToken = ++resizeTokenRef.current",
    "if (resizeTokenRef.current !== settingsSwapToken) return",
  ],
  [
    "provider-to-provider",
    "const providerSwitchToken = ++resizeTokenRef.current",
    "if (resizeTokenRef.current !== providerSwitchToken) return",
  ],
]) {
  assert.notEqual(
    pickHandler.indexOf(declaration),
    -1,
    `${name} transition must capture a lifecycle token`,
  );
  assert.notEqual(
    pickHandler.indexOf(guard),
    -1,
    `${name} transition must reject a stale timer`,
  );
}

// The DOM `blur` close listener must be gone entirely — it was the second,
// racing close source.
assert.equal(
  overlaySource.indexOf('window.addEventListener("blur"'),
  -1,
  "no DOM blur close listener must remain (races the Tauri focus signal)",
);

// A popup can make the preallocated transparent reserve part of the native
// hit region. A click there stays in this webview, so it does not produce a
// Tauri focus-loss event. One mount-time document pointer listener must close
// that case while leaving visible bar/popup controls alone.
assert.notEqual(
  overlaySource.indexOf("function closePopupsImmediately()"),
  -1,
  "DOM, Tauri, and native outside-click paths must share one immediate-close helper",
);
assert.equal(
  (overlaySource.match(/document\.addEventListener\(\"pointerdown\"/g) || [])
    .length,
  1,
  "outside pointer handling must use exactly one document listener",
);
const pointerListenerEffect = section(
  "const onDocumentPointerDown",
  "document.removeEventListener(\"pointerdown\"",
);
assert.notEqual(
  pointerListenerEffect.indexOf("widgetRef.current?.contains"),
  -1,
  "document outside-click handling must preserve clicks inside visible content",
);
assert.notEqual(
  pointerListenerEffect.indexOf(
    "document.addEventListener(\"pointerdown\", onDocumentPointerDown, true)",
  ),
  -1,
  "document outside-click handling must use capture phase",
);
assert.notEqual(
  pointerListenerEffect.indexOf("closePopupsImmediately()"),
  -1,
  "document outside-click handling must call the shared close helper",
);

// Windows taskbar activation can change the foreground HWND without sending
// the WebView's Tauri focus-loss event. The native poll must bridge that
// transition into the same guarded frontend close path.
assert.notEqual(
  overlaySource.indexOf('window.addEventListener("overlay-focus-lost"'),
  -1,
  "overlay must listen for the native foreground-loss browser event",
);
assert.notEqual(
  win32Source.indexOf("GetForegroundWindow"),
  -1,
  "Win32 poll must observe the foreground window",
);
assert.notEqual(
  win32Source.indexOf('win.eval("window.dispatchEvent'),
  -1,
  "Win32 foreground loss must dispatch directly inside the overlay WebView",
);

console.log("popup close hit-region tests passed");
