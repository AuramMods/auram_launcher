use crate::jobs::{Job, TaskState, TaskTicket};
use anyhow::{bail, Result};
use futures_util::StreamExt;
use reqwest::{header::HeaderMap, Response, StatusCode};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::time::Instant;

pub(super) const ATTEMPTS: u32 = 5;
const SPACING: Duration = Duration::from_millis(250);
const RECOVERY: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct RetryPolicy {
    origins: Mutex<HashMap<String, Cooldown>>,
}
struct Cooldown {
    until: Instant,
    next: Instant,
    paced_until: Instant,
}

#[derive(Debug)]
pub(super) struct RetryableStatus {
    status: StatusCode,
    delay: Duration,
    shared: bool,
    limited: bool,
}
impl std::fmt::Display for RetryableStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (HTTP {}); retry after {} seconds",
            if self.limited {
                "Server rate limit"
            } else {
                "Server temporarily unavailable"
            },
            self.status.as_u16(),
            self.delay.as_secs().saturating_add(1),
        )
    }
}
impl std::error::Error for RetryableStatus {}

fn origin(url: &str) -> Result<(String, String)> {
    let url = url::Url::parse(url)?;
    Ok((
        url.origin().ascii_serialization(),
        url.host_str().unwrap_or("server").into(),
    ))
}
fn exponential(attempt: u32) -> Duration {
    Duration::from_millis((400u64 << attempt.min(7)).min(30_000))
}
fn date(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp())
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}
fn classify(
    status: StatusCode,
    headers: &HeaderMap,
    body: &str,
    attempt: u32,
    now: i64,
) -> Option<RetryableStatus> {
    // Server Date avoids retrying too early when the user's clock is ahead.
    let now = header(headers, "date").and_then(date).unwrap_or(now);
    let remaining_zero = header(headers, "x-ratelimit-remaining") == Some("0");
    let retry_after = header(headers, "retry-after").and_then(|value| {
        value
            .parse::<u64>()
            .ok()
            .or_else(|| date(value).map(|time| time.saturating_sub(now).max(0) as u64))
    });
    let reset = remaining_zero
        .then(|| header(headers, "x-ratelimit-reset")?.parse::<i64>().ok())
        .flatten()
        .map(|time| time.saturating_sub(now).max(0) as u64);
    let message = body.to_ascii_lowercase();
    let limited = status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::FORBIDDEN
            && (remaining_zero
                || retry_after.is_some()
                || message.contains("rate limit")
                || message.contains("abuse detection"));
    if !limited && status != StatusCode::REQUEST_TIMEOUT && !status.is_server_error() {
        return None;
    }
    let advertised = retry_after
        .into_iter()
        .chain(reset)
        .max()
        .map(Duration::from_secs);
    let delay = advertised
        .unwrap_or_else(|| {
            if limited {
                Duration::from_secs(60 << attempt.min(4))
            } else {
                exponential(attempt)
            }
        })
        .max(exponential(attempt));
    Some(RetryableStatus {
        status,
        delay,
        shared: limited || advertised.is_some(),
        limited,
    })
}
pub(super) fn permanent_status(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.status().is_some())
}
pub(super) fn transient(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RetryableStatus>().is_some()
        || error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout() || e.is_body())
}

impl RetryPolicy {
    fn defer(&self, url: &str, delay: Duration) -> Result<()> {
        let (key, _) = origin(url)?;
        // A malformed far-future header must fail explicitly, never overflow or
        // shorten the server's requested wait into an immediate retry.
        let until = Instant::now()
            .checked_add(delay)
            .ok_or_else(|| anyhow::anyhow!("Server retry time is out of range"))?;
        let paced_until = until
            .checked_add(RECOVERY)
            .ok_or_else(|| anyhow::anyhow!("Server retry time is out of range"))?;
        let mut origins = self.origins.lock().unwrap();
        match origins.get_mut(&key) {
            Some(state) => {
                state.until = state.until.max(until);
                state.paced_until = state.paced_until.max(paced_until);
            }
            None => {
                origins.insert(
                    key,
                    Cooldown {
                        until,
                        next: until,
                        paced_until,
                    },
                );
            }
        }
        Ok(())
    }
    pub(super) async fn wait(&self, url: &str, job: &Job, task: Option<&TaskTicket>) -> Result<()> {
        let (key, host) = origin(url)?;
        let mut reported = false;
        loop {
            job.check()?;
            let waiting = {
                let mut origins = self.origins.lock().unwrap();
                let now = Instant::now();
                if let Some(state) = origins.get_mut(&key) {
                    let until = state.until.max(state.next);
                    if now < until {
                        Some((until - now, now < state.until))
                    } else {
                        if now < state.paced_until {
                            state.next = now + SPACING;
                        } else {
                            origins.remove(&key);
                        }
                        None
                    }
                } else {
                    None
                }
            };
            let Some((delay, cooldown)) = waiting else {
                if reported {
                    job.detail("download", format!("Retrying {host}"));
                }
                return Ok(());
            };
            if cooldown {
                reported = true;
                if let Some(task) = task {
                    task.state(TaskState::Retrying, 0);
                }
                job.detail(
                    "download",
                    format!(
                        "{host} requested a pause; retrying in {}s",
                        delay.as_secs().saturating_add(1)
                    ),
                );
            }
            tokio::select! {
                _=job.cancel.cancelled()=>bail!("Operation cancelled"),
                _=tokio::time::sleep(delay.min(Duration::from_secs(1)))=>{},
            }
        }
    }
    pub(super) async fn response(
        &self,
        url: &str,
        response: Response,
        attempt: u32,
        job: &Job,
    ) -> Result<Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let headers = response.headers().clone();
        // Preserve reqwest status errors (including 404) for existing pack fallback
        // behavior. A genuine permission failure must not turn into a long wait.
        let permanent = response.error_for_status_ref().err();
        let now = chrono::Utc::now().timestamp();
        let mut retry = classify(status, &headers, "", attempt, now);
        let mut body = Vec::new();
        if status == StatusCode::FORBIDDEN && retry.is_none() {
            let mut stream = response.bytes_stream();
            while body.len() < 64 * 1024 {
                let next = tokio::select! {
                    _=job.cancel.cancelled()=>bail!("Operation cancelled"),
                    next=stream.next()=>next,
                };
                let Some(Ok(chunk)) = next else { break };
                let remaining = 64 * 1024 - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            }
            retry = classify(
                status,
                &headers,
                &String::from_utf8_lossy(&body),
                attempt,
                now,
            );
        }
        if let Some(mut retry) = retry {
            // Jitter keeps launchers on different machines from retrying in lockstep.
            retry.delay += Duration::from_millis(rand::random_range(0..=250));
            if retry.shared {
                self.defer(url, retry.delay)?;
            }
            return Err(retry.into());
        }
        match permanent {
            Some(error) => Err(error.into()),
            None => bail!("Unexpected HTTP {status}"),
        }
    }
    pub(super) async fn after_error(
        &self,
        error: &anyhow::Error,
        attempt: u32,
        job: &Job,
    ) -> Result<()> {
        job.check()?;
        let delay = match error.downcast_ref::<RetryableStatus>() {
            Some(retry) if retry.shared => return Ok(()), // next request joins the shared cooldown
            Some(retry) => retry.delay,
            None => exponential(attempt) + Duration::from_millis(rand::random_range(0..=250)),
        };
        tokio::select! {
            _=job.cancel.cancelled()=>bail!("Operation cancelled"),
            _=tokio::time::sleep(delay)=>Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
