//! Win32 helpers for atomic window geometry changes.
//!
//! Tauri exposes `set_size` and `set_position` as two separate calls, which
//! causes a visible flicker when both change at once (the window renders at
//! the intermediate state). `SetWindowPos` from user32 changes both in a
//! single message, so the bar never appears in the wrong place.

#[cfg(windows)]
pub use windows_impl::*;

/// Clamp the native window's y-coordinate using the visible bar at its bottom
/// edge. The transparent popup buffer may extend above the monitor; clamping
/// the full preallocated window is what prevents the bar from being dragged to
/// the top of the screen.
pub(crate) fn clamp_window_y_to_bar(
    window_y: i32,
    window_h: i32,
    bar_h: i32,
    work_top: i32,
    work_bottom: i32,
) -> i32 {
    let min_y = work_top.saturating_add(bar_h).saturating_sub(window_h);
    let max_y = work_bottom.saturating_sub(window_h);
    if min_y > max_y {
        return work_top;
    }
    window_y.clamp(min_y, max_y)
}

/// Resize a window from its right edge while keeping the final horizontal
/// rectangle inside the active monitor work area. If the requested width is
/// wider than the work area, the window fills that work area instead.
pub(crate) fn clamp_right_anchored_width(
    window_x: i32,
    window_width: i32,
    requested_width: i32,
    work_left: i32,
    work_right: i32,
) -> (i32, i32) {
    let work_width = work_right.saturating_sub(work_left).max(1);
    let width = requested_width.clamp(1, work_width);
    let right = window_x.saturating_add(window_width.max(0));
    let desired_x = right.saturating_sub(width);
    let max_x = work_left.saturating_add(work_width).saturating_sub(width);
    (desired_x.clamp(work_left, max_x), width)
}

#[cfg(windows)]
mod windows_impl {
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
    use std::time::Duration;

    use tauri::Manager;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateRectRgn, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, SetWindowLongPtrW,
        SetWindowPos, GWL_EXSTYLE, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION, WS_EX_TRANSPARENT, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
        WS_SYSMENU, WS_THICKFRAME,
    };

    /// Collapsed bar height in logical px — KEEP IN SYNC with `.bar { height }`
    /// and BAR_H_LOGICAL in commands.rs / overlay.tsx.
    const BAR_H_LOGICAL: f64 = 36.0;
    /// Extra physical padding above an expanded content strip so popup edges
    /// aren't hard-clipped by the window region. A collapsed strip must not
    /// include this padding or it briefly exposes the closing popup's bottom.
    const REGION_PAD_LOGICAL: f64 = 10.0;

    /// Guards the poll task so it is spawned exactly once per process, even if
    /// `open_overlay` runs several times (re-show). The task then lives for the
    /// whole app lifetime, looking the window up by label each tick.
    static POLL_STARTED: AtomicBool = AtomicBool::new(false);

    /// Coalesces delayed non-client refreshes after rapid region changes. Only
    /// the latest mutation needs to repaint after WebView2's asynchronous
    /// native-frame work has settled.
    static BORDERLESS_REFRESH_EPOCH: AtomicU64 = AtomicU64::new(0);

    /// Height of the interactive content strip measured from the *bottom* of
    /// the overlay window, in physical pixels. The overlay is intentionally
    /// kept taller than the bar (pre-sized to the tallest popup) so open/close
    /// never needs a visible SetWindowPos — which flashes a transparent
    /// window. Only this bottom strip (bar, or bar + popup) should capture
    /// mouse input; the empty transparent region above stays click-through.
    ///
    /// `0` means "bar height only" (safe default — never claim the full tall
    /// window, or the empty area becomes an invisible wall).
    static CONTENT_HIT_HEIGHT: AtomicI32 = AtomicI32::new(0);
    /// Width of the interactive content rectangle measured from the right
    /// edge, in physical pixels. `0` safely means the full HWND width until
    /// the frontend reports the visible minibar width.
    static CONTENT_HIT_WIDTH: AtomicI32 = AtomicI32::new(0);

    fn padded_content_strip(base: i32, bar_phys: i32, pad: i32, win_h: i32) -> i32 {
        let expanded_pad = if base > bar_phys { pad } else { 0 };
        (base + expanded_pad).clamp(1, win_h.max(1))
    }

    fn content_rect(
        win_w: i32,
        win_h: i32,
        stored_width: i32,
        stored_height: i32,
    ) -> (i32, i32, i32, i32) {
        let width = if stored_width <= 0 {
            win_w.max(1)
        } else {
            stored_width.clamp(1, win_w.max(1))
        };
        let height = if stored_height <= 0 {
            win_h.max(1)
        } else {
            stored_height.clamp(1, win_h.max(1))
        };
        (win_w - width, win_h - height, win_w, win_h)
    }

    /// Resolve the content-strip height in physical px (from the bottom).
    fn resolved_hit_height(window: &tauri::WebviewWindow, win_h: i32) -> i32 {
        let stored = CONTENT_HIT_HEIGHT.load(Ordering::Relaxed);
        let scale = window.scale_factor().unwrap_or(1.0);
        let bar_phys = (BAR_H_LOGICAL * scale).round() as i32;
        let pad = (REGION_PAD_LOGICAL * scale).round() as i32;
        let base = if stored <= 0 { bar_phys } else { stored };
        padded_content_strip(base, bar_phys, pad, win_h)
    }

    /// Clip the HWND to the bottom content strip via `SetWindowRgn`.
    ///
    /// This is what actually makes click-through work for the pre-sized tall
    /// window: pixels outside the region are not part of the window for hit
    /// testing, so clicks fall through to apps beneath — even if the
    /// WS_EX_TRANSPARENT poll is briefly wrong. The webview client area stays
    /// full-size (no SetWindowPos), so open/close only changes the region.
    ///
    /// The redraw flag is TRUE so newly exposed popup pixels paint immediately.
    /// WebView2 may then repaint a cached native caption asynchronously even
    /// though its style bits remain absent; the synchronous and coalesced
    /// delayed `enforce_borderless` calls below clear that stale frame.
    fn apply_content_region(window: &tauri::WebviewWindow) -> Result<(), String> {
        // WebView2 can restore the native frame while the window is shown or
        // reshaped. Remove it before SetWindowRgn's redraw, otherwise the
        // transparent buffer briefly paints the "AI Usage Tracker" titlebar.
        enforce_borderless(window)?;
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect as *mut _).is_err() {
                return Err("GetWindowRect failed".into());
            }
            let win_w = (rect.right - rect.left).max(1);
            let win_h = (rect.bottom - rect.top).max(1);
            let strip = resolved_hit_height(window, win_h);
            let stored_width = CONTENT_HIT_WIDTH.load(Ordering::Relaxed);
            let (left, top, _, _) = content_rect(win_w, win_h, stored_width, strip);

            let changed = if strip >= win_h && left == 0 {
                // Full window — clear any prior region.
                SetWindowRgn(hwnd, None, true)
            } else {
                // Region coordinates are relative to the window's top-left.
                let hrgn = CreateRectRgn(left, top, win_w, win_h);
                // On success the system owns `hrgn` and will free it.
                SetWindowRgn(hwnd, Some(hrgn), true)
            };
            // SetWindowRgn returns nonzero on success.
            if changed == 0 {
                return Err("SetWindowRgn failed".into());
            }
        }
        // SetWindowRgn can repaint a cached non-client caption even when the
        // HWND style is already borderless. Recalculate the frame after the
        // redraw so that stale caption never remains visible.
        enforce_borderless(window)?;
        schedule_borderless_refresh(window.clone());
        Ok(())
    }

    /// Store the content-strip height without applying the region yet.
    /// Used by `set_overlay_geometry` so a grow + region reshape use the
    /// *new* height instead of the stale bar-only strip.
    pub fn store_content_hit_height(physical_px: i32) {
        CONTENT_HIT_HEIGHT.store(physical_px.max(0), Ordering::Relaxed);
    }

    pub fn store_content_hit_width(physical_px: i32) {
        CONTENT_HIT_WIDTH.store(physical_px.max(0), Ordering::Relaxed);
    }

    /// WebView2 can repaint a cached caption shortly after SetWindowRgn and
    /// after the synchronous frame refresh returns. Reapply SWP_FRAMECHANGED
    /// once that work settles. Epoch coalescing prevents rapid open/close
    /// region changes from stacking stale refreshes.
    fn schedule_borderless_refresh(window: tauri::WebviewWindow) {
        let epoch = BORDERLESS_REFRESH_EPOCH
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if BORDERLESS_REFRESH_EPOCH.load(Ordering::SeqCst) != epoch {
                return;
            }
            if let Err(e) = enforce_borderless(&window) {
                log::warn!("delayed borderless refresh failed: {e}");
            }
        });
    }

    /// Update the interactive content strip height (physical px from bottom)
    /// and reshape the window region so empty space above the bar is not
    /// hittable.
    pub fn set_content_hit_height(
        window: &tauri::WebviewWindow,
        physical_px: i32,
    ) -> Result<(), String> {
        store_content_hit_height(physical_px);
        apply_content_region(window)
    }

    /// Update both dimensions of the bottom-right interactive content
    /// rectangle. Transparent reserve above and to the left remains outside
    /// the HWND region and therefore cannot capture clicks.
    pub fn set_content_hit_size(
        window: &tauri::WebviewWindow,
        physical_width: i32,
        physical_height: i32,
    ) -> Result<(), String> {
        store_content_hit_width(physical_width);
        store_content_hit_height(physical_height);
        apply_content_region(window)
    }

    /// Remove native title-bar and frame styles from the tracker overlay.
    /// Tauri's builder-level `decorations(false)` is not reliably reflected in
    /// the HWND for this transparent, initially-hidden WebView2 window, which
    /// leaves a title bar above the preallocated popup buffer on Windows.
    pub fn enforce_borderless(window: &tauri::WebviewWindow) -> Result<(), String> {
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        let decoration_bits =
            (WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0)
                as isize;
        unsafe {
            let current = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let borderless = current & !decoration_bits;
            if borderless != current {
                let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, borderless);
            }
            // SWP_FRAMECHANGED is required even when the style bits were
            // already absent: SetWindowRgn may have painted a stale cached
            // caption without restoring WS_CAPTION in GWL_STYLE.
            SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
            )
            .map_err(|e| format!("SetWindowPos(borderless): {e}"))?;
        }
        Ok(())
    }

    /// Atomically set a window's outer rectangle (position + size) in physical
    /// pixels. This is the moral equivalent of `win.set_position(p); win.set_size(s);`
    /// but applied in one Win32 message so there is no intermediate render.
    ///
    /// Re-applies the content region afterward so width/height changes cannot
    /// leave a stale region that either clips the bar or re-opens a hit hole.
    pub fn set_window_rect(
        window: &tauri::WebviewWindow,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> Result<(), String> {
        // Strip a frame before SetWindowPos can repaint the visible
        // transparent buffer with a native titlebar.
        enforce_borderless(window)?;
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        unsafe {
            SetWindowPos(hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER)
                .map_err(|e| format!("SetWindowPos: {e}"))?;
        }
        // Best-effort: geometry change succeeded even if region refresh fails.
        let _ = apply_content_region(window);
        Ok(())
    }

    /// Convenience: look up a window by label and call `set_window_rect`.
    pub fn set_window_rect_by_label(
        app: &tauri::AppHandle,
        label: &str,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    ) -> Result<(), String> {
        let win = app
            .get_webview_window(label)
            .ok_or_else(|| format!("window '{label}' not found"))?;
        set_window_rect(&win, x, y, w, h)
    }

    /// Return the monitor work-area rect (physical px) for the monitor the
    /// window currently overlaps. The work area is the desktop minus the
    /// taskbar, so clamping to it keeps the bar from parking under the
    /// taskbar (where Windows renders the taskbar on top of it).
    pub fn work_area(window: &tauri::WebviewWindow) -> Result<RECT, String> {
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                rcMonitor: RECT::default(),
                rcWork: RECT::default(),
                dwFlags: 0,
            };
            if !GetMonitorInfoW(monitor, &mut info as *mut _).as_bool() {
                return Err("GetMonitorInfoW returned false".into());
            }
            Ok(info.rcWork)
        }
    }

    /// Return the window's outer rect corrected to lie fully within the monitor
    /// work area. If the window already fits, the values are unchanged. Lets a
    /// caller compute the bar's true bottom edge *after* clearing the taskbar.
    pub fn clamped_rect(window: &tauri::WebviewWindow) -> Result<(i32, i32, i32, i32), String> {
        let pos = window.outer_position().map_err(|e| e.to_string())?;
        let size = window.outer_size().map_err(|e| e.to_string())?;
        let work = work_area(window)?;
        let w = size.width as i32;
        let h = size.height as i32;
        let max_x = (work.right - w).max(work.left);
        let scale = window.scale_factor().unwrap_or(1.0);
        let bar_h = (BAR_H_LOGICAL * scale).round() as i32;
        let x = pos.x.clamp(work.left, max_x);
        let y = super::clamp_window_y_to_bar(pos.y, h, bar_h, work.top, work.bottom);
        Ok((x, y, w, h))
    }

    /// Move the window fully inside the monitor work area if any part of it
    /// overlaps the taskbar. Returns true if a reposition was applied.
    pub fn clamp_into_work_area(window: &tauri::WebviewWindow) -> Result<bool, String> {
        let pos = window.outer_position().map_err(|e| e.to_string())?;
        let (x, y, w, h) = clamped_rect(window)?;
        if x == pos.x && y == pos.y {
            return Ok(false);
        }
        set_window_rect(window, x, y, w, h)?;
        Ok(true)
    }

    /// Toggle the WS_EX_TRANSPARENT extended style. When set, mouse and
    /// pen events pass through the window to whatever is beneath — essential
    /// for letting snipping tools, drawing apps, and screen markers operate
    /// over the overlay. The frontend pairs this with hover-enter/leave to
    /// briefly take input when the user actually wants to interact.
    pub fn set_click_through(
        window: &tauri::WebviewWindow,
        click_through: bool,
    ) -> Result<(), String> {
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        unsafe {
            let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let bit = WS_EX_TRANSPARENT.0 as isize;
            let new = if click_through {
                current | bit
            } else {
                current & !bit
            };
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        }
        Ok(())
    }

    /// True when the mouse cursor is currently over the interactive content
    /// strip (bar, or bar + open popup) at the bottom of the overlay window.
    /// Both `GetCursorPos` and `GetWindowRect` report physical pixels, so the
    /// comparison is DPI-correct without any conversion.
    ///
    /// Note: `GetWindowRect` ignores `SetWindowRgn`, so we still apply the
    /// bottom-strip test here for the WS_EX_TRANSPARENT poll. The region is
    /// the hard guarantee that empty space never eats clicks; this poll only
    /// decides when the *content* strip should be interactive vs pass-through
    /// (so snipping tools can operate over the bar when the cursor is away).
    pub fn cursor_over_content(window: &tauri::WebviewWindow) -> Result<bool, String> {
        let hwnd_raw = window.hwnd().map_err(|e| e.to_string())?;
        let hwnd = HWND(hwnd_raw.0 as *mut _);
        unsafe {
            let mut pt = POINT::default();
            if GetCursorPos(&mut pt as *mut _).is_err() {
                return Err("GetCursorPos failed".into());
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect as *mut _).is_err() {
                return Err("GetWindowRect failed".into());
            }
            if pt.x < rect.left || pt.x >= rect.right || pt.y < rect.top || pt.y >= rect.bottom {
                return Ok(false);
            }
            let win_h = (rect.bottom - rect.top).max(1);
            let strip = resolved_hit_height(window, win_h);
            let win_w = (rect.right - rect.left).max(1);
            let stored_width = CONTENT_HIT_WIDTH.load(Ordering::Relaxed);
            let content_width = if stored_width <= 0 {
                win_w
            } else {
                stored_width.clamp(1, win_w)
            };
            Ok(pt.x >= rect.right - content_width && pt.y >= rect.bottom - strip)
        }
    }

    /// Spawn (once) a background task that keeps the overlay's click-through
    /// state in sync with the cursor position: interactive when the cursor is
    /// over the bar/popup content strip, pass-through otherwise.
    ///
    /// This replaces the old frontend mouseenter/mouseleave toggle. That
    /// approach was fundamentally unreliable on a click-through window —
    /// while `WS_EX_TRANSPARENT` is set, mouse events pass straight through
    /// the webview, so `mouseenter` often never fired (leaving the bar
    /// unclickable), and when it did fire, `mouseleave` could be missed on
    /// focus loss, leaving click-through stuck *off*. Combined with a tall
    /// pre-sized window and a full-rect hit test, a stuck-off state turned
    /// the whole transparent surface into an invisible wall.
    ///
    /// Polling the cursor natively sidesteps all of that: it needs no webview
    /// events, reacts within one tick, only touches the extended style when
    /// the desired state changes, and only claims the bottom content strip.
    pub fn ensure_click_through_poll(app: tauri::AppHandle) {
        if POLL_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        tauri::async_runtime::spawn(async move {
            poll_loop(app).await;
        });
    }

    /// True only when the previous foreground window was this overlay and the
    /// current foreground window is something else. The taskbar's
    /// `Shell_TrayWnd` can become foreground without WebView2 producing the
    /// Tauri `Focused(false)` event, so the native poll uses this transition as
    /// a reliable outside-click signal.
    fn foreground_left_overlay(previous: Option<isize>, current: isize, overlay: isize) -> bool {
        previous == Some(overlay) && current != overlay
    }

    async fn poll_loop(app: tauri::AppHandle) {
        // A freshly built window starts click-through (see overlay.rs), so
        // mirror that as our assumed current state to avoid a redundant set.
        let mut current_click_through = true;
        let mut last_foreground: Option<isize> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let Some(win) = app.get_webview_window(crate::overlay::OVERLAY_LABEL) else {
                // Window gone (closed/destroyed). A later reopen recreates it
                // click-through, so reset our assumption to match.
                current_click_through = true;
                last_foreground = None;
                continue;
            };
            if let Ok(hwnd) = win.hwnd() {
                let overlay_raw = hwnd.0 as isize;
                let foreground_raw = unsafe { GetForegroundWindow().0 as isize };
                if foreground_left_overlay(last_foreground, foreground_raw, overlay_raw) {
                    if let Err(e) =
                        win.eval("window.dispatchEvent(new Event('overlay-focus-lost'));")
                    {
                        log::warn!("overlay focus-loss eval failed: {e}");
                    }
                }
                last_foreground = Some(foreground_raw);
            }
            // Interactive only while the cursor is over the bar/popup strip.
            // Transparent space above the bar stays pass-through.
            let over = cursor_over_content(&win).unwrap_or(false);
            let want_click_through = !over;
            if want_click_through != current_click_through {
                current_click_through = want_click_through;
                let _ = set_click_through(&win, want_click_through);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::{clamp_right_anchored_width, clamp_window_y_to_bar};
        use super::{content_rect, foreground_left_overlay, padded_content_strip};

        #[test]
        fn content_hit_rect_is_bottom_right_and_clamped() {
            assert_eq!(content_rect(320, 942, 181, 36), (139, 906, 320, 942));
            assert_eq!(content_rect(320, 942, 999, 999), (0, 0, 320, 942));
            assert_eq!(content_rect(320, 942, 0, 0), (0, 0, 320, 942));
        }

        #[test]
        fn collapsed_content_strip_does_not_expose_region_padding() {
            assert_eq!(padded_content_strip(36, 36, 10, 942), 36);
        }

        #[test]
        fn expanded_content_strip_keeps_region_padding() {
            assert_eq!(padded_content_strip(300, 36, 10, 942), 310);
        }

        #[test]
        fn detects_foreground_transition_away_from_overlay() {
            assert!(foreground_left_overlay(Some(100), 200, 100));
            assert!(!foreground_left_overlay(Some(100), 100, 100));
            assert!(!foreground_left_overlay(Some(200), 300, 100));
            assert!(!foreground_left_overlay(None, 200, 100));
        }

        #[test]
        fn allows_the_transparent_buffer_above_the_screen_when_bar_is_at_top() {
            assert_eq!(clamp_window_y_to_bar(-906, 942, 36, 0, 1080), -906);
        }

        #[test]
        fn clamps_the_bar_anchor_instead_of_the_transparent_buffer() {
            // A window dragged to y=450 would put its 36px bar at y=1356.
            // The work area ends at 1080, so the bar should settle at 1044;
            // the hidden 942px buffer is allowed to extend above the screen.
            assert_eq!(clamp_window_y_to_bar(450, 942, 36, 0, 1080), 138);
        }

        #[test]
        fn clamps_right_anchored_width_to_the_active_work_area() {
            assert_eq!(
                clamp_right_anchored_width(1600, 320, 500, 0, 1920),
                (1420, 500),
            );
            assert_eq!(clamp_right_anchored_width(50, 320, 500, 0, 1920), (0, 500),);
            assert_eq!(
                clamp_right_anchored_width(2000, 320, 600, 1920, 3840),
                (1920, 600),
            );
            assert_eq!(
                clamp_right_anchored_width(-800, 320, 500, -1920, 0),
                (-980, 500),
            );
            assert_eq!(
                clamp_right_anchored_width(-100, 200, 2000, -1920, -960),
                (-1920, 960),
            );
            assert_eq!(
                clamp_right_anchored_width(1800, 400, 400, 0, 1920),
                (1520, 400),
            );
        }
    }
}
