use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const OVERLAY_LABEL: &str = "overlay";

/// Preallocated overlay HWND size in logical px. The window always opens at
/// this footprint (tall enough for the tallest popup, wide enough for a full
/// multi-provider bar) and the frontend then shrinks it to fit content. Kept
/// in sync with `OVERLAY_PREALLOCATED_{W,H}_LOGICAL` in `src/overlay.tsx`.
pub const OVERLAY_PREALLOCATED_W_LOGICAL: f64 = 800.0;
pub const OVERLAY_PREALLOCATED_H_LOGICAL: f64 = 942.0;

/// Suppress WebView2's default Edge-style right-click menu ("Save image as",
/// "Copy", "More tools", …). Runs before any page script so the menu never
/// flashes. Keyboard copy/paste still works where inputs allow it.
const DISABLE_BROWSER_CONTEXT_MENU: &str =
    "document.addEventListener('contextmenu', e => e.preventDefault(), true);";

/// Snapshot the overlay's current logical position and persist it to the
/// `overlay_position` config field. Safe to call on any path that ends with
/// the bar going away (hide-to-tray, quit, X-button, OS shutdown), and from
/// the frontend's post-drag debounce. A failure to read the position or write
/// to disk is logged but never propagated — saving the last good position is
/// best-effort and must not block the exit path.
///
/// The persisted coordinates are normalized to the *preallocated* window
/// footprint (`OVERLAY_PREALLOCATED_{W,H}_LOGICAL`), not the window's current
/// shrunk size. After content measurement the overlay narrows (`set_overlay_width`
/// keeps the right edge fixed) and shortens (`set_overlay_geometry` keeps the
/// bottom edge fixed). Saving the shrunk window's top-left would drift: the next
/// open places a full 800×942 window at that top-left, shifting the right edge
/// right by the width delta and the bottom edge down by the height delta, and
/// the re-shrink anchors to the shifted edges — moving the bar on every
/// open/close cycle. Instead we store the top-left a preallocated-sized window
/// needs to reproduce the current right and bottom edges, so opening at that
/// position leaves the visible bar exactly where it was.
pub(crate) fn persist_overlay_position(win: &WebviewWindow) {
    let Ok(pos) = win.outer_position() else {
        return;
    };
    let Ok(size) = win.outer_size() else {
        return;
    };
    let Ok(scale) = win.scale_factor() else {
        return;
    };
    let right = (pos.x as f64 + size.width as f64) / scale;
    let bottom = (pos.y as f64 + size.height as f64) / scale;
    let x = right - OVERLAY_PREALLOCATED_W_LOGICAL;
    let y = bottom - OVERLAY_PREALLOCATED_H_LOGICAL;
    if let Err(e) = crate::secrets::set_overlay_position(crate::secrets::OverlayPosition {
        x,
        y,
        extra: serde_json::Map::new(),
    }) {
        log::warn!("persist_overlay_position: failed to save ({x},{y}): {e}");
    }
}

/// Compute bottom-right position above the Windows taskbar (logical px).
fn bottom_right(app: &AppHandle, w: f64, h: f64) -> (f64, f64) {
    match app.primary_monitor().ok().flatten() {
        Some(monitor) => {
            let scale = monitor.scale_factor();
            let size = monitor.size();
            let pos = monitor.position();
            let mon_w = size.width as f64 / scale;
            let mon_h = size.height as f64 / scale;
            let mon_x = pos.x as f64 / scale;
            let mon_y = pos.y as f64 / scale;
            // 12px right margin, 56px bottom margin to clear taskbar
            let x = mon_x + mon_w - w - 12.0;
            let y = mon_y + mon_h - h - 56.0;
            (x.max(0.0), y.max(0.0))
        }
        None => (100.0, 100.0),
    }
}

/// Spawn (or focus) the overlay window.
pub fn open_overlay(app: &AppHandle) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        win.show()?;
        crate::win32::enforce_borderless(&win).map_err(std::io::Error::other)?;
        win.set_focus()?;
        return Ok(());
    }
    // Preallocate the tallest normal popup while the HWND is hidden. The
    // frontend keeps this height grow-only after show; resizing a visible
    // transparent window is what produces the white frame on popup close.
    // Width starts wide enough for a full multi-provider minibar so the first
    // paint cannot clip trackers before JS measures and calls set_overlay_width.
    // Keep in sync with overlay.tsx OVERLAY_PREALLOCATED_{W,H}_LOGICAL.
    let (w, h) = (OVERLAY_PREALLOCATED_W_LOGICAL, OVERLAY_PREALLOCATED_H_LOGICAL);
    // Prefer the last user-dragged position (logical px), falling back to
    // the bottom-right corner when no position is saved or the saved one
    // would put the bar off-screen.
    let saved = crate::secrets::get_overlay_position();
    let (def_x, def_y) = bottom_right(app, w, h);
    let (x, y) = clamp_to_work_area(app, saved.map(|p| (p.x, p.y)), (def_x, def_y), w, h);
    let _win = WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("index.html".into()))
        .title("AI Usage Tracker")
        .inner_size(w, h)
        .position(x, y)
        .min_inner_size(240.0, 36.0)
        .decorations(false)
        .transparent(true)
        .skip_taskbar(true)
        .always_on_top(true)
        .resizable(false)
        .shadow(false)
        // Start hidden: the frontend measures the tallest popup, grows the
        // window to fit it while still invisible (a SetWindowPos on a
        // *visible* transparent window repaints the surface for a frame and
        // flashes the bar), then calls show(). After that open/close only
        // toggles content — the window stays pre-sized — so the first click
        // never flashes. Click-through uses a content-strip hit test so the
        // empty transparent region above the bar never swallows input.
        .visible(false)
        // Kill the WebView2 "browser" context menu (Save image / Copy / etc.).
        .initialization_script(DISABLE_BROWSER_CONTEXT_MENU)
        .build()?;
    crate::win32::enforce_borderless(&_win).map_err(std::io::Error::other)?;
    // Persist the bar's position whenever it goes away for good.
    // CloseRequested fires on X-button, Alt+F4, taskbar close, and the
    // OS-driven destroy during `quit_cleanly` — all paths the user can
    // take to exit. Hide-to-tray does *not* fire this (it calls .hide()
    // directly), so it persists position in `hide_to_tray` itself. The
    // listener is bound for the lifetime of the window handle. Set after
    // build() because Tauri 2's WebviewWindowBuilder doesn't expose
    // on_window_event as a builder method, and the listener is `Fn +
    // 'static` so it can't borrow local state — we move an AppHandle clone
    // in and look the window up by label each time the listener fires.
    let app_handle = app.clone();
    _win.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed => {
            if let Some(win) = app_handle.get_webview_window(OVERLAY_LABEL) {
                persist_overlay_position(&win);
            }
        }
        _ => {}
    });
    // Start click-through so the overlay doesn't steal events from drawing /
    // snipping tools. The frontend toggles click-through off on hover and
    // back on after the pointer leaves the widget. Note: we deliberately do
    // NOT set WDA_EXCLUDEFROMCAPTURE — the bar should appear in screenshots
    // and screen recordings, just like any other visible window.
    let _ = crate::win32::set_click_through(&_win, true);
    // Clip the HWND to the bar strip immediately so the pre-sized tall
    // window never blocks clicks above the bar, even before the frontend
    // reports a content height.
    let _ = crate::win32::set_content_hit_height(&_win, 0);
    // Keep the overlay's click-through state in sync with the cursor from a
    // native poll (see win32::ensure_click_through_poll). Spawned once; it
    // looks the window up by label each tick, so it survives re-shows.
    crate::win32::ensure_click_through_poll(app.clone());
    Ok(())
}

/// If `saved` is Some and keeps the visible bar inside the primary monitor's
/// work area, return it. The transparent popup buffer may extend above the
/// monitor because the bar is rendered at the bottom of the overlay window.
/// Otherwise fall back to `default`.
fn clamp_to_work_area(
    app: &AppHandle,
    saved: Option<(f64, f64)>,
    default: (f64, f64),
    w: f64,
    h: f64,
) -> (f64, f64) {
    let Some((x, y)) = saved else {
        return default;
    };
    let Some(monitor) = app.primary_monitor().ok().flatten() else {
        return default;
    };
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let pos = monitor.position();
    let mon_w = size.width as f64 / scale;
    let mon_h = size.height as f64 / scale;
    let mon_x = pos.x as f64 / scale;
    let mon_y = pos.y as f64 / scale;
    // The bar is the bottom 36 logical pixels of the preallocated window. The
    // window itself is allowed to extend above the monitor so the bar can sit
    // at the top of the screen without being snapped back down.
    let min_y = mon_y + 36.0 - h;
    let max_y = mon_y + mon_h - h;
    if x < mon_x || y < min_y || x + w > mon_x + mon_w || y > max_y {
        return default;
    }
    (x, y)
}

/// Hide overlay to the tray without quitting. Scheduler and tray
/// keep running; left-click the tray icon (or "Open Tracker") to restore.
///
/// Also persists the overlay's current logical position so the next open
/// restores it — even when the user hides the bar within the frontend's
/// 150ms `onMoved` debounce window (or after a move triggered by
/// `clamp_overlay_position` that hasn't yet been re-saved).
pub fn hide_to_tray(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        persist_overlay_position(&win);
        let _ = win.hide();
    }
    log::info!("hide_to_tray: window hidden; app stays in tray");
}

/// Tear down WebView windows before process exit. Reduces (does not always
/// eliminate) Chromium's noisy `Failed to unregister class Chrome_WidgetWin_0`
/// ERROR_CLASS_DOES_NOT_EXIST (1412) race when multiple HWNDs share a class.
///
/// Also persists the overlay's current position one last time. The window
/// event listener set in `open_overlay` already saves on Destroyed, but
/// this is a belt-and-suspenders write that survives the case where a
/// future refactor changes the destroy path.
pub fn quit_cleanly(app: &AppHandle) {
    log::info!("quit_cleanly: closing windows then exiting");
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        persist_overlay_position(&win);
        let _ = win.hide();
        let _ = win.destroy();
    }
    app.exit(0);
}
