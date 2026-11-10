fn main() {
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    // The resource manifest is linked into binaries only. The test exe then
    // starts without Common Controls v6 and dies with STATUS_ENTRYPOINT_NOT_FOUND.
    // Embed the same manifest into every MSVC target, including tests.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let mut attrs = tauri_build::Attributes::new();
    if target_os == "windows" && target_env == "msvc" {
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        attrs = attrs.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
