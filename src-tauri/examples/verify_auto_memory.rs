//! Headless check of the actual automatically selected heap on managed Java 17.
use anyhow::{ensure, Result};
use auram_core::{
    download::Downloader,
    jobs::Job,
    launch_options,
    runtime::{self, Platform, Runtime},
    storage::Layout,
};

#[tokio::main]
async fn main() -> Result<()> {
    auram_core::java_bridge::entry();
    let layout = Layout::new(
        std::env::args_os()
            .nth(1)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("auram-runtime-verification")),
    )?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
        platform: Platform::current()?,
    };
    let job = Job::default();
    let java = runtime.java(&job).await?;
    let memory = launch_options::automatic_memory();
    let mut arguments = launch_options::jvm_arguments(memory.maximum_mib)?;
    arguments.extend(["-XX:+PrintFlagsFinal".into(), "-version".into()]);
    let log = runtime.layout.root.join("logs/automatic-memory-probe.log");
    runtime::run_logged(&java, &arguments, &runtime.layout.root, &log, &job, &[]).await?;
    let text = std::fs::read_to_string(log)?;
    for (name, expected) in [
        ("MaxHeapSize", memory.maximum_mib * 1024 * 1024),
        ("InitialHeapSize", memory.initial_mib * 1024 * 1024),
    ] {
        let actual = text.lines().find_map(|line| {
            let words = line.split_whitespace().collect::<Vec<_>>();
            let index = words.iter().position(|word| *word == name)?;
            words.get(index + 2)?.parse::<u64>().ok()
        });
        ensure!(
            actual == Some(expected),
            "{name} was {actual:?}, expected {expected}"
        );
    }
    println!("PASS: {} preset; {} MiB maximum, {} MiB initial; detected RAM {:?} MiB; managed Java accepted original G1 flags",memory.profile,memory.maximum_mib,memory.initial_mib,memory.system_memory_mib);
    Ok(())
}
