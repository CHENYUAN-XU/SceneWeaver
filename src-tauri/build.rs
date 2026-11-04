#[cfg(target_os = "windows")]
fn prepare_webview2_loader() {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")))
        .expect("CARGO_HOME or USERPROFILE is required to locate WebView2Loader.dll");
    let registry_sources = cargo_home.join("registry").join("src");
    let loader = fs::read_dir(&registry_sources)
        .expect("Cargo registry sources are unavailable; run cargo fetch before building")
        .filter_map(Result::ok)
        .flat_map(|index| {
            fs::read_dir(index.path())
                .into_iter()
                .flat_map(|entries| entries.filter_map(Result::ok))
        })
        .map(|crate_dir| crate_dir.path().join("x64").join("WebView2Loader.dll"))
        .find(|candidate| {
            candidate.is_file() && candidate.to_string_lossy().contains("webview2-com-sys-")
        })
        .expect("webview2-com-sys x64 WebView2Loader.dll is missing from the Cargo registry");

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let destination = manifest_dir
        .join("target")
        .join("release")
        .join("WebView2Loader.dll");
    fs::create_dir_all(
        destination
            .parent()
            .expect("WebView2Loader destination must have a parent"),
    )
    .expect("failed to create WebView2Loader build directory");
    fs::copy(&loader, &destination)
        .expect("failed to stage WebView2Loader.dll for Tauri resources");
    println!("cargo:rerun-if-changed={}", loader.display());
}

#[cfg(not(target_os = "windows"))]
fn prepare_webview2_loader() {}

fn main() {
    // Tauri validates bundle resources during every Cargo build, including a
    // clean `cargo test --no-run`. Stage the exact x64 loader from the locked
    // webview2-com-sys dependency before that validation, so packaging is
    // reproducible and the installed EXE keeps its adjacent loader DLL.
    prepare_webview2_loader();
    tauri_build::build();
    #[cfg(target_os = "windows")]
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        prepare_gnu_test_resources();
    }
}

#[cfg(target_os = "windows")]
fn prepare_gnu_test_resources() {
    use std::path::PathBuf;
    use std::process::Command;

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    // Tauri's GNU resource is a COFF object despite the .a extension. Only
    // binary targets receive it automatically, so wrap it for the lib tests.
    let archive = output.join("libsceneweaver_test_resources.a");
    let status = Command::new("ar")
        .arg("crs")
        .arg(&archive)
        .arg(output.join("libresource.a"))
        .status()
        .expect("start MinGW ar");
    assert!(
        status.success(),
        "archive the Windows manifest for lib tests"
    );
    println!("cargo:rustc-link-search=native={}", output.display());
}
