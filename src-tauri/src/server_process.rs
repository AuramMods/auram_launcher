//! Owns launcher-started dedicated server processes and their bounded console buffers.
use crate::{
    deploy, java_bridge,
    jobs::Job,
    runtime::{Platform, Runtime},
    storage,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{mpsc, oneshot, watch},
};

const MAX_LINES: usize = 1200;
const MAX_LINE_BYTES: usize = 8192;
static NEXT_LINE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ServerState {
    Running,
    Stopping,
    Stopped,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    id: String,
    state: ServerState,
    exit: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleLine {
    id: u64,
    source: &'static str,
    text: String,
}

#[derive(Clone)]
struct Control {
    commands: mpsc::Sender<Request>,
    stopped: watch::Receiver<bool>,
}

struct Request {
    command: Option<String>,
    reply: oneshot::Sender<Result<(), String>>,
}

struct SessionData {
    state: ServerState,
    exit: Option<String>,
    control: Option<Control>,
    lines: VecDeque<ConsoleLine>,
}

struct Session {
    directory: PathBuf,
    data: Mutex<SessionData>,
}
impl Session {
    fn data(&self) -> std::sync::MutexGuard<'_, SessionData> {
        self.data.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn line(&self, source: &'static str, text: String) {
        let mut data = self.data();
        if data.lines.len() == MAX_LINES {
            data.lines.pop_front();
        }
        data.lines.push_back(ConsoleLine {
            id: NEXT_LINE.fetch_add(1, Ordering::Relaxed),
            source,
            text,
        });
    }
}

#[derive(Default)]
pub struct ServerProcesses {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
}
impl ServerProcesses {
    fn sessions(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Session>>> {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.sessions()
            .iter()
            .map(|(id, session)| {
                let data = session.data();
                ServerStatus {
                    id: id.clone(),
                    state: data.state,
                    exit: data.exit.clone(),
                }
            })
            .collect()
    }

    pub fn console(&self, id: &str, after: u64) -> Vec<ConsoleLine> {
        self.sessions().get(id).map_or_else(Vec::new, |session| {
            session
                .data()
                .lines
                .iter()
                .filter(|line| line.id > after)
                .cloned()
                .collect()
        })
    }

    pub fn active(&self) -> bool {
        self.sessions()
            .values()
            .any(|session| session.data().control.is_some())
    }

    pub fn running(&self, id: &str) -> bool {
        self.control(id).is_some()
    }

    pub fn uses(&self, directory: &Path) -> bool {
        self.sessions()
            .values()
            .any(|session| overlaps(session, directory))
    }

    fn control(&self, id: &str) -> Option<Control> {
        self.sessions().get(id)?.data().control.clone()
    }

    pub fn start(&self, id: &str, directory: PathBuf, java: &Path, args: &[String]) -> Result<()> {
        self.spawn(id, directory, java_bridge::interactive_command(java, args)?)
    }

    fn spawn(&self, id: &str, directory: PathBuf, mut command: Command) -> Result<()> {
        let mut sessions = self.sessions();
        if sessions
            .get(id)
            .is_some_and(|session| session.data().control.is_some())
            || sessions
                .values()
                .any(|session| overlaps(session, &directory))
        {
            bail!("A local server is already running in this folder");
        }
        command
            .current_dir(&directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let child = command.spawn().context("Start the dedicated server JVM")?;
        let session = sessions
            .get(id)
            .filter(|session| session.directory == directory)
            .cloned()
            .unwrap_or_else(|| {
                Arc::new(Session {
                    directory,
                    data: Mutex::new(SessionData {
                        state: ServerState::Stopped,
                        exit: None,
                        control: None,
                        lines: VecDeque::new(),
                    }),
                })
            });
        let (commands, requests) = mpsc::channel(16);
        let (finished, stopped) = watch::channel(false);
        {
            let mut data = session.data();
            data.state = ServerState::Running;
            data.exit = None;
            data.control = Some(Control { commands, stopped });
        }
        session.line("launcher", "Starting server…".into());
        sessions.insert(id.into(), session.clone());
        tokio::spawn(supervise(child, session, requests, finished));
        Ok(())
    }

    pub async fn send(&self, id: &str, command: &str) -> Result<()> {
        let command = command.trim();
        if command.is_empty() || command.len() > 4096 || command.contains(char::is_control) {
            bail!("Enter a single server command of up to 4096 bytes");
        }
        let control = self.control(id).context("The server is stopped")?;
        control.request(Some(command.into())).await
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        let Some(mut control) = self.control(id) else {
            return Ok(());
        };
        if let Err(error) = control.request(None).await {
            if !*control.stopped.borrow() {
                return Err(error);
            }
        }
        tokio::time::timeout(
            Duration::from_secs(90),
            control.stopped.wait_for(|stopped| *stopped),
        )
        .await
        .context("The server is still shutting down. Check its console for progress.")??;
        Ok(())
    }

    pub fn forget_stopped(&self, id: &str) -> Result<()> {
        let mut sessions = self.sessions();
        if sessions
            .get(id)
            .is_some_and(|session| session.data().control.is_some())
        {
            bail!("Stop the local server first");
        }
        sessions.remove(id);
        Ok(())
    }
}

fn overlaps(session: &Session, directory: &Path) -> bool {
    session.data().control.is_some()
        && (directory.starts_with(&session.directory) || session.directory.starts_with(directory))
}

impl Control {
    async fn request(&self, command: Option<String>) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(8), async {
            let (reply, response) = oneshot::channel();
            self.commands
                .send(Request { command, reply })
                .await
                .map_err(|_| anyhow::anyhow!("The server has exited"))?;
            response
                .await
                .context("The server has exited")?
                .map_err(anyhow::Error::msg)
        })
        .await
        .context("The server console is busy. Try again.")?
    }
}

async fn supervise(
    mut child: Child,
    session: Arc<Session>,
    mut requests: mpsc::Receiver<Request>,
    finished: watch::Sender<bool>,
) {
    let mut input = child.stdin.take().expect("piped server stdin");
    let stdout = child.stdout.take().expect("piped server stdout");
    let stderr = child.stderr.take().expect("piped server stderr");
    let mut stdout = tokio::spawn(read_output(stdout, session.clone(), "stdout"));
    let mut stderr = tokio::spawn(read_output(stderr, session.clone(), "stderr"));
    let mut stopping = false;
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = child.wait() => break outcome,
            Some(request) = requests.recv() => {
                if request.reply.is_closed() {
                    continue;
                }
                let wants_stop = request.command.is_none();
                let result = if stopping {
                    if wants_stop { Ok(()) } else { Err("The server is shutting down".into()) }
                } else {
                    let command = request.command.as_deref().unwrap_or("stop");
                    let line = format!("{command}\n");
                    match tokio::time::timeout(Duration::from_secs(5), input.write_all(line.as_bytes())).await {
                        Ok(Ok(())) => {
                            if wants_stop {
                                stopping = true;
                                session.data().state = ServerState::Stopping;
                                session.line("launcher", "Stopping server…".into());
                            } else {
                                session.line("command", format!("> {command}"));
                            }
                            Ok(())
                        }
                        Ok(Err(error)) => Err(format!("Write to the server console: {error}")),
                        Err(_) => Err("The server is not reading console commands yet".into()),
                    }
                };
                let _ = request.reply.send(result);
            }
        }
    };
    drop(child);
    if tokio::time::timeout(Duration::from_secs(3), async {
        let _ = tokio::join!(&mut stdout, &mut stderr);
    })
    .await
    .is_err()
    {
        stdout.abort();
        stderr.abort();
    }
    let exit = match outcome {
        Ok(status) if status.success() => "Server stopped".into(),
        Ok(status) => format!("Server exited with {status}"),
        Err(error) => format!("Wait for the server process: {error}"),
    };
    session.line("launcher", exit.clone());
    {
        let mut data = session.data();
        data.state = ServerState::Stopped;
        data.control = None;
        data.exit = Some(exit);
    }
    let _ = finished.send(true);
}

async fn read_output(output: impl AsyncRead + Unpin, session: Arc<Session>, source: &'static str) {
    let mut reader = BufReader::new(output);
    let mut line = Vec::new();
    let mut truncated = false;
    loop {
        let available = match reader.fill_buf().await {
            Ok(available) => available,
            Err(error) => {
                session.line("launcher", format!("Read server {source}: {error}"));
                break;
            }
        };
        if available.is_empty() {
            if !line.is_empty() {
                record_line(&session, source, &mut line, truncated);
            }
            break;
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let end = newline.unwrap_or(available.len());
        let keep = end.min(MAX_LINE_BYTES - line.len());
        line.extend_from_slice(&available[..keep]);
        truncated |= keep < end;
        reader.consume(end + usize::from(newline.is_some()));
        if newline.is_some() {
            record_line(&session, source, &mut line, truncated);
            truncated = false;
        }
    }
}

fn record_line(session: &Session, source: &'static str, line: &mut Vec<u8>, truncated: bool) {
    let mut text = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line)).into_owned();
    if truncated {
        text.push('…');
    }
    session.line(source, text);
    line.clear();
}

pub async fn launch_arguments(
    directory: &Path,
    runtime: &Runtime,
    memory_gib: u8,
    job: &Job,
) -> Result<Vec<String>> {
    let (mut arguments, forge_file) =
        deployed_arguments(directory, runtime.platform, memory_gib).await?;
    arguments.push(
        runtime
            .server_forge_argfile(directory, &forge_file, job)
            .await?,
    );
    arguments.push("nogui".into());
    Ok(arguments)
}

async fn deployed_arguments(
    directory: &Path,
    platform: Platform,
    memory_gib: u8,
) -> Result<(Vec<String>, String)> {
    use deploy::connection::Remote;
    let remote = deploy::local::LocalRemote::new(directory.to_path_buf())?;
    if remote.stat(".auram-deploy/lock").await?.is_some() {
        bail!("Recover the interrupted deployment before starting this server");
    }
    let version = deploy::read_version(&remote)
        .await?
        .context("Deploy Auram to this folder before starting the server")?;
    if version.minecraft != "1.20.1"
        || version.forge.is_empty()
        || !version
            .forge
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        bail!("Deploy Auram again to prepare the Forge launch files for this server");
    }
    let args_name = match platform {
        Platform::MacArm => "unix_args.txt",
        Platform::WindowsX64 => "win_args.txt",
    };
    let forge_args = format!(
        "libraries/net/minecraftforge/forge/1.20.1-{}/{args_name}",
        version.forge
    );
    std::fs::File::open(storage::safe_join(directory, &forge_args)?)
        .with_context(|| format!("Open the deployed Forge launch arguments at {forge_args}"))?;
    let eula = storage::safe_join(directory, "eula.txt")?;
    let accepted = match std::fs::read_to_string(eula) {
        Ok(text) => text
            .lines()
            .rev()
            .find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "eula").then(|| value.trim().eq_ignore_ascii_case("true"))
            })
            .unwrap_or(false),
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("Read the server's eula.txt"),
    };
    if !accepted {
        bail!("Read the Minecraft EULA at https://aka.ms/MinecraftEULA and set eula=true in the server folder's eula.txt");
    }
    crate::server_instances::validate_memory(memory_gib)?;
    let mut args = Vec::new();
    let user_args = storage::safe_join(directory, "user_jvm_args.txt")?;
    match std::fs::File::open(user_args) {
        Ok(_) => args.push("@user_jvm_args.txt".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Open the server's user_jvm_args.txt"),
    }
    args.extend([
        "-Dfile.encoding=UTF-8".into(),
        // Forge should read the UTF-8 stdin pipe instead of attaching JLine to a terminal.
        "-Dterminal.jline=false".into(),
        format!("-Xms{}G", crate::server_instances::MIN_SERVER_MEMORY),
        format!("-Xmx{memory_gib}G"),
    ]);
    Ok((args, forge_args))
}

#[cfg(test)]
mod tests;
