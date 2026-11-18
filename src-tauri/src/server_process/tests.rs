use super::*;

#[tokio::test]
async fn launch_uses_the_deployed_version_memory_and_platform_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert!(deployed_arguments(root, Platform::MacArm, 12)
        .await
        .is_err());
    std::fs::write(
        root.join("auram.json"),
        r#"{"schema":1,"version":"161","minecraft":"1.20.1","forge":"47.4.10"}"#,
    )
    .unwrap();
    let forge = root.join("libraries/net/minecraftforge/forge/1.20.1-47.4.10");
    std::fs::create_dir_all(&forge).unwrap();
    for name in ["unix_args.txt", "win_args.txt"] {
        std::fs::write(forge.join(name), "forge.fixture.Main").unwrap();
    }
    std::fs::write(root.join("user_jvm_args.txt"), "-Xmx4G\n-Dflag=retained").unwrap();
    let (args, forge_file) = deployed_arguments(root, Platform::WindowsX64, 30)
        .await
        .unwrap();
    assert_eq!(
        args,
        [
            "@user_jvm_args.txt",
            "-Dfile.encoding=UTF-8",
            "-Dterminal.jline=false",
            "-Xms12G",
            "-Xmx30G",
        ],
    );
    assert_eq!(
        forge_file,
        "libraries/net/minecraftforge/forge/1.20.1-47.4.10/win_args.txt"
    );
    let (args, forge_file) = deployed_arguments(root, Platform::MacArm, 12)
        .await
        .unwrap();
    assert!(forge_file.ends_with("/unix_args.txt"));
    assert!(args.contains(&"-Xmx12G".into()));
    assert!(deployed_arguments(root, Platform::MacArm, 31)
        .await
        .is_err());
    std::fs::create_dir_all(root.join(".auram-deploy/lock")).unwrap();
    assert!(deployed_arguments(root, Platform::MacArm, 12)
        .await
        .unwrap_err()
        .to_string()
        .contains("Recover"));
}

#[test]
fn agreement_is_required_and_recorded_only_in_the_selected_folder() {
    let selected = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let path = selected.path().join("eula.txt");
    let unaccepted = "# Existing file\r\neula=false\r\n";
    std::fs::write(&path, unaccepted).unwrap();

    assert!(!ensure_eula(selected.path(), false).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), unaccepted);
    assert!(!ensure_eula(other.path(), false).unwrap());
    assert!(!other.path().join("eula.txt").exists());

    assert!(ensure_eula(selected.path(), true).unwrap());
    assert!(ensure_eula(selected.path(), false).unwrap());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .ends_with("eula=true\n"));
    assert!(!other.path().join("eula.txt").exists());
    assert!(ensure_eula(other.path(), true).unwrap());
    assert!(ensure_eula(other.path(), false).unwrap());

    let previously_accepted = "# Accepted outside Auram\r\neula=true\r\n";
    std::fs::write(&path, previously_accepted).unwrap();
    assert!(ensure_eula(selected.path(), true).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), previously_accepted);
}

#[cfg(unix)]
#[test]
fn agreement_does_not_follow_an_eula_symlink() {
    let selected = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "eula=false\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), selected.path().join("eula.txt")).unwrap();

    assert!(ensure_eula(selected.path(), true).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path()).unwrap(),
        "eula=false\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn owns_processes_sends_commands_drains_both_streams_and_restarts() {
    fn fixture() -> Command {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"
          printf 'ready\n'
          printf 'diagnostic\n' >&2
          while IFS= read -r command; do
            if [ "$command" = stop ]; then
              printf 'saved\n'
              exit 0
            fi
            printf 'received %s\n' "$command"
          done
        "#,
        ]);
        command
    }
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("Server 世界");
    std::fs::create_dir(&server).unwrap();
    let processes = ServerProcesses::default();
    processes.spawn("local", server.clone(), fixture()).unwrap();
    assert!(processes.active());
    assert!(processes.uses(temp.path()));
    assert!(processes.spawn("alias", server.clone(), fixture()).is_err());
    assert!(processes.forget_stopped("local").is_err());
    assert!(processes.send("local", "list\nstop").await.is_err());
    processes.send("local", "say 雪 with spaces").await.unwrap();
    processes.stop("local").await.unwrap();
    assert!(!processes.active());
    let first = processes.console("local", 0);
    assert!(first
        .iter()
        .any(|line| line.text == "received say 雪 with spaces"));
    assert!(first
        .iter()
        .any(|line| line.source == "stderr" && line.text == "diagnostic"));
    assert!(first.iter().any(|line| line.text == "saved"));
    assert_eq!(processes.status()[0].state, ServerState::Stopped);
    let cursor = first.last().unwrap().id;
    processes.spawn("local", server, fixture()).unwrap();
    processes.stop("local").await.unwrap();
    let second = processes.console("local", cursor);
    assert!(second.iter().all(|line| line.id > cursor));
    assert!(second.iter().any(|line| line.text == "saved"));
    processes.forget_stopped("local").unwrap();
    assert!(processes.status().is_empty());
}

#[tokio::test]
async fn console_trims_a_large_line_and_retains_only_recent_output() {
    let session = Arc::new(Session {
        directory: PathBuf::new(),
        data: Mutex::new(SessionData {
            state: ServerState::Stopped,
            exit: None,
            control: None,
            lines: VecDeque::new(),
        }),
    });
    let mut bytes = "short\n".repeat(MAX_LINES).into_bytes();
    bytes.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES * 2));
    bytes.extend_from_slice(b"\nfinal");
    read_output(bytes.as_slice(), session.clone(), "stdout").await;
    let data = session.data();
    assert_eq!(data.lines.len(), MAX_LINES);
    assert_eq!(data.lines.back().unwrap().text, "final");
    let long = &data.lines[MAX_LINES - 2].text;
    assert_eq!(long.chars().count(), MAX_LINE_BYTES + 1);
    assert!(long.ends_with('…'));
}
