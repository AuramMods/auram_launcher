use anyhow::{bail, Context, Result};
use auram_core::{
    download::Downloader,
    jobs::Job,
    runtime::{self, Platform, Runtime},
    storage::{self, Layout},
};
use std::fs;

#[tokio::main]
async fn main() -> Result<()> {
    auram_core::java_bridge::entry();
    let layout = Layout::new(
        std::env::var_os("AURAM_VERIFY_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("auram-runtime-verification")),
    )?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
        platform: Platform::current()?,
    };
    let java = runtime
        .java(&Job::default())
        .await
        .context("Provision managed Java")?;
    let temp = tempfile::tempdir()?;
    let classes = temp.path().join("argument path with spaces 世界");
    fs::create_dir_all(&classes)?;
    let source = classes.join("EchoArgs.java");
    fs::write(
        &source,
        r#"import java.nio.file.*; import java.nio.charset.StandardCharsets;
public class EchoArgs { public static void main(String[] args) throws Exception {
 if (System.getProperty("auram.long").length()!=40000) throw new Exception("Long JVM argument truncated");
 Files.writeString(Path.of(args[0]), args[1], StandardCharsets.UTF_8);
} }"#,
    )?;
    let result = execute(
        &java,
        &[
            "-m".into(),
            "jdk.compiler/com.sun.tools.javac.Main".into(),
            "-encoding".into(),
            "UTF-8".into(),
            source.to_string_lossy().into_owned(),
        ],
    )
    .await?;
    if !result.status.success() {
        bail!(
            "Java fixture compilation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let output = classes.join("result 雪.txt");
    let payload = "Unicode 雪 and spaces, quotes \" and backslash \\";
    let arguments = vec![
        format!("-Dauram.long={}", "x".repeat(40000)),
        "-cp".into(),
        classes.to_string_lossy().into(),
        "EchoArgs".into(),
        output.to_string_lossy().into(),
        payload.into(),
    ];
    let file = temp.path().join("launch args.txt");
    storage::atomic_write(&file, runtime::argfile(&arguments).as_bytes())?;
    let result = execute(&java, &[format!("@{}", file.display())]).await?;
    if !result.status.success() {
        bail!(
            "Java argument-file execution failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    if fs::read_to_string(output)? != payload {
        bail!("Java did not preserve Unicode/quoted argument bytes");
    }
    #[cfg(windows)]
    {
        let result = execute(&java, &arguments).await?;
        if !result.status.success() {
            bail!(
                "Java private-pipe launch failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        // The same CLI mode is used by generated server startup scripts.
        let helper = if std::env::args_os().nth(1).is_some() {
            java.with_file_name("auram-verify-packaged.exe")
        } else {
            auram_core::java_bridge::helper(&java)?
        };
        let result = tokio::process::Command::new(helper)
            .arg("--auram-java-cli")
            .arg(format!("@{}", file.display()))
            .output()
            .await?;
        if !result.status.success() {
            bail!(
                "Java server CLI launch failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    println!("QA_EVT {{\"event\":\"java_argument_file\",\"status\":\"pass\",\"details\":\"Managed Java 17 executed Unicode paths, spaces, quotes, backslashes and a 40,000-character JVM argument\"}}");
    Ok(())
}

async fn execute(java: &std::path::Path, arguments: &[String]) -> Result<std::process::Output> {
    let (mut command, mut input) = auram_core::java_bridge::command(java, arguments)?;
    #[cfg(windows)]
    if let Some(launcher) = std::env::args_os().nth(1) {
        // Exercise the actual GUI-subsystem executable and its packaged manifest.
        let helper = java.with_file_name("auram-verify-packaged.exe");
        storage::atomic_copy(std::path::Path::new(&launcher), &helper)?;
        command = tokio::process::Command::new(helper);
        command.arg("--auram-java-pipe");
        input = Some(serde_json::to_vec(arguments)?);
    }
    #[cfg(not(windows))]
    let _ = &mut input;
    command
        .stdin(if input.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().context("Execute managed Java")?;
    auram_core::java_bridge::feed(&mut child, input).await?;
    Ok(child.wait_with_output().await?)
}
