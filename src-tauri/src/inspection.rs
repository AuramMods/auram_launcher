//! Request-scoped inspection cancellation, including close-before-start races.
//! A late dialog-close message must never cancel another launcher operation.
use anyhow::{bail, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct State {
    active: BTreeMap<String, CancellationToken>,
    cancelled: VecDeque<String>,
}
#[derive(Default)]
pub struct Inspections(Mutex<State>);
pub struct Registration<'a> {
    owner: &'a Inspections,
    id: String,
}
impl Inspections {
    pub fn register(&self, id: &str, cancel: CancellationToken) -> Result<Registration<'_>> {
        if id.is_empty() || id.len() > 128 {
            bail!("Invalid server inspection request ID");
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.active.contains_key(id) {
            bail!("This server inspection is already running");
        }
        if let Some(index) = state.cancelled.iter().position(|value| value == id) {
            state.cancelled.remove(index);
            cancel.cancel();
        }
        state.active.insert(id.into(), cancel);
        Ok(Registration {
            owner: self,
            id: id.into(),
        })
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 128 {
            bail!("Invalid server inspection request ID");
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(token) = state.active.get(id) {
            token.cancel();
        } else if !state.cancelled.iter().any(|value| value == id) {
            state.cancelled.push_back(id.into());
            if state.cancelled.len() > 128 {
                state.cancelled.pop_front();
            }
        }
        Ok(())
    }
}
impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.owner
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn close_before_start_and_close_during_request_cancel_only_that_inspection() {
        let requests = Inspections::default();
        requests.cancel("closed-before-start").unwrap();
        let token = CancellationToken::new();
        let before = requests
            .register("closed-before-start", token.clone())
            .unwrap();
        assert!(token.is_cancelled());
        drop(before);
        let token = CancellationToken::new();
        let other = CancellationToken::new();
        let active = requests.register("opened", token.clone()).unwrap();
        let separate = requests.register("separate", other.clone()).unwrap();
        requests.cancel("opened").unwrap();
        assert!(token.is_cancelled());
        assert!(!other.is_cancelled());
        drop(active);
        drop(separate);
        assert!(requests.0.lock().unwrap().active.is_empty());
    }
    #[test]
    fn late_close_and_bounded_tombstones_cannot_cancel_subsequent_jobs() {
        let requests = Inspections::default();
        let old = requests
            .register("old-dialog", CancellationToken::new())
            .unwrap();
        drop(old);
        let deployment = CancellationToken::new();
        let current = requests.register("new-dialog", deployment.clone()).unwrap();
        requests.cancel("old-dialog").unwrap();
        assert!(!deployment.is_cancelled());
        for id in 0..300 {
            requests.cancel(&format!("finished-{id}")).unwrap();
        }
        assert_eq!(requests.0.lock().unwrap().cancelled.len(), 128);
        drop(current);
    }
    #[tokio::test]
    async fn cancelling_an_inspection_releases_the_real_job_lease_and_keeps_the_next_job_running() {
        let requests = Inspections::default();
        let jobs = crate::jobs::Jobs::default();
        let lease = jobs
            .begin("Checking server version", &[], std::sync::Arc::new(|_| {}))
            .unwrap();
        let registration = requests
            .register("dialog", lease.job.cancel.clone())
            .unwrap();
        requests.cancel("dialog").unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            lease.job.cancel.cancelled(),
        )
        .await
        .unwrap();
        assert!(lease.job.check().is_err());
        lease.job.finish(Some("Cancelled".into()));
        drop(registration);
        drop(lease);
        assert!(!jobs.busy());
        let next = jobs
            .begin("Deploying server", &[], std::sync::Arc::new(|_| {}))
            .unwrap();
        requests.cancel("dialog").unwrap();
        assert!(next.job.check().is_ok());
        next.job.finish(None);
    }
}
