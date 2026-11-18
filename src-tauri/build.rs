use std::process::Command;
fn main() {
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("Git is required for commit-count build metadata");
        assert!(output.status.success(), "Unable to read Git build metadata");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let sha = git(&["rev-parse", "HEAD"]);
    let count = git(&["rev-list", "--count", &sha]);
    println!("cargo:rustc-env=AURAM_BUILD_NUMBER={count}");
    println!("cargo:rustc-env=AURAM_COMMIT={sha}");
    let git_dir = git(&["rev-parse", "--absolute-git-dir"]);
    println!("cargo:rerun-if-changed={git_dir}/HEAD");
    println!("cargo:rerun-if-changed={git_dir}/refs");
    println!("cargo:rerun-if-changed={git_dir}/packed-refs");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=windows.manifest");
    println!("cargo:rerun-if-changed=icons/game.ico");
    println!("cargo:rerun-if-changed=native/macos");
    println!("cargo:rerun-if-changed=../assets/icon.png");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target == "macos" {
        let app = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("Auram.app");
        let result = Command::new("/bin/sh")
            .arg("native/macos/build-helper.sh")
            .arg(app)
            .status()
            .expect("Compile and sign the native Auram game helper");
        assert!(
            result.success(),
            "Unable to build the native Auram game helper"
        );
    }
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        let game_icon = root.join("src-tauri/icons/game.ico");
        let game_icon = game_icon.to_string_lossy().replace('\\', "/");
        tauri_build::try_build(
            tauri_build::Attributes::new().windows_attributes(
                tauri_build::WindowsAttributes::new()
                    .app_manifest(include_str!("windows.manifest"))
                    .append_rc_content(format!(r#"GLFW_ICON ICON "{game_icon}""#)),
            ),
        )
        .expect("Build native application resources");
        if target == "windows" {
            // The native QA examples can serve as JLI hosts too. Use the same
            // UTF-8 manifest and GLFW icon that Tauri linked into the launcher.
            println!(
                "cargo:rustc-link-arg-examples={}/resource.lib",
                std::env::var("OUT_DIR").unwrap()
            );
        }
    } else if target == "windows" {
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("windows.manifest")
                .display()
        );
    }
}
