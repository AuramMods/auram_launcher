use anyhow::{bail, Result};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub operation: String,
    pub message: String,
    pub completed: u64,
    pub total: u64,
    pub bytes: u64,
    pub total_bytes: u64,
}
#[derive(Clone)]
pub struct Job {
    pub cancel: CancellationToken,
    pub emit: Arc<dyn Fn(Progress) + Send + Sync>,
}
impl Default for Job {
    fn default() -> Self {
        Self {
            cancel: CancellationToken::new(),
            emit: Arc::new(|_| {}),
        }
    }
}
impl Job {
    pub fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            bail!("Operation cancelled");
        }
        Ok(())
    }
    pub fn progress(
        &self,
        operation: &str,
        message: impl Into<String>,
        completed: u64,
        total: u64,
    ) {
        (self.emit)(Progress {
            operation: operation.into(),
            message: message.into(),
            completed,
            total,
            ..Default::default()
        });
    }
}
#[derive(Default)]
pub struct Jobs {
    active: Mutex<Option<CancellationToken>>,
}
pub struct Lease<'a> {
    owner: &'a Jobs,
    pub job: Job,
}
impl Jobs {
    pub fn begin(&self, emit: Arc<dyn Fn(Progress) + Send + Sync>) -> Result<Lease<'_>> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            bail!("Another operation is still running");
        }
        let cancel = CancellationToken::new();
        *active = Some(cancel.clone());
        Ok(Lease {
            owner: self,
            job: Job { cancel, emit },
        })
    }
    pub fn cancel(&self) {
        if let Some(token) = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            token.cancel();
        }
    }
    pub fn busy(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        *self.owner.active.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
