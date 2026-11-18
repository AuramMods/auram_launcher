//! Optional Git selected explicitly by the user. The bundled transport owns
//! validation and the object cache, regardless of which transport fills it.
use crate::jobs::Job;
use anyhow::{bail, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
};

pub(super) fn configured() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("AURAM_PACK_GIT")?);
    if !path.is_absolute() {
        return None;
    }
    // macOS's developer-tools shim can open an installer dialog on a clean Mac.
    #[cfg(target_os = "macos")]
    if std::fs::canonicalize(&path).ok().as_deref() == Some(Path::new("/usr/bin/git")) {
        return None;
    }
    Some(path)
}

pub(super) async fn fetch(
    executable: &Path,
    directory: &Path,
    remote: &str,
    oids: &[String],
    metadata: bool,
    job: &Job,
) -> Result<()> {
    job.check()?;
    let mut command = Command::new(executable);
    // Keep system and user credential helpers, but do not inherit repository
    // redirection, object alternates, tracing, or interactive prompt programs.
    for (name, _) in std::env::vars_os() {
        let key = name.to_string_lossy().to_ascii_uppercase();
        if key.starts_with("GIT_TRACE")
            || key.starts_with("GCM_TRACE")
            || matches!(
                key.as_str(),
                "GIT_CONFIG_PARAMETERS"
                    | "GIT_CONFIG_COUNT"
                    | "GIT_DIR"
                    | "GIT_COMMON_DIR"
                    | "GIT_WORK_TREE"
                    | "GIT_INDEX_FILE"
                    | "GIT_NAMESPACE"
                    | "GIT_QUARANTINE_PATH"
                    | "GIT_OBJECT_DIRECTORY"
                    | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                    | "GIT_CURL_VERBOSE"
                    | "GIT_SSL_NO_VERIFY"
                    | "GIT_SHALLOW_FILE"
                    | "GIT_EXEC_PATH"
                    | "GIT_CONFIG"
            )
        {
            command.env_remove(name);
        }
    }
    #[cfg(test)]
    let protocols = "https:http"; // loopback smart-HTTP fixtures
    #[cfg(not(test))]
    let protocols = "https";
    command
        .current_dir(directory)
        .env("GIT_ALLOW_PROTOCOL", protocols)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("--git-dir")
        .arg(directory)
        .arg("-c")
        .arg("protocol.version=2")
        .arg("-c")
        .arg("credential.interactive=false")
        .arg("-c")
        .arg("core.askPass=")
        .arg("-c")
        .arg("http.sslVerify=true")
        .arg("-c")
        .arg("http.lowSpeedLimit=1")
        .arg("-c")
        .arg("http.lowSpeedTime=45")
        .arg("-c")
        .arg("fetch.fsckObjects=true")
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            directory.join("disabled-hooks").display()
        ))
        .arg("-c")
        .arg("gc.auto=0")
        .arg("-c")
        .arg(format!("remote.auram-pack.url={remote}"))
        .arg("-c")
        .arg("remote.auram-pack.promisor=true")
        .arg("-c")
        .arg("remote.auram-pack.partialclonefilter=blob:none")
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "--filter=blob:none",
            "--stdin",
        ]);
    if metadata {
        command.arg("--depth=1");
    }
    command
        .arg("auram-pack")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .context("Could not start the configured Git executable")?;
    let tree = ProcessTree::attach(&child)?;
    let mut input = child.stdin.take().context("Git input is unavailable")?;
    let result = tokio::select! {
        _ = job.cancel.cancelled() => Err(anyhow::anyhow!("Operation cancelled")),
        result = tokio::time::timeout(Duration::from_secs(120), async {
            input.write_all(oids.join("\n").as_bytes()).await?;
            input.write_all(b"\n").await?;
            drop(input);
            Ok::<_, anyhow::Error>(child.wait().await?)
        }) => result.context("Configured Git fetch timed out").and_then(|result| result),
    };
    match result {
        Ok(status) if status.success() => {
            tree.finish();
            Ok(())
        }
        Ok(status) => bail!("Configured Git fetch failed ({status})"),
        Err(error) => {
            drop(tree);
            let _ = child.kill().await;
            Err(error)
        }
    }
}

#[cfg(unix)]
struct ProcessTree(Option<i32>);
#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &Child) -> Result<Self> {
        let id = child
            .id()
            .context("Git exited before its process group could be recorded")?;
        Ok(Self(Some(i32::try_from(id)?)))
    }
    fn finish(mut self) {
        self.0 = None;
    }
}
#[cfg(unix)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            // The Git child was created as leader of its own group.
            unsafe {
                libc::kill(-id, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
struct ProcessTree(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &Child) -> Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::*;
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error()).context("Create Git process job");
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let child = child
            .raw_handle()
            .context("Git process handle is unavailable")?;
        unsafe {
            if SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
                || AssignProcessToJobObject(handle.as_raw_handle(), child) == 0
            {
                return Err(std::io::Error::last_os_error()).context("Limit Git process lifetime");
            }
        }
        Ok(Self(handle))
    }
    fn finish(self) {
        drop(self.0);
    }
}
