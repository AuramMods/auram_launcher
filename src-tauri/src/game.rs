use crate::{
    auth::Credentials,
    jobs::Job,
    runtime::{self, Ready, Runtime},
    storage::{self, Settings},
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    fs,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub last_exit: Option<String>,
}
#[derive(Clone, Default)]
pub struct Game {
    pub status: Arc<Mutex<GameStatus>>,
}
impl Game {
    pub fn status(&self) -> GameStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn launch(
        &self,
        runtime: &Runtime,
        ready: &Ready,
        session: &Credentials,
        settings: &Settings,
        job: &Job,
    ) -> Result<()> {
        if self.status().running {
            bail!("Minecraft is already running");
        }
        job.check()?;
        job.phase("launch", "Launch", false);
        job.progress("launch", "Preparing natives and starting Minecraft", 0, 0);
        let arguments = runtime.arguments(ready, session, settings)?;
        let main = ready.forge["mainClass"]
            .as_str()
            .or_else(|| ready.vanilla["mainClass"].as_str())
            .context("Game main class missing")?;
        let main_position = arguments
            .iter()
            .position(|arg| arg == main)
            .context("Game main class argument missing")?;
        #[cfg(target_os = "macos")]
        let (arguments, main_position, glfw) = {
            let path = crate::mac_game_helper::prepare_glfw(runtime, ready)?;
            let mut arguments = arguments;
            arguments.insert(
                main_position,
                format!("-Dorg.lwjgl.glfw.libname={}", path.display()),
            );
            (arguments, main_position + 1, path)
        };
        let argument_path = runtime.layout.state("launch-jvm.args");
        storage::atomic_write(
            &argument_path,
            runtime::argfile(&arguments[..=main_position]).as_bytes(),
        )?;
        let log = runtime.layout.root.join("logs/game.log");
        let file = Arc::new(Mutex::new(fs::File::create(&log)?));
        let mut native_arguments = vec![format!("@{}", argument_path.display())];
        native_arguments.extend_from_slice(&arguments[main_position + 1..]);
        let (mut command, input) = crate::java_bridge::game_command(
            &ready.java,
            &native_arguments,
            &runtime.layout.root,
            #[cfg(target_os = "macos")]
            &glfw,
        )?;
        command
            .current_dir(runtime.layout.game())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn().context("Unable to start Minecraft")?;
        crate::java_bridge::feed(&mut child, input).await?;
        let stdout = child.stdout.take().context("Minecraft output missing")?;
        let stderr = child
            .stderr
            .take()
            .context("Minecraft error output missing")?;
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = GameStatus {
            running: true,
            pid: child.id(),
            last_exit: None,
        };
        let state = self.status.clone();
        let secrets = vec![
            session.minecraft_token.clone(),
            session.refresh_token.clone(),
        ];
        tokio::spawn(async move {
            let (status, one, two) = tokio::join!(
                child.wait(),
                runtime::copy_log(stdout, file.clone(), &secrets),
                runtime::copy_log(stderr, file.clone(), &secrets)
            );
            let message = match status {
                Ok(status) if status.success() => "Minecraft closed".into(),
                Ok(status) => {
                    format!("Minecraft exited with {status}. Open the game log for details.")
                }
                Err(error) => format!("Unable to monitor Minecraft: {error}"),
            };
            let message = if let Err(error) = one.and(two) {
                format!("{message} Log capture failed: {error}")
            } else {
                message
            };
            *state.lock().unwrap_or_else(|e| e.into_inner()) = GameStatus {
                running: false,
                pid: None,
                last_exit: Some(message),
            };
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = self.status();
        if !status.running {
            bail!("{}", status.last_exit.unwrap_or_default());
        }
        job.progress("launch", "Minecraft is running", 1, 1);
        Ok(())
    }
}
