use anyhow::{bail, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
const EMIT_INTERVAL: Duration = Duration::from_millis(120);
#[derive(Clone, Copy)]
pub struct PhaseSpec(pub &'static str, pub &'static str, pub bool);
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Running,
    Cancelling,
    Complete,
    Failed,
    Cancelled,
}
impl JobState {
    fn terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Cancelled)
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PhaseState {
    Pending,
    Running,
    Complete,
    Skipped,
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    Queued,
    Checking,
    Running,
    Downloading,
    Verifying,
    Retrying,
    Complete,
    Failed,
    Cancelled,
}
impl TaskState {
    fn active(self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Running | Self::Downloading | Self::Verifying | Self::Retrying
        )
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub id: String,
    pub name: String,
    pub state: TaskState,
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    pub attempt: u32,
    pub cached: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProgress {
    pub label: String,
    pub basis: String,
    pub completed: u64,
    pub total: u64,
    pub cached: u64,
    pub ready_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: f64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseProgress {
    pub id: String,
    pub label: String,
    pub state: PhaseState,
    pub detail: String,
    pub completed: u64,
    pub total: u64,
    pub percent: Option<f64>,
    pub batch: Option<BatchProgress>,
    pub active: u64,
    pub queued: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub job_id: u64,
    pub revision: u64,
    pub title: String,
    pub state: JobState,
    pub phases: Vec<PhaseProgress>,
    pub active_phase: usize,
    pub tasks: Vec<TaskProgress>,
    pub recent: Vec<TaskProgress>,
    pub downloaded_bytes: u64,
    pub error: Option<String>,
    // Retained for CLI diagnostics; individual transfers never replace the phase message.
    pub operation: String,
    pub message: String,
}
struct Phase {
    id: String,
    label: String,
    determinate: bool,
    state: PhaseState,
    detail: String,
    completed: u64,
    total: u64,
    batch: Option<(String, Vec<String>, bool)>,
}
struct Task {
    progress: TaskProgress,
    phase: usize,
    order: u64,
    finished: u64,
}
struct Tracker {
    id: u64,
    revision: u64,
    title: String,
    state: JobState,
    phases: Vec<Phase>,
    current: usize,
    tasks: BTreeMap<String, Task>,
    serial: u64,
    downloaded: u64,
    error: Option<String>,
    operation: String,
    last_emit: Instant,
    last: Option<Progress>,
    dirty: bool,
    scheduled: bool,
}
impl Tracker {
    fn phase(&mut self) -> &mut Phase {
        if self.phases.is_empty() {
            self.phases
                .push(Phase::new(PhaseSpec("work", "Working", false)));
            self.phases[0].state = PhaseState::Running;
        }
        &mut self.phases[self.current]
    }
    fn snapshot(&self) -> Progress {
        let phases = self
            .phases
            .iter()
            .enumerate()
            .map(|(index, phase)| {
                let tasks = self
                    .tasks
                    .values()
                    .filter(|t| t.phase == index)
                    .collect::<Vec<_>>();
                let batch = phase.batch.as_ref().map(|(label, ids, byte_weighted)| {
                    let items = ids
                        .iter()
                        .filter_map(|id| self.tasks.get(id))
                        .map(|t| &t.progress)
                        .collect::<Vec<_>>();
                    let completed = items
                        .iter()
                        .filter(|t| t.state == TaskState::Complete)
                        .count() as u64;
                    let total = items.len() as u64;
                    let total_bytes = items
                        .iter()
                        .try_fold(0u64, |sum, t| t.total_bytes.map(|n| sum.saturating_add(n)));
                    let ready_bytes = items
                        .iter()
                        .filter(|t| t.state == TaskState::Complete)
                        .map(|t| t.total_bytes.unwrap_or(t.bytes))
                        .sum();
                    let percent = if completed == total {
                        100.0
                    } else if let Some(bytes) = total_bytes.filter(|n| *byte_weighted && *n > 0) {
                        ready_bytes as f64 / bytes as f64 * 100.0
                    } else if total > 0 {
                        completed as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    };
                    BatchProgress {
                        label: label.clone(),
                        basis: if *byte_weighted { "bytes" } else { "files" }.into(),
                        completed,
                        total,
                        cached: items
                            .iter()
                            .filter(|t| t.cached && t.state == TaskState::Complete)
                            .count() as u64,
                        ready_bytes,
                        total_bytes,
                        percent: percent.min(if completed == total { 100.0 } else { 99.9 }),
                    }
                });
                PhaseProgress {
                    id: phase.id.clone(),
                    label: phase.label.clone(),
                    state: phase.state,
                    detail: phase.detail.clone(),
                    completed: phase.completed,
                    total: phase.total,
                    percent: if matches!(phase.state, PhaseState::Complete | PhaseState::Skipped) {
                        Some(100.0)
                    } else if phase.determinate {
                        batch.as_ref().map(|b| b.percent)
                    } else {
                        None
                    },
                    batch,
                    active: tasks.iter().filter(|t| t.progress.state.active()).count() as u64,
                    queued: tasks
                        .iter()
                        .filter(|t| t.progress.state == TaskState::Queued)
                        .count() as u64,
                }
            })
            .collect::<Vec<_>>();
        let mut active = self
            .tasks
            .values()
            .filter(|t| {
                t.phase == self.current
                    && t.progress.state != TaskState::Queued
                    && t.progress.state != TaskState::Complete
            })
            .collect::<Vec<_>>();
        active.sort_by_key(|t| t.order);
        let mut recent = self
            .tasks
            .values()
            .filter(|t| t.phase == self.current && t.progress.state == TaskState::Complete)
            .collect::<Vec<_>>();
        recent.sort_by_key(|t| std::cmp::Reverse(t.finished));
        Progress {
            job_id: self.id,
            revision: self.revision,
            title: self.title.clone(),
            state: self.state,
            active_phase: self.current,
            tasks: active
                .into_iter()
                .take(16)
                .map(|t| t.progress.clone())
                .collect(),
            recent: recent
                .into_iter()
                .take(3)
                .map(|t| t.progress.clone())
                .collect(),
            downloaded_bytes: self.downloaded,
            error: self.error.clone(),
            operation: self.operation.clone(),
            message: phases
                .get(self.current)
                .map(|p| p.detail.clone())
                .unwrap_or_default(),
            phases,
        }
    }
    fn register(&mut self, key: &str, name: &str, size: Option<u64>) -> String {
        self.phase();
        let id = format!("{}:{key}", self.current);
        if !self.tasks.contains_key(&id) {
            self.serial += 1;
            self.tasks.insert(
                id.clone(),
                Task {
                    phase: self.current,
                    order: self.serial,
                    finished: 0,
                    progress: TaskProgress {
                        id: id.clone(),
                        name: name.into(),
                        state: TaskState::Queued,
                        bytes: 0,
                        total_bytes: size,
                        attempt: 0,
                        cached: false,
                        error: None,
                    },
                },
            );
        }
        id
    }
}
impl Phase {
    fn new(spec: PhaseSpec) -> Self {
        Self {
            id: spec.0.into(),
            label: spec.1.into(),
            determinate: spec.2,
            state: PhaseState::Pending,
            detail: String::new(),
            completed: 0,
            total: 0,
            batch: None,
        }
    }
}
#[derive(Clone)]
pub struct Job {
    pub cancel: CancellationToken,
    tracker: Arc<Mutex<Tracker>>,
    emit: Arc<dyn Fn(Progress) + Send + Sync>,
}
impl Default for Job {
    fn default() -> Self {
        Self::new("Working", &[], Arc::new(|_| {}))
    }
}
impl Job {
    pub fn new(
        title: &str,
        phases: &[PhaseSpec],
        emit: Arc<dyn Fn(Progress) + Send + Sync>,
    ) -> Self {
        let mut tracker = Tracker {
            id: NEXT_JOB.fetch_add(1, Ordering::Relaxed),
            revision: 0,
            title: title.into(),
            state: JobState::Running,
            phases: phases.iter().copied().map(Phase::new).collect(),
            current: 0,
            tasks: BTreeMap::new(),
            serial: 0,
            downloaded: 0,
            error: None,
            operation: String::new(),
            last_emit: Instant::now() - EMIT_INTERVAL,
            last: None,
            dirty: true,
            scheduled: false,
        };
        if let Some(first) = tracker.phases.first_mut() {
            first.state = PhaseState::Running;
        }
        Self {
            cancel: CancellationToken::new(),
            tracker: Arc::new(Mutex::new(tracker)),
            emit,
        }
    }
    fn change(&self, force: bool, update: impl FnOnce(&mut Tracker)) {
        let mut trailing = None;
        let snapshot = {
            let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            if tracker.state.terminal() {
                return;
            }
            update(&mut tracker);
            tracker.dirty = true;
            if force || tracker.last_emit.elapsed() >= EMIT_INTERVAL {
                tracker.revision += 1;
                tracker.last_emit = Instant::now();
                tracker.dirty = false;
                let snapshot = tracker.snapshot();
                tracker.last = Some(snapshot.clone());
                Some(snapshot)
            } else {
                if !tracker.scheduled {
                    tracker.scheduled = true;
                    trailing = Some(EMIT_INTERVAL.saturating_sub(tracker.last_emit.elapsed()));
                }
                None
            }
        };
        if let Some(snapshot) = snapshot {
            (self.emit)(snapshot);
        }
        if let Some(delay) = trailing {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let job = self.clone();
                runtime.spawn(async move {
                    tokio::time::sleep(delay).await;
                    job.emit_trailing();
                });
            } else {
                self.tracker
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .scheduled = false;
            }
        }
    }
    fn emit_trailing(&self) {
        let snapshot = {
            let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            tracker.scheduled = false;
            if !tracker.dirty || tracker.state.terminal() {
                None
            } else {
                tracker.dirty = false;
                tracker.revision += 1;
                tracker.last_emit = Instant::now();
                let snapshot = tracker.snapshot();
                tracker.last = Some(snapshot.clone());
                Some(snapshot)
            }
        };
        if let Some(snapshot) = snapshot {
            (self.emit)(snapshot);
        }
    }
    pub fn snapshot(&self) -> Progress {
        let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
        tracker.last.clone().unwrap_or_else(|| tracker.snapshot())
    }
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        Ok(())
    }
    pub fn phase(&self, id: &str, label: &str, determinate: bool) {
        self.change(true, |t| {
            let index = t.phases.iter().position(|p| p.id == id).unwrap_or_else(|| {
                t.phases.push(Phase {
                    id: id.into(),
                    label: label.into(),
                    determinate,
                    state: PhaseState::Pending,
                    detail: String::new(),
                    completed: 0,
                    total: 0,
                    batch: None,
                });
                t.phases.len() - 1
            });
            if index != t.current {
                for (i, p) in t.phases.iter_mut().enumerate().take(index) {
                    if p.state == PhaseState::Running {
                        p.state = PhaseState::Complete;
                    } else if p.state == PhaseState::Pending {
                        p.state = if i == t.current {
                            PhaseState::Complete
                        } else {
                            PhaseState::Skipped
                        };
                    }
                }
            }
            t.current = index;
            t.phases[index].state = PhaseState::Running;
            t.operation = id.into();
        });
    }
    pub fn progress(
        &self,
        operation: &str,
        message: impl Into<String>,
        completed: u64,
        total: u64,
    ) {
        let message = message.into();
        self.change(false, |t| {
            t.operation = operation.into();
            let phase = t.phase();
            phase.detail = message;
            phase.completed = completed;
            phase.total = total;
        });
    }
    pub fn plan_downloads(&self, label: &str, items: &[(String, String, Option<u64>)]) {
        self.change(true, |t| {
            let ids = items
                .iter()
                .map(|(key, name, size)| {
                    let id = t.register(key, name, *size);
                    let task = &mut t.tasks.get_mut(&id).unwrap().progress;
                    task.state = TaskState::Queued;
                    task.bytes = 0;
                    task.cached = false;
                    task.error = None;
                    id
                })
                .collect();
            t.phase().batch = Some((
                label.into(),
                ids,
                items.iter().all(|item| item.2.is_some())
                    && items.iter().any(|item| item.2.is_some_and(|n| n > 0)),
            ));
        });
    }
    pub fn task(&self, key: &str, name: &str, size: Option<u64>) -> TaskTicket {
        let mut id = String::new();
        self.change(false, |t| {
            t.operation = "download".into();
            id = t.register(key, name, size);
            t.tasks.get_mut(&id).unwrap().progress.state = TaskState::Checking;
        });
        TaskTicket {
            job: self.clone(),
            id,
            finished: false,
        }
    }
    pub fn flush(&self) {
        self.change(true, |_| {});
    }
    pub fn complete_phase(&self) {
        self.change(true, |t| {
            t.phase().state = PhaseState::Complete;
        });
    }
    pub fn finish(&self, error: Option<String>) {
        self.change(true, |t| {
            t.state = if error.is_none() {
                JobState::Complete
            } else if self.cancel.is_cancelled() {
                JobState::Cancelled
            } else {
                JobState::Failed
            };
            t.error = error;
            if t.state != JobState::Complete
                && t.phases
                    .get(t.current)
                    .is_some_and(|p| p.state == PhaseState::Complete)
            {
                t.phases[t.current].state = if t.state == JobState::Cancelled {
                    PhaseState::Cancelled
                } else {
                    PhaseState::Failed
                };
            }
            for p in &mut t.phases {
                if p.state == PhaseState::Running {
                    p.state = match t.state {
                        JobState::Complete => PhaseState::Complete,
                        JobState::Cancelled => PhaseState::Cancelled,
                        _ => PhaseState::Failed,
                    };
                } else if p.state == PhaseState::Pending && t.state == JobState::Complete {
                    p.state = PhaseState::Skipped;
                }
            }
            for task in t
                .tasks
                .values_mut()
                .filter(|task| task.progress.state.active())
            {
                task.progress.state = TaskState::Cancelled;
            }
        });
    }
}
pub struct TaskTicket {
    job: Job,
    id: String,
    finished: bool,
}
impl TaskTicket {
    pub fn state(&self, state: TaskState, attempt: u32) {
        self.job.change(false, |t| {
            t.operation = "download".into();
            if let Some(task) = t.tasks.get_mut(&self.id) {
                task.progress.state = state;
                if attempt > 0 {
                    task.progress.attempt = attempt;
                }
            }
        });
    }
    pub fn transfer(&self, bytes: u64, total: Option<u64>, network: u64) {
        self.job.change(false, |t| {
            t.operation = "download".into();
            t.downloaded = t.downloaded.saturating_add(network);
            if let Some(task) = t.tasks.get_mut(&self.id) {
                task.progress.bytes = bytes;
                if total.is_some() {
                    task.progress.total_bytes = total;
                }
                task.progress.state = TaskState::Downloading;
            }
        });
    }
    pub fn complete(&mut self, cached: bool) {
        self.finished = true;
        self.job.change(false, |t| {
            t.serial += 1;
            if let Some(task) = t.tasks.get_mut(&self.id) {
                task.progress.state = TaskState::Complete;
                task.progress.cached = cached;
                task.progress.bytes = task.progress.total_bytes.unwrap_or(task.progress.bytes);
                task.finished = t.serial;
            }
        });
    }
    pub fn fail(&mut self, error: &str) {
        self.finished = true;
        self.job.change(false, |t| {
            if let Some(task) = t.tasks.get_mut(&self.id) {
                task.progress.state = if self.job.cancel.is_cancelled() {
                    TaskState::Cancelled
                } else {
                    TaskState::Failed
                };
                task.progress.error = Some(error.into());
            }
        });
    }
}
impl Drop for TaskTicket {
    fn drop(&mut self) {
        if !self.finished {
            self.job.change(false, |t| {
                if let Some(task) = t.tasks.get_mut(&self.id) {
                    task.progress.state = TaskState::Cancelled;
                }
            });
        }
    }
}
#[derive(Default)]
struct JobSlot {
    active: Option<Job>,
    last: Option<Progress>,
}
#[derive(Default)]
pub struct Jobs {
    slot: Mutex<JobSlot>,
}
pub struct Lease<'a> {
    owner: &'a Jobs,
    pub job: Job,
}
impl Jobs {
    pub fn begin(
        &self,
        title: &str,
        phases: &[PhaseSpec],
        emit: Arc<dyn Fn(Progress) + Send + Sync>,
    ) -> Result<Lease<'_>> {
        let job = Job::new(title, phases, emit);
        {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            if slot.active.is_some() {
                bail!("Another operation is still running");
            }
            slot.active = Some(job.clone());
        }
        job.flush();
        Ok(Lease { owner: self, job })
    }
    pub fn cancel(&self) {
        let job = self
            .slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .clone();
        if let Some(job) = job {
            job.cancel.cancel();
            job.change(true, |t| t.state = JobState::Cancelling);
        }
    }
    pub fn busy(&self) -> bool {
        self.slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .is_some()
    }
    pub fn progress(&self) -> Option<Progress> {
        let (job, last) = {
            let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            (slot.active.clone(), slot.last.clone())
        };
        job.map(|j| j.snapshot()).or(last)
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if !self.job.snapshot().state.terminal() {
            self.job.finish(Some("Operation interrupted".into()));
        }
        let mut slot = self.owner.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.last = Some(self.job.snapshot());
        slot.active = None;
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
