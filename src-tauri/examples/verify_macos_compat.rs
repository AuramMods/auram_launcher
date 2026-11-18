//! Headless live regression for the macOS JNA abort and Auram heap profiles.
use anyhow::{bail, Context, Result};
use auram_core::{
    auth::{Credentials, Profile},
    download::Downloader,
    jobs::Job,
    launch_options,
    runtime::{self, Platform, Runtime, Versions},
    storage::{self, Layout, Settings},
};
use std::fs;

#[tokio::main]
async fn main() -> Result<()> {
    let platform = Platform::current()?;
    if platform != Platform::MacArm {
        bail!("This regression exercises macOS ARM64 native libraries");
    }
    let layout = Layout::new(
        std::env::var_os("AURAM_VERIFY_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("auram-runtime-verification")),
    )?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
        platform,
    };
    let job = Job::default();
    let ready = runtime
        .ensure(
            Versions {
                minecraft: "1.20.1".into(),
                forge: "47.4.10".into(),
            },
            &job,
        )
        .await?;
    println!(
        "Runtime download body bytes: {}",
        runtime.net.transferred_bytes()
    );
    // These are explicit argument-generation fixtures, never used to sign in or launch a game.
    let session = Credentials {
        profile: Some(Profile {
            name: "Fixture".into(),
            uuid: "fixture".into(),
            xuid: "fixture".into(),
        }),
        ..Default::default()
    };
    let args = runtime.arguments(&ready, &session, &Settings::default())?;
    let classpath = &args[args
        .iter()
        .position(|a| a == "-cp")
        .context("Classpath missing")?
        + 1];
    let temp = tempfile::tempdir()?;
    let classes = temp.path().join("native probe with spaces 世界");
    fs::create_dir_all(&classes)?;
    let source = classes.join("MacCompatibilityProbe.java");
    fs::write(
        &source,
        r#"
import com.sun.jna.Native;
import com.sun.jna.NativeLibrary;
import oshi.SystemInfo;
public class MacCompatibilityProbe {
  public static void main(String[] args) {
    if (!System.getProperty("os.arch").equals("aarch64")) throw new AssertionError("Native ARM64 required");
    if (!Native.VERSION.equals("5.13.0")) throw new AssertionError("Unexpected JNA " + Native.VERSION);
    // Failed dlopen diagnostics longer than 1024 bytes aborted JNA 5.12.1 inside LOAD_ERROR.
    String missing = "/tmp/" + "auram missing library path 世界/".repeat(90) + "libMissingAuram.dylib";
    try {
      NativeLibrary.getInstance(missing);
      throw new AssertionError("Nonexistent library loaded");
    } catch (UnsatisfiedLinkError expected) {
      if (expected.getMessage().length() <= 1024) throw new AssertionError("Long-error regression was not exercised");
    }
    var hardware = new SystemInfo().getHardware();
    if (hardware.getMemory().getTotal() <= 0) throw new AssertionError("Native memory query failed");
    if (hardware.getProcessor().getLogicalProcessorCount() <= 0) throw new AssertionError("Native CPU query failed");
    System.out.println("PASS: Zulu " + System.getProperty("java.version") + " ARM64, JNA " + Native.VERSION + ", long dlopen error, native CPU/memory queries");
  }
}
"#,
    )?;
    runtime::run_logged(
        &ready.java,
        &[
            "-cp".into(),
            classpath.clone(),
            "-m".into(),
            "jdk.compiler/com.sun.tools.javac.Main".into(),
            "-encoding".into(),
            "UTF-8".into(),
            source.to_string_lossy().into(),
        ],
        &classes,
        &runtime.layout.root.join("logs/macos-probe-compile.log"),
        &job,
        &[],
    )
    .await?;
    for memory in [4096, 9216, 11264, 12288, 20480] {
        let mut arguments = launch_options::jvm_arguments(memory)?;
        arguments.extend([
            "-XstartOnFirstThread".into(),
            "-Djava.awt.headless=true".into(),
            "-Djna.nosys=true".into(),
            "-cp".into(),
            format!("{}:{classpath}", classes.display()),
            "MacCompatibilityProbe".into(),
        ]);
        let file = classes.join("launch args.txt");
        storage::atomic_write(&file, runtime::argfile(&arguments).as_bytes())?;
        let log = runtime
            .layout
            .root
            .join(format!("logs/macos-probe-{memory}.log"));
        runtime::run_logged(
            &ready.java,
            &[format!("@{}", file.display())],
            &classes,
            &log,
            &job,
            &[],
        )
        .await
        .with_context(|| format!("macOS compatibility with {memory} MiB heap"))?;
        let output = fs::read_to_string(log)?;
        println!(
            "{memory} MiB: {}",
            output
                .lines()
                .find(|line| line.starts_with("PASS:"))
                .context("Probe did not report success")?
        );
    }
    Ok(())
}
