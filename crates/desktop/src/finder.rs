//! macOS: Finder's *New Arugula Tab Here* (M47).
//!
//! Info.plist declares a service (`NSServices`) for folders, so Finder
//! lists it when you right-click one (under Quick Actions or Services) and
//! in its Services menu. macOS starts the app if it isn't running and calls
//! `newTabHere:userData:error:` on the provider registered here, with the
//! selection on a pasteboard: each folder (a file's folder) opens as a new
//! tab, through the same path as `arugula://open?cwd=` (`links.rs`).
//!
//! `NSRequiredContext` (empty) in the plist is what makes a third-party
//! service show without a trip to the keyboard settings first.

use std::{path::PathBuf, sync::OnceLock};

use objc2::{
    AllocAnyThread, MainThreadMarker, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
};
use objc2_app_kit::{NSApplication, NSPasteboard, NSUpdateDynamicServices};
use objc2_foundation::{NSString, NSURL, ns_string};
use tauri::AppHandle;

static APP: OnceLock<AppHandle> = OnceLock::new();

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this has no Drop.
    #[unsafe(super(NSObject))]
    #[name = "ArugulaServices"]
    struct Services;

    impl Services {
        // SAFETY: the signature AppKit calls a service with.
        #[unsafe(method(newTabHere:userData:error:))]
        fn new_tab_here(&self, pboard: &NSPasteboard, _data: Option<&NSString>, _error: *mut *mut NSString) {
            let Some(app) = APP.get() else { return };
            let paths = paths(pboard);
            if paths.is_empty() {
                eprintln!("arugula: the service got no folder");
            }
            for p in paths {
                crate::links::open_dir(app, &p);
            }
        }
    }
);

/// The file URLs on the pasteboard, as paths. Finder sends file reference
/// URLs (`file:///.file/id=…`): NSURL resolves them to paths.
fn paths(pboard: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pboard.pasteboardItems() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| item.stringForType(ns_string!("public.file-url")))
        .filter_map(|s| NSURL::URLWithString(&s)?.filePathURL()?.path())
        .map(|p| PathBuf::from(p.to_string()))
        .collect()
}

/// Register the provider. On the main thread, at launch.
pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let Some(mtm) = MainThreadMarker::new() else { return };
    let provider: Retained<Services> = unsafe { msg_send![Services::alloc(), init] };
    let ns_app = NSApplication::sharedApplication(mtm);
    let obj: &AnyObject = &provider;
    unsafe { ns_app.setServicesProvider(Some(obj)) };
    // AppKit holds the provider weakly: keep it for the app's life.
    std::mem::forget(provider);
    // Tell the services list about this app's (pbs picks it up).
    NSUpdateDynamicServices();
}
