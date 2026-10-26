//! Per-window Windows 11 frame preferences. No global DWM or system changes.

use std::{ffi::c_void, mem::size_of};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND,
};

pub(super) fn apply(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|error| error.to_string())?;
    let corner = DWMWCP_ROUND;
    let border = DWMWA_COLOR_NONE;
    // SAFETY: Tauri supplies this live window's HWND. Both attribute buffers are
    // correctly typed, sized, and remain alive throughout these synchronous calls.
    // Tao's shadow(true) already enables the DWM frame/shadow; extending another
    // frame here would risk adding a visible native edge to our custom chrome.
    let results = unsafe {
        [
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const c_void,
                size_of::<windows::Win32::Graphics::Dwm::DWM_WINDOW_CORNER_PREFERENCE>() as u32,
            ),
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border as *const _ as *const c_void,
                size_of::<u32>() as u32,
            ),
        ]
    };
    for result in results {
        if let Err(error) = result {
            // Windows 10 does not implement these Windows 11 attributes. Retain
            // its native shadow and square corners instead of preventing launch.
            if error.code() != windows::core::HRESULT(0x80070057u32 as i32) {
                return Err(format!("Settings DWM frame preference: {error}"));
            }
        }
    }
    Ok(())
}
