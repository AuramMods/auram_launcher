//! Headless launch controller for the native GLFW fixture, using Game::launch.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use anyhow::{Context, Result};
    use auram_core::{
        auth::{Credentials, Profile},
        download::Downloader,
        game::Game,
        jobs::Job,
        runtime::{Platform, Ready, Runtime, Versions},
        storage::{self, Layout, Settings},
    };
    use serde_json::json;
    use std::{fs, path::PathBuf, time::Duration};

    auram_core::java_bridge::entry();
    let mut args = std::env::args_os().skip(1);
    let mut path = || -> Result<PathBuf> {
        Ok(PathBuf::from(args.next().context(
            "Pass new-data-dir, managed-java and fixture-dir",
        )?))
    };
    let root = path()?;
    let java = path()?;
    let fixture = path()?;
    let detach = args.next().is_some_and(|a| a == "--detach");
    fs::create_dir(&root).context("Use a new isolated data directory")?;
    let layout = Layout::new(root.clone())?;
    let runtime = Runtime {
        net: Downloader::new(&layout)?,
        layout,
        platform: Platform::current()?,
    };
    for item in ["branding.json", "icon.png", "auram-bootstrap.jar"] {
        storage::atomic_copy(
            &fixture.join("branding").join(item),
            &runtime.layout.game().join("branding").join(item),
        )?;
    }
    if runtime.platform == Platform::MacArm {
        let folder = "org/lwjgl/lwjgl-glfw/3.3.1";
        let native = "lwjgl-glfw-3.3.1-natives-macos-arm64.jar";
        storage::atomic_copy(
            &fixture.join("libraries").join(native),
            &runtime
                .layout
                .runtime()
                .join("libraries")
                .join(folder)
                .join(native),
        )?;
    }
    let mut classpath = vec![fixture.join("classes")];
    for entry in fs::read_dir(fixture.join("libraries"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "jar") {
            classpath.push(path);
        }
    }
    let classpath = std::env::join_paths(classpath)?;
    let report = root.join("Report 雪.txt");
    let stop = root.join("stop");
    let ready = Ready {
        java,
        versions: Versions {
            minecraft: "1.20.1".into(),
            forge: "47.4.10".into(),
        },
        vanilla: json!({
            "mainClass": "AuramProbe",
            "libraries": [{"name": "org.lwjgl:lwjgl-glfw:3.3.1:natives-macos-arm64"}],
            "arguments": {
                "jvm": ["-classpath", classpath.to_string_lossy()],
                "game": [report, stop, "true", "Unicode 雪 with spaces, quotes \" and backslash \\",
                    runtime.layout.game().join("branding/icon.png"), "${auth_access_token}"],
            },
        }),
        forge: json!({}),
    };
    let credentials = Credentials {
        minecraft_token: "qa-only-dummy-token".into(),
        profile: Some(Profile {
            name: "AuramQA".into(),
            uuid: "c7dad607797a4dd7b7f41d6d187aa2bc".into(),
            xuid: "0".into(),
        }),
        ..Credentials::default()
    };
    let game = Game::default();
    game.launch(
        &runtime,
        &ready,
        &credentials,
        &Settings::default(),
        &Job::default(),
    )
    .await?;
    let pid = game.status().pid.context("Missing running game PID")?;
    let contents = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            match fs::read_to_string(&report) {
                Ok(contents) => break Ok(contents),
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && game.status().running =>
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => break Err(error),
            }
        }
    })
    .await??;
    anyhow::ensure!(
        contents.contains(&format!("pid={pid}")),
        "GameStatus tracks the wrong process"
    );
    println!("{contents}");
    if !detach {
        fs::write(stop, b"stop")?;
        tokio::time::timeout(Duration::from_secs(15), async {
            while game.status().running {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await?;
        anyhow::ensure!(game
            .status()
            .last_exit
            .is_some_and(|exit| exit.contains("23")));
        let log = fs::read_to_string(root.join("logs/game.log"))?;
        anyhow::ensure!(log.contains("probe stdout") && log.contains("probe stderr"));
        anyhow::ensure!(
            !log.contains("qa-only-dummy-token"),
            "Game log contains a credential"
        );
    }
    Ok(())
}
