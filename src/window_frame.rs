use crate::sidebar::{ink, ui_font};
use crate::workspace::Workspace;
use rune::{Cx, El, ImageCache, col, el, row, text};

pub const HEIGHT: f32 = 53.;

pub fn titlebar(w: &Workspace, cx: &mut Cx<Workspace>) -> El {
    let insets = cx.chrome_insets();
    cx.caption_color(ink(0xd5dce5));
    let brand = w.caption_brand.get_or_init(|| {
        ImageCache::new(w.wake.clone())
            .load(&include_bytes!("../assets/brand/png/mcli-caption.png")[..])
    });
    let mut contents = row()
        .w_full()
        .grow(1.)
        .min_h(0.)
        .pl(insets.leading + 23.)
        .pr(insets.trailing)
        .items_center()
        .clip_soft(0.)
        .child(
            el().key("caption-brand")
                .role("img")
                .label("Mcli")
                .size(96., 32.)
                .pixels(brand.pixels()),
        );
    if w.layout.viewport.get()[0] - insets.leading - insets.trailing >= 325. {
        contents = contents
            .child(el().w(19.))
            .child(
                el().key("caption-divider")
                    .size(1., 19.)
                    .fill(ink(0x474e5a)),
            )
            .child(el().w(18.))
            .child(
                text(if crate::single_instance::is_dev_channel() {
                    "多 Agent 终端工作台 · DEV"
                } else {
                    "多 Agent 终端工作台"
                })
                .key("caption-description")
                .font(ui_font(16., 460.))
                .color(ink(0xd2d7df)),
            );
    }
    let mut bar = col()
        .key("workspace-titlebar")
        .w_full()
        .h(HEIGHT)
        .window_drag()
        .linear_gradient(
            std::f32::consts::FRAC_PI_2,
            [
                (0., ink(0x0d111b)),
                (0.40, ink(0x101420)),
                (1., ink(0x141d2f)),
            ],
        );
    if is_windows_10() {
        bar = bar.child(el().w_full().h(1.).fill(ink(0x2b323e)));
    }
    bar.child(contents)
        .child(el().w_full().h(1.).fill(ink(0x2b323e)))
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_windows_10_version(major: u32, build: u32) -> bool {
    major == 10 && build < 22000
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn adjust_win10_nccalc_top(
    outer_top: i32,
    current_top: i32,
    framed: bool,
    zoomed: bool,
) -> i32 {
    if framed && !zoomed {
        outer_top
    } else {
        current_top
    }
}

#[cfg(windows)]
pub fn is_windows_10() -> bool {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

    static WIN10: OnceLock<bool> = OnceLock::new();
    *WIN10.get_or_init(|| {
        #[repr(C)]
        struct OsVersionInfoW {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            service_pack: [u16; 128],
        }
        let mut info = OsVersionInfoW {
            size: std::mem::size_of::<OsVersionInfoW>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
        // SAFETY: `ntdll.dll` is always loaded; `RtlGetVersion` fills `OsVersionInfoW`.
        unsafe {
            let module = GetModuleHandleW(ntdll.as_ptr());
            if module.is_null() {
                return false;
            }
            let Some(func) = GetProcAddress(module, c"RtlGetVersion".as_ptr().cast()) else {
                return false;
            };
            let get: unsafe extern "system" fn(*mut OsVersionInfoW) -> i32 =
                std::mem::transmute(func);
            get(&mut info) == 0 && is_windows_10_version(info.major, info.build)
        }
    })
}

#[cfg(not(windows))]
pub fn is_windows_10() -> bool {
    false
}

/// On Windows 10 (build < 22000), Rune's `CustomFrame` leaves a 1 px top non-client row
/// (`rgrc[0].top = outer.top + 1`) on restored windows while keeping `WS_CAPTION` without
/// `DwmExtendFrameIntoClientArea`. On Windows 10 DWM this resurrects a duplicate native
/// title bar above our custom title bar. Zeroing the top non-client inset back to `outer.top`
/// matches Windows 11 and maximized state, cleanly eliminating the duplicate frame.
#[cfg(all(windows, not(test)))]
pub fn install_win10_frame_fix(handle: &rune::WindowHandle) {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::UI::Shell::SetWindowSubclass;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, IsZoomed, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
    };

    if !is_windows_10() {
        return;
    }
    let Ok(Some(hwnd)) = handle.with_raw_handle(|raw| match raw.as_raw() {
        rune::raw_window_handle::RawWindowHandle::Win32(win) => Some(win.hwnd.get() as HWND),
        _ => None,
    }) else {
        return;
    };

    // SAFETY: `hwnd` is a live window owned by the UI thread. `SetWindowSubclass` with a
    // fixed ID is idempotent and cleaned up on `WM_NCDESTROY`.
    unsafe {
        if SetWindowSubclass(hwnd, Some(win10_frame_subclass), WIN10_FRAME_SUBCLASS_ID, 0) == 0 {
            return;
        }
        let mut before = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let has_before = GetClientRect(hwnd, &mut before) != 0;
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_NOOWNERZORDER,
        );
        let mut after = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut win_rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if has_before
            && GetClientRect(hwnd, &mut after) != 0
            && IsZoomed(hwnd) == 0
            && GetWindowRect(hwnd, &mut win_rect) != 0
        {
            let grown = (after.bottom - after.top) - (before.bottom - before.top);
            if grown != 0 {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    win_rect.right - win_rect.left,
                    (win_rect.bottom - win_rect.top - grown).max(1),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
            }
        }
    }
}

#[cfg(all(windows, not(test)))]
const WIN10_FRAME_SUBCLASS_ID: usize = 0x4d43_4c49; // "MCLI"

#[cfg(all(windows, not(test)))]
unsafe extern "system" fn win10_frame_subclass(
    hwnd: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetWindowLongPtrW, IsZoomed, NCCALCSIZE_PARAMS, WM_NCCALCSIZE, WM_NCDESTROY,
        WS_CAPTION,
    };

    if message == WM_NCDESTROY {
        // SAFETY: removes this subclass on window destruction.
        unsafe {
            RemoveWindowSubclass(hwnd, Some(win10_frame_subclass), WIN10_FRAME_SUBCLASS_ID);
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
    }
    if message == WM_NCCALCSIZE && wparam != 0 {
        // SAFETY: when `wparam != 0`, `lparam` points to `NCCALCSIZE_PARAMS`.
        unsafe {
            let params = &mut *(lparam as *mut NCCALCSIZE_PARAMS);
            let outer_top = params.rgrc[0].top;
            let result = DefSubclassProc(hwnd, message, wparam, lparam);
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let framed = style & WS_CAPTION == WS_CAPTION;
            let zoomed = IsZoomed(hwnd) != 0;
            params.rgrc[0].top =
                adjust_win10_nccalc_top(outer_top, params.rgrc[0].top, framed, zoomed);
            return result;
        }
    }
    // SAFETY: forwards all other messages to the next procedure in the subclass chain.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_windows_10_builds_only_below_22000() {
        assert!(is_windows_10_version(10, 17763));
        assert!(is_windows_10_version(10, 19045));
        assert!(is_windows_10_version(10, 21999));
        assert!(!is_windows_10_version(10, 22000));
        assert!(!is_windows_10_version(10, 26300));
        assert!(!is_windows_10_version(11, 22000));
    }

    #[test]
    fn zeroes_top_non_client_inset_only_for_restored_framed_windows() {
        assert_eq!(adjust_win10_nccalc_top(100, 101, true, false), 100);
        assert_eq!(adjust_win10_nccalc_top(100, 108, true, true), 108);
        assert_eq!(adjust_win10_nccalc_top(100, 101, false, false), 101);
    }
}
