fn main() {
    let manifest = tauri_build::AppManifest::new().commands(&[
        "pick_workspace",
        "desktop_notify",
        "desktop_initial_links",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to build Ternilo desktop metadata");
}
