use anyhow::{bail, Context, Result};
use auram_core::{
    download::Downloader,
    jobs::Job,
    runtime::{self, Platform, Runtime},
    storage::{self, Layout},
};
use std::{fs, process::Command};

#[tokio::main]
async fn main() -> Result<()> {
    let layout = Layout::new(std::env::temp_dir().join("auram-runtime-verification"))?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
        platform: Platform::current()?,
    };
    let java = runtime.java(&Job::default()).await?;
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
    let compiler = java.with_file_name(if cfg!(windows) { "javac.exe" } else { "javac" });
    let result = Command::new(compiler)
        .args(["-encoding", "UTF-8"])
        .arg(&source)
        .output()?;
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
    let result = Command::new(&java)
        .arg(format!("@{}", file.display()))
        .output()
        .context("Execute Java argument file")?;
    if !result.status.success() {
        bail!(
            "Java argument-file execution failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    if fs::read_to_string(output)? != payload {
        bail!("Java did not preserve Unicode/quoted argument bytes");
    }
    println!("QA_EVT {{\"event\":\"java_argument_file\",\"status\":\"pass\",\"details\":\"Managed Java 17 executed Unicode paths, spaces, quotes, backslashes and a 40,000-character JVM argument\"}}");
    Ok(())
}
