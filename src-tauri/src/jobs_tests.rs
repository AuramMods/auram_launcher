use super::*;
fn job() -> Job {
    Job::new(
        "Install",
        &[
            PhaseSpec("files", "Pack files", true),
            PhaseSpec("apply", "Apply files", false),
        ],
        Arc::new(|_| {}),
    )
}
fn batch(job: &Job) -> BatchProgress {
    job.flush();
    job.snapshot().phases[job.snapshot().active_phase]
        .batch
        .clone()
        .unwrap()
}
#[test]
fn interleaved_transfers_and_retries_never_inflate_verified_progress() {
    let job = job();
    job.plan_downloads(
        "Pack files",
        &[
            ("small".into(), "small.jar".into(), Some(100)),
            ("large".into(), "large.jar".into(), Some(900)),
        ],
    );
    let mut small = job.task("small", "small.jar", Some(100));
    let mut large = job.task("large", "large.jar", Some(900));
    small.transfer(50, Some(100), 50);
    large.transfer(900, Some(900), 900);
    large.state(TaskState::Verifying, 1);
    assert_eq!(batch(&job).percent, 0.0); // Received bytes are not verified bytes.
    small.complete(false);
    assert_eq!(batch(&job).percent, 10.0);
    large.state(TaskState::Retrying, 1);
    large.transfer(100, Some(900), 100);
    assert_eq!(batch(&job).percent, 10.0);
    large.transfer(900, Some(900), 800);
    large.complete(false);
    let complete = batch(&job);
    assert_eq!(complete.percent, 100.0);
    assert_eq!(complete.ready_bytes, 1000);
    assert_eq!(job.snapshot().downloaded_bytes, 1850);
    job.phase("apply", "Apply files", false);
    assert_eq!(job.snapshot().phases[0].state, PhaseState::Complete);
    assert_eq!(job.snapshot().phases[1].percent, None);
    job.finish(None);
    assert_eq!(job.snapshot().state, JobState::Complete);
}
#[test]
fn unknown_sizes_keep_a_fixed_file_basis_and_cached_content_is_not_network_traffic() {
    let job = job();
    job.plan_downloads(
        "Libraries",
        &[
            ("one".into(), "one.jar".into(), Some(1)),
            ("two".into(), "two.jar".into(), None),
        ],
    );
    let mut one = job.task("one", "one.jar", Some(1));
    let mut two = job.task("two", "two.jar", None);
    one.complete(true);
    assert_eq!(batch(&job).percent, 50.0);
    two.transfer(900, Some(1000), 100);
    let late_total = batch(&job);
    assert_eq!(late_total.basis, "files");
    assert_eq!(late_total.percent, 50.0);
    two.complete(false);
    let done = batch(&job);
    assert_eq!(done.cached, 1);
    assert_eq!(done.percent, 100.0);
    assert_eq!(job.snapshot().downloaded_bytes, 100);
}
#[test]
fn progress_is_coalesced_without_losing_counts_and_empty_batches_complete() {
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let sink = snapshots.clone();
    let job = Job::new(
        "Install",
        &[PhaseSpec("files", "Files", true)],
        Arc::new(move |p| sink.lock().unwrap().push(p)),
    );
    job.plan_downloads("Files", &[("a".into(), "a.jar".into(), Some(1000))]);
    // Freeze the emission window so this remains deterministic on slow machines.
    job.tracker.lock().unwrap().last_emit = Instant::now() + Duration::from_secs(60);
    let mut task = job.task("a", "a.jar", Some(1000));
    for bytes in 1..=1000 {
        task.transfer(bytes, Some(1000), 1);
    }
    assert_eq!(snapshots.lock().unwrap().len(), 1);
    task.complete(false);
    job.flush();
    assert_eq!(snapshots.lock().unwrap().len(), 2);
    assert_eq!(job.snapshot().downloaded_bytes, 1000);
    job.plan_downloads("Nothing to download", &[]);
    assert_eq!(batch(&job).percent, 100.0);
}
#[tokio::test]
async fn a_stalled_transfer_still_emits_its_last_update() {
    let job = job();
    job.plan_downloads("Files", &[("a".into(), "a.jar".into(), Some(100))]);
    let task = job.task("a", "a.jar", Some(100));
    task.transfer(25, Some(100), 25);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job.snapshot().tasks[0].bytes, 25);
    assert_eq!(job.snapshot().tasks[0].state, TaskState::Downloading);
}
#[test]
fn late_file_updates_cannot_replace_the_current_phase_message() {
    let job = job();
    job.plan_downloads("Files", &[("a".into(), "a.jar".into(), Some(100))]);
    let mut task = job.task("a", "a.jar", Some(100));
    task.complete(false);
    job.phase("apply", "Apply files", false);
    job.progress("apply", "Activating verified files", 0, 0);
    task.transfer(100, Some(100), 0);
    job.flush();
    let snapshot = job.snapshot();
    assert_eq!(snapshot.active_phase, 1);
    assert_eq!(snapshot.message, "Activating verified files");
    assert_eq!(snapshot.phases[0].percent, Some(100.0));
}
#[test]
fn failure_cancellation_and_success_are_distinct_and_job_exclusion_is_preserved() {
    let jobs = Jobs::default();
    let lease = jobs
        .begin(
            "Install",
            &[PhaseSpec("files", "Files", true)],
            Arc::new(|_| {}),
        )
        .unwrap();
    assert!(jobs.begin("Other", &[], Arc::new(|_| {})).is_err());
    jobs.cancel();
    assert_eq!(lease.job.snapshot().state, JobState::Cancelling);
    lease.job.finish(Some("cancelled".into()));
    assert_eq!(lease.job.snapshot().state, JobState::Cancelled);
    let previous_id = lease.job.snapshot().job_id;
    drop(lease);
    assert!(!jobs.busy());
    let lease = jobs
        .begin(
            "Retry",
            &[PhaseSpec("files", "Files", true)],
            Arc::new(|_| {}),
        )
        .unwrap();
    assert!(lease.job.snapshot().job_id > previous_id);
    let mut task = lease.job.task("a", "a.jar", Some(100));
    task.fail("checksum mismatch");
    lease.job.finish(Some("checksum mismatch".into()));
    assert_eq!(lease.job.snapshot().state, JobState::Failed);
    assert_eq!(lease.job.snapshot().tasks[0].state, TaskState::Failed);
    drop(lease);
    let lease = jobs.begin("Finished", &[], Arc::new(|_| {})).unwrap();
    lease.job.cancel.cancel();
    lease.job.finish(None);
    assert_eq!(lease.job.snapshot().state, JobState::Complete);
}
