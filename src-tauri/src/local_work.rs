//! Bounded CPU/disk work, independent of Tokio's async executor and HTTP limits.
use crate::jobs::Job;
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::sync::{Mutex, OnceLock};

fn pool() -> &'static Option<rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let detected = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(8);
        let workers = std::env::var("AURAM_FILE_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(detected);
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers.clamp(1, 32))
            .thread_name(|i| format!("auram-files-{i}"))
            .build()
            .ok()
    })
}
pub fn workers() -> usize {
    pool()
        .as_ref()
        .map_or(1, rayon::ThreadPool::current_num_threads)
}
/// Indexed collection keeps planning deterministic despite out-of-order completion.
/// If OS thread creation fails, the same actual work runs on the calling worker.
pub fn map<T: Sync, U: Send>(
    items: &[T],
    work: impl Fn(&T) -> Result<U> + Sync + Send,
) -> Result<Vec<U>> {
    match pool() {
        Some(pool) => pool.install(|| items.par_iter().map(work).collect()),
        None => items.iter().map(work).collect(),
    }
}
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .context("Local file worker failed")?
}
/// Count only finished files, serializing the brief emit so counts cannot regress.
pub struct Progress<'a> {
    job: &'a Job,
    message: String,
    total: u64,
    finished: Mutex<u64>,
}
impl<'a> Progress<'a> {
    pub fn new(job: &'a Job, message: &str, total: usize) -> Self {
        let message = message.to_owned();
        job.progress("local-files", &message, 0, total as u64);
        Self {
            job,
            message,
            total: total as u64,
            finished: Mutex::new(0),
        }
    }
    pub fn complete(&self) {
        let mut finished = self.finished.lock().unwrap_or_else(|e| e.into_inner());
        *finished += 1;
        self.job
            .progress("local-files", &self.message, *finished, self.total);
    }
}
impl Drop for Progress<'_> {
    fn drop(&mut self) {
        // Rayon workers may run outside a Tokio context; always publish their
        // final coalesced count before the coordinator changes phase.
        self.job.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeSet, sync::Arc};
    #[test]
    fn parallel_results_are_ordered_and_failures_propagate() {
        let input = (0..128).collect::<Vec<_>>();
        let threads = Mutex::new(BTreeSet::new());
        let output = map(&input, |n| {
            threads.lock().unwrap().insert(
                std::thread::current()
                    .name()
                    .unwrap_or("fallback")
                    .to_owned(),
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
            Ok(n * 2)
        })
        .unwrap();
        assert_eq!(output, input.iter().map(|n| n * 2).collect::<Vec<_>>());
        assert!(threads.lock().unwrap().len() <= workers());
        if workers() > 1 {
            assert!(threads.lock().unwrap().len() > 1);
        }
        assert!(map(&input, |n| {
            anyhow::ensure!(*n != 33, "fixture error");
            Ok(*n)
        })
        .unwrap_err()
        .to_string()
        .contains("fixture error"));
    }
    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_leaves_the_async_executor_responsive() {
        let (ready, started) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let work = tokio::spawn(blocking(move || {
            let _ = ready.send(());
            wait.recv_timeout(std::time::Duration::from_secs(2))?;
            Ok(42)
        }));
        started.await.unwrap();
        release.send(()).unwrap();
        assert_eq!(work.await.unwrap().unwrap(), 42);
    }
    #[test]
    fn concurrent_completions_never_regress_or_lose_counts() {
        let counts = Arc::new(Mutex::new(Vec::new()));
        let sink = counts.clone();
        let job = Job::new(
            "Inspect",
            &[],
            Arc::new(move |p| {
                sink.lock()
                    .unwrap()
                    .push(p.phases[p.active_phase].completed)
            }),
        );
        let progress = Progress::new(&job, "Checking files", 1000);
        map(&(0..1000).collect::<Vec<_>>(), |_| {
            progress.complete();
            Ok(())
        })
        .unwrap();
        drop(progress);
        let counts = counts.lock().unwrap();
        assert_eq!(counts.last(), Some(&1000));
        assert!(counts.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
