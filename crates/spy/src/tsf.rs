//! Passive owner-thread metadata; COM references never escape one sample.
//! The SDK msctf.h declares the getter, omitted from windows 0.58 bindings.
use super::tsf_result;
use serde_json::{json, Value};
use std::{cell::Cell, ffi::c_void};
use windows::core::{Interface, HRESULT};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfContextComposition, ITfDocumentMgr, ITfThreadMgr,
};

// Exact documented SDK signature: HRESULT WINAPI TF_GetThreadMgr(ITfThreadMgr **pptim).
#[link(name = "msctf", kind = "raw-dylib")]
extern "system" {
    fn TF_GetThreadMgr(pptim: *mut *mut c_void) -> HRESULT;
}

thread_local! { static SAMPLING: Cell<bool> = const { Cell::new(false) }; }
struct SamplingGuard;
impl Drop for SamplingGuard {
    fn drop(&mut self) {
        SAMPLING.with(|s| s.set(false));
    }
}

pub(crate) fn snapshot() -> Value {
    if SAMPLING.with(|s| s.replace(true)) {
        return json!({"state":"skipped_reentry"});
    }
    let _guard = SamplingGuard;
    // Caller checked that the HWND belongs to this current UI thread.
    unsafe { sample() }
}

unsafe fn sample() -> Value {
    let mut raw = std::ptr::null_mut();
    let hr = TF_GetThreadMgr(&mut raw);
    let manager = (!raw.is_null()).then(|| ITfThreadMgr::from_raw(raw));
    let mut record = json!({"thread_manager":tsf_result("thread_manager", hr.0, manager.is_some()),
        "document_manager":null, "context":null, "status":null, "composition":null});
    if hr.is_err() {
        return record;
    }
    let Some(manager) = manager else {
        return record;
    };
    raw = std::ptr::null_mut();
    // Binding vtables preserve successful NULL versus HRESULT failure, unlike
    // convenience wrappers which convert a NULL interface to E_POINTER.
    let hr = (manager.vtable().GetFocus)(manager.as_raw(), &mut raw);
    let document = (!raw.is_null()).then(|| ITfDocumentMgr::from_raw(raw));
    record["document_manager"] = tsf_result("document_manager", hr.0, document.is_some());
    if hr.is_err() {
        return record;
    }
    let Some(document) = document else {
        return record;
    };
    raw = std::ptr::null_mut();
    let hr = (document.vtable().GetTop)(document.as_raw(), &mut raw);
    let context = (!raw.is_null()).then(|| ITfContext::from_raw(raw));
    record["context"] = tsf_result("context", hr.0, context.is_some());
    if hr.is_err() {
        return record;
    }
    let Some(context) = context else {
        return record;
    };
    record["status"] = match context.GetStatus() {
        Ok(status) => json!({"state":"available", "dynamic_flags":status.dwDynamicFlags,
            "static_flags":status.dwStaticFlags}),
        Err(error) => tsf_result("status", error.code().0, false),
    };
    record["composition"] = match context.cast::<ITfContextComposition>() {
        Err(error) => tsf_result("composition_interface", error.code().0, false),
        Ok(composition) => match composition.EnumCompositions() {
            Err(error) => tsf_result("composition_enumerator", error.code().0, false),
            Ok(enumerator) => {
                let mut views = [None; 1];
                let mut fetched = 0;
                match enumerator.Next(&mut views, &mut fetched) {
                    Err(error) => tsf_result("composition_next", error.code().0, false),
                    Ok(()) => {
                        json!({"state":"available", "present":fetched == 1 && views[0].is_some()})
                    }
                }
            }
        },
    };
    record
}
