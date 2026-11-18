use crate::runtime::{Ready, Runtime};
use crate::storage;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tokio::process::Command;

const DEV_APP: &[(&str, &[u8])] = &[
    (
        "Contents/Resources/Auram.png",
        include_bytes!("../../assets/icon.png"),
    ),
    (
        "Contents/MacOS/Auram",
        include_bytes!(concat!(env!("OUT_DIR"), "/Auram.app/Contents/MacOS/Auram")),
    ),
    (
        "Contents/Info.plist",
        include_bytes!(concat!(env!("OUT_DIR"), "/Auram.app/Contents/Info.plist")),
    ),
    (
        "Contents/Resources/Auram.icns",
        include_bytes!(concat!(
            env!("OUT_DIR"),
            "/Auram.app/Contents/Resources/Auram.icns"
        )),
    ),
    (
        "Contents/_CodeSignature/CodeResources",
        include_bytes!(concat!(
            env!("OUT_DIR"),
            "/Auram.app/Contents/_CodeSignature/CodeResources"
        )),
    ),
];
const EXECUTABLE: &str = "Contents/MacOS/Auram";

pub(super) fn prepare_glfw(runtime: &Runtime, ready: &Ready) -> Result<PathBuf> {
    use std::io::Read;
    const NATIVE: &str = "macos/arm64/org/lwjgl/glfw/libglfw.dylib";
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    // Select the same classifier that the managed client puts on its classpath.
    let (library, coordinate) = [&ready.forge, &ready.vanilla]
        .into_iter()
        .flat_map(|meta| meta["libraries"].as_array().into_iter().flatten().rev())
        .filter_map(|lib| Some((lib, lib["name"].as_str()?)))
        .find(|(_, name)| {
            name.starts_with("org.lwjgl:lwjgl-glfw:") && name.ends_with(":natives-macos-arm64")
        })
        .context("The managed client's ARM64 GLFW library is missing")?;
    let jar = library["downloads"]["artifact"]["path"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or(crate::runtime::maven_path(coordinate)?);
    let root = runtime.layout.runtime();
    let jar = storage::safe_join(&root.join("libraries"), &jar)?;
    let mut archive = zip::ZipArchive::new(fs::File::open(jar)?)?;
    let mut bytes = Vec::new();
    archive
        .by_name(NATIVE)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "Managed GLFW library exceeds its size limit"
    );
    let identity = hex::encode(Sha256::digest(&bytes));
    let native = storage::safe_join(
        &root,
        &format!("natives/macos-arm64/auram-glfw/{identity}/libglfw.dylib"),
    )?;
    if fs::read(&native).ok().as_deref() != Some(bytes.as_slice()) {
        storage::atomic_write(&native, &bytes)?;
    }
    Ok(native)
}

fn helper(data: &Path) -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    if let Some(contents) = executable
        .parent()
        .filter(|directory| directory.file_name().is_some_and(|name| name == "MacOS"))
        .and_then(Path::parent)
        .filter(|directory| directory.file_name().is_some_and(|name| name == "Contents"))
    {
        // Resolve relative to the launcher bundle so a relocated app still
        // uses its own signed helper.
        let helper = contents.join("Helpers/Auram.app").join(EXECUTABLE);
        fs::File::open(&helper).context("Open the bundled Auram game application")?;
        return Ok(helper);
    }

    // Standalone Cargo and development binaries materialize the same signed
    // bundle under a content key. New builds never replace a running helper.
    let mut hash = Sha256::new();
    for (path, bytes) in DEV_APP {
        hash.update(path);
        hash.update(bytes);
    }
    let relative = format!(
        "cache/game-helper/{}/Auram.app",
        hex::encode(hash.finalize())
    );
    let bundle = storage::safe_join(data, &relative)?;
    for (relative, bytes) in DEV_APP {
        let target = storage::safe_join(&bundle, relative)?;
        if fs::read(&target).ok().as_deref() != Some(bytes) {
            storage::atomic_write(&target, bytes)?;
        }
        if *relative == EXECUTABLE {
            fs::set_permissions(target, fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(bundle.join(EXECUTABLE))
}

pub(super) fn command(
    java: &Path,
    arguments: &[String],
    data: &Path,
    glfw: &Path,
) -> Result<(Command, Option<Vec<u8>>)> {
    let home = java
        .parent()
        .and_then(Path::parent)
        .context("Managed Java home missing")?;
    let library = home.join("lib/libjli.dylib");
    let mut command = Command::new(helper(data)?);
    command.arg("--auram-game-pipe");
    let input = serde_json::to_vec(&serde_json::json!({
        "library": library,
        "glfw": glfw,
        "arguments": arguments,
    }))?;
    Ok((command, Some(input)))
}
