fn main() {
    // The app's own commands, so capabilities can grant them to pages
    // (capabilities/default.json): the daemon's page calls the huddle ones
    // (M63), the app's own pages the rest.
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
        "daemon_status",
        "retry",
        "compat",
        "compat_fix",
        "cloud_status",
        "cloud_signin",
        "cloud_local",
        "call_native_start",
        "call_native_peer",
        "call_native_remote",
        "call_native_drop",
        "call_native_mute",
        "call_native_stop",
        "call_native_status",
    ])))
    .expect("tauri-build");
}
