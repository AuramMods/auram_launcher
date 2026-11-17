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
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        tauri_build::build();
    }
}
