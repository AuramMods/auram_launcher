//! Reduce concurrency only after an operation actually reports a limit/lost
//! transport. Retry private staged uploads; never replay live mutations here.
use crate::jobs::Job;
use anyhow::{bail, Context, Result};
use std::{collections::BTreeSet, future::Future, sync::Mutex, time::Duration};

pub const MAX_CONNECTIONS: usize = 20;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    Limit,
    Disconnected,
    Never,
}
struct State {
    idle: Vec<usize>,
    healthy: BTreeSet<usize>,
}
pub struct Slots {
    state: Mutex<State>,
    changed: tokio::sync::Notify,
}
struct Slot<'a> {
    owner: &'a Slots,
    index: usize,
}
impl Slots {
    pub fn new(count: usize) -> Self {
        Self {
            state: Mutex::new(State {
                idle: (0..count).collect(),
                healthy: (0..count).collect(),
            }),
            changed: tokio::sync::Notify::new(),
        }
    }
    pub fn count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .healthy
            .len()
    }
    pub fn control(&self) -> Result<usize> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .healthy
            .first()
            .copied()
            .context("All SFTP connections closed; reconnect to retry")
    }
    async fn acquire(&self, job: &Job) -> Result<Slot<'_>> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(index) = state.idle.pop() {
                    return Ok(Slot { owner: self, index });
                }
                if state.healthy.is_empty() {
                    bail!("All SFTP connections closed; reconnect to retry");
                }
            }
            tokio::select! { _=job.cancel.cancelled()=>bail!("SFTP upload cancelled"), _=changed=>{} }
        }
    }
    fn retire(&self, index: usize, keep_last: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if keep_last && state.healthy.len() == 1 {
            return false;
        }
        let removed = state.healthy.remove(&index);
        self.changed.notify_waiters();
        removed
    }
    /// Lease one transport for an ordered live mutation. Never replay a request
    /// after an uncertain acknowledgement, and never cancel it mid-mutation.
    pub async fn once<T, F, Fut, C, Close>(
        &self,
        job: &Job,
        perform: F,
        close: C,
        classify: fn(&anyhow::Error) -> Retry,
    ) -> Result<T>
    where
        F: FnOnce(usize) -> Fut,
        Fut: Future<Output = Result<T>>,
        C: FnOnce(usize) -> Close,
        Close: Future<Output = ()>,
    {
        let slot = self.acquire(job).await?;
        let result = perform(slot.index).await;
        if let Err(error) = &result {
            let retry = classify(error);
            let index = slot.index;
            if retry != Retry::Never && self.retire(index, retry == Retry::Limit) {
                drop(slot);
                close(index).await;
            }
        }
        result
    }
    pub async fn run<T, F, Fut, C, Close>(
        &self,
        job: &Job,
        perform: F,
        close: C,
        classify: fn(&anyhow::Error) -> Retry,
    ) -> Result<T>
    where
        F: Fn(usize) -> Fut,
        Fut: Future<Output = Result<T>>,
        C: Fn(usize) -> Close,
        Close: Future<Output = ()>,
    {
        for attempt in 0..MAX_CONNECTIONS + 4 {
            job.check()?;
            let slot = self.acquire(job).await?;
            let result = tokio::select! { _=job.cancel.cancelled()=>bail!("SFTP upload cancelled"), result=perform(slot.index)=>result };
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    job.check()?;
                    let retry = classify(&error);
                    if retry == Retry::Never || attempt == MAX_CONNECTIONS + 3 {
                        return Err(error);
                    }
                    let index = slot.index;
                    let retired = self.retire(index, retry == Retry::Limit);
                    drop(slot);
                    if retired {
                        close(index).await;
                    }
                    let count = self.count();
                    if count == 0 {
                        return Err(error)
                            .context("All SFTP connections failed; reconnect to retry");
                    }
                    // Only failures incur backoff. Healthy file transfers have no delay.
                    job.detail("sftp-upload", format!("Server connection limit or closed transport; continuing with {count} connections"));
                    let delay = Duration::from_millis((150u64 << attempt.min(3)).min(1200));
                    tokio::select! { _=job.cancel.cancelled()=>bail!("SFTP upload cancelled"), _=tokio::time::sleep(delay)=>{} }
                }
            }
        }
        unreachable!()
    }
}
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.healthy.contains(&self.index) {
            state.idle.push(self.index);
        }
        drop(state);
        self.owner.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[derive(Debug)]
    struct Limit;
    impl std::fmt::Display for Limit {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Too many connections")
        }
    }
    impl std::error::Error for Limit {}
    fn classify(error: &anyhow::Error) -> Retry {
        if error.is::<Limit>() {
            Retry::Limit
        } else {
            Retry::Never
        }
    }
    #[tokio::test]
    async fn uncertain_mutation_is_not_replayed_and_failed_transport_is_retired() {
        let slots = Slots::new(3);
        let calls = AtomicUsize::new(0);
        let closed = AtomicUsize::new(0);
        let result: Result<()> = slots
            .once(
                &Job::default(),
                |_| async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    bail!("Lost rename acknowledgement")
                },
                |_| async {
                    closed.fetch_add(1, Ordering::SeqCst);
                },
                |_| Retry::Disconnected,
            )
            .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(slots.count(), 2);
        slots
            .once(
                &Job::default(),
                |_| async { Ok(()) },
                |_| async {},
                classify,
            )
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn server_limits_retire_slots_and_finish_all_waiting_uploads() {
        use futures_util::{StreamExt, TryStreamExt};
        let slots = Slots::new(MAX_CONNECTIONS);
        let open = Arc::new(AtomicUsize::new(MAX_CONNECTIONS));
        let job = Job::default();
        let completed = futures_util::stream::iter(0..60)
            .map(|file| {
                let slots = &slots;
                let job = &job;
                let open = &open;
                async move {
                    slots
                        .run(
                            job,
                            |_| async {
                                if open.load(Ordering::SeqCst) > 3 {
                                    return Err(Limit.into());
                                }
                                tokio::task::yield_now().await;
                                Ok(file)
                            },
                            |_| async {
                                open.fetch_sub(1, Ordering::SeqCst);
                            },
                            classify,
                        )
                        .await
                }
            })
            .buffer_unordered(MAX_CONNECTIONS)
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(completed.len(), 60);
        assert_eq!(slots.count(), 3);
        assert_eq!(open.load(Ordering::SeqCst), 3);
        assert!(slots.control().is_ok());
    }
    #[tokio::test]
    async fn permissions_do_not_retire_slots_and_cancelled_waiters_do_not_hang() {
        let slots = Slots::new(1);
        let job = Job::default();
        let held = slots.acquire(&job).await.unwrap();
        job.cancel.cancel();
        assert!(slots.acquire(&job).await.is_err());
        drop(held);
        let error = slots
            .run(
                &Job::default(),
                |_| async { anyhow::bail!("Permission denied") },
                |_| async { panic!("must not retire a permanent error") },
                classify,
            )
            .await as Result<()>;
        assert!(error.is_err());
        assert_eq!(slots.count(), 1);
    }
    #[tokio::test]
    async fn losing_every_transport_wakes_waiters_and_fails_instead_of_waiting_forever() {
        let slots = Slots::new(1);
        let job = Job::default();
        let result: Result<()> = slots
            .run(
                &job,
                |_| async { anyhow::bail!("Connection lost") },
                |_| async {},
                |_| Retry::Disconnected,
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("All SFTP"));
        assert_eq!(slots.count(), 0);
        assert!(slots.acquire(&job).await.is_err());
    }
}
