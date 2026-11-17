//! Per-process readiness from an actual pack update attempt, never a network probe.
use anyhow::Result;
use serde::Serialize;
use std::{future::Future, sync::Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PackState {
    #[default]
    Pending,
    Running,
    Ready,
    Offline,
    NotInstalled,
    Failed,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackPreparation {
    pub state: PackState,
    pub version: Option<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
}
impl PackPreparation {
    pub fn ready(version: &str) -> Self {
        Self {
            state: PackState::Ready,
            version: Some(version.into()),
            ..Self::default()
        }
    }
    pub fn offline(version: &str, error: &anyhow::Error) -> Self {
        Self {
            state: PackState::Offline,
            version: Some(version.into()),
            message: Some(format!(
                "Pack updates are unavailable right now. You can launch installed Auram {version}."
            )),
            detail: Some(format!("{error:#}")),
        }
    }
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            state: PackState::Failed,
            message: Some(message.into()),
            ..Self::default()
        }
    }
    pub fn allows_launch(&self) -> bool {
        matches!(self.state, PackState::Ready | PackState::Offline)
    }
}
#[derive(Default)]
pub struct PackStartup {
    runner: tokio::sync::Mutex<()>,
    state: Mutex<PackPreparation>,
}
impl PackStartup {
    pub fn status(&self) -> PackPreparation {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn record(&self, value: PackPreparation) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = value;
    }
    pub async fn prepare<F: FnOnce() -> Fut, Fut: Future<Output = Result<PackPreparation>>>(
        &self,
        retry: bool,
        work: F,
    ) -> PackPreparation {
        let _runner = self.runner.lock().await;
        let previous = self.status();
        if !retry && previous.state != PackState::Pending {
            return previous;
        }
        self.record(PackPreparation {
            state: PackState::Running,
            ..PackPreparation::default()
        });
        let guard = Attempt(self);
        let result = match work().await {
            Ok(result) => result,
            Err(error) => PackPreparation::failed(format!("Pack preparation failed: {error:#}")),
        };
        self.record(result.clone());
        drop(guard);
        result
    }
}
struct Attempt<'a>(&'a PackStartup);
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if self.0.status().state == PackState::Running {
            self.0.record(PackPreparation::failed(
                "Pack preparation was interrupted. Retry the update before launching.",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[tokio::test]
    async fn concurrent_startup_requests_share_one_attempt_and_wait_until_ready() {
        let startup = Arc::new(PackStartup::default());
        let count = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let first = {
            let startup = startup.clone();
            let count = count.clone();
            let release = release.clone();
            tokio::spawn(async move {
                startup
                    .prepare(false, || async {
                        count.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(PackPreparation::ready("151"))
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(startup.status().state, PackState::Running);
        assert!(!startup.status().allows_launch());
        let second = {
            let startup = startup.clone();
            let count = count.clone();
            tokio::spawn(async move {
                startup
                    .prepare(false, || async {
                        count.fetch_add(1, Ordering::SeqCst);
                        Ok(PackPreparation::ready("unexpected"))
                    })
                    .await
            })
        };
        release.notify_one();
        assert_eq!(first.await.unwrap(), second.await.unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(startup.status().allows_launch());
    }
    #[tokio::test]
    async fn failures_require_explicit_retry_and_aborted_work_is_recoverable() {
        let startup = Arc::new(PackStartup::default());
        assert_eq!(
            startup
                .prepare(false, || async { anyhow::bail!("checksum mismatch") })
                .await
                .state,
            PackState::Failed
        );
        assert_eq!(
            startup
                .prepare(false, || async { panic!("must not silently retry") })
                .await
                .state,
            PackState::Failed
        );
        assert_eq!(
            startup
                .prepare(true, || async { Ok(PackPreparation::ready("151")) })
                .await
                .state,
            PackState::Ready
        );
        let pending = {
            let startup = startup.clone();
            tokio::spawn(async move {
                startup
                    .prepare(true, std::future::pending::<Result<PackPreparation>>)
                    .await
            })
        };
        tokio::task::yield_now().await;
        pending.abort();
        let _ = pending.await;
        assert_eq!(startup.status().state, PackState::Failed);
        assert_eq!(
            startup
                .prepare(true, || async { Ok(PackPreparation::ready("152")) })
                .await
                .state,
            PackState::Ready
        );
    }
}
