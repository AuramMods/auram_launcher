use super::*;
use crate::{
    download::{Checksum, Download, Downloader},
    storage::{self, Layout},
};
use axum::{body::Body, http::Response as HttpResponse, routing::get, Router};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn headers(values: &[(&str, &str)]) -> HeaderMap {
    values
        .iter()
        .map(|(name, value)| {
            (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn reset_and_retry_after_are_not_capped_and_account_for_server_clock() {
    let primary = classify(
        StatusCode::FORBIDDEN,
        &headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "4600"),
            ("date", "Thu, 01 Jan 1970 00:16:40 GMT"),
        ]),
        "",
        0,
        10_000,
    )
    .unwrap();
    assert_eq!(primary.delay, Duration::from_secs(3600));
    assert!(primary.limited && primary.shared);
    let dated = classify(
        StatusCode::TOO_MANY_REQUESTS,
        &headers(&[
            ("date", "Wed, 21 Oct 2015 07:28:00 GMT"),
            ("retry-after", "Wed, 21 Oct 2015 07:30:00 GMT"),
        ]),
        "",
        0,
        0,
    )
    .unwrap();
    assert_eq!(dated.delay, Duration::from_secs(120));
    let both = classify(
        StatusCode::TOO_MANY_REQUESTS,
        &headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "4600"),
            ("retry-after", "7200"),
        ]),
        "",
        0,
        1000,
    )
    .unwrap();
    assert_eq!(both.delay, Duration::from_secs(7200));
}

#[test]
fn secondary_limits_back_off_exponentially_but_permission_errors_do_not_retry() {
    for attempt in 0..ATTEMPTS {
        let retry = classify(
            StatusCode::FORBIDDEN,
            &HeaderMap::new(),
            r#"{"message":"You have exceeded a secondary rate limit."}"#,
            attempt,
            0,
        )
        .unwrap();
        assert_eq!(retry.delay, Duration::from_secs(60 << attempt));
    }
    assert!(classify(StatusCode::FORBIDDEN, &HeaderMap::new(), "Forbidden", 0, 0).is_none());
    assert!(classify(StatusCode::NOT_FOUND, &HeaderMap::new(), "Not Found", 0, 0).is_none());
    assert!(classify(
        StatusCode::UNAUTHORIZED,
        &headers(&[("retry-after", "10")]),
        "",
        0,
        0
    )
    .is_none());
    let busy = classify(StatusCode::SERVICE_UNAVAILABLE, &HeaderMap::new(), "", 3, 0).unwrap();
    assert_eq!(busy.delay, Duration::from_millis(3200));
    assert!(!busy.shared);
    assert_eq!(exponential(100), Duration::from_secs(30));
}

struct Fixture {
    base: String,
    net: Downloader,
    server: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let temp = tempfile::tempdir().unwrap();
        let net = Downloader::new(&Layout::new(temp.path().into()).unwrap()).unwrap();
        Self {
            base,
            net,
            server,
            _temp: temp,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
    fn file(&self, path: &str) -> Download {
        Download {
            label: None,
            url: self.url(path),
            target: self._temp.path().join(path.trim_start_matches('/')),
            size: Some(6),
            checksum: Checksum::GitBlob("ce013625030ba8dba906f756967f9e9ca394464a".into()),
        }
    }
}
fn response(status: u16, retry: Option<&str>, body: &str) -> HttpResponse<Body> {
    let mut response = HttpResponse::builder().status(status);
    if let Some(value) = retry {
        response = response.header("retry-after", value);
    }
    response.body(Body::from(body.to_owned())).unwrap()
}

#[tokio::test]
async fn metadata_retries_rate_limits_and_transient_errors_but_not_ordinary_403() {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let fixture = Fixture::new(
        Router::new()
            .route(
                "/meta",
                get(move || {
                    let counter = counter.clone();
                    async move {
                        match counter.fetch_add(1, Ordering::SeqCst) {
                            0 => {
                                response(403, Some("0"), r#"{"message":"API rate limit exceeded"}"#)
                            }
                            1 => response(503, None, "busy"),
                            _ => response(200, None, r#"{"version":155}"#),
                        }
                    }
                }),
            )
            .route(
                "/forbidden",
                get(|| async { response(403, None, "Forbidden") }),
            )
            .route(
                "/missing",
                get(|| async { response(404, None, "Not Found") }),
            ),
    )
    .await;
    let value: Value = fixture.net.json(&fixture.url("/meta")).await.unwrap();
    assert_eq!(value, json!({"version":155}));
    assert_eq!(requests.load(Ordering::SeqCst), 3);
    for (path, status) in [("/forbidden", 403), ("/missing", 404)] {
        let error = fixture
            .net
            .json::<Value>(&fixture.url(path))
            .await
            .unwrap_err();
        assert!(!crate::download::unavailable(&error));
        assert_eq!(
            error
                .downcast_ref::<reqwest::Error>()
                .unwrap()
                .status()
                .unwrap()
                .as_u16(),
            status
        );
    }
}

#[tokio::test]
async fn metadata_and_parallel_files_share_cooldown_and_resume_gradually() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let counter = requests.clone();
    let fixture = Fixture::new(Router::new().fallback(get(move |uri: axum::http::Uri| {
        let counter = counter.clone();
        async move {
            let first = {
                let mut requests = counter.lock().unwrap();
                let first = requests.is_empty();
                requests.push(Instant::now());
                first
            };
            if first {
                response(429, Some("1"), "slow down")
            } else if uri.path() == "/meta" {
                response(200, None, "{}")
            } else {
                response(200, None, "hello\n")
            }
        }
    })))
    .await;
    let net = fixture.net.clone();
    let url = fixture.url("/meta");
    let job = Job::default();
    let running = job.clone();
    let metadata = tokio::spawn(async move { net.json_with_job::<Value>(&url, &running).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.net.retry.origins.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let files = (0..4)
        .map(|i| fixture.file(&format!("/file-{i}")))
        .collect();
    fixture.net.many(files, &job).await.unwrap();
    metadata.await.unwrap().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(requests[1] - requests[0] >= Duration::from_secs(1));
    assert!(requests[1..]
        .windows(2)
        .all(|pair| pair[1] - pair[0] >= Duration::from_millis(240)));
}

#[tokio::test]
async fn long_cooldowns_are_cancellable_and_do_not_block_another_origin() {
    let limited = Fixture::new(
        Router::new().fallback(get(|| async { response(429, Some("3600"), "limited") })),
    )
    .await;
    let healthy =
        Fixture::new(Router::new().fallback(get(|| async { response(200, None, "{}") }))).await;
    let net = limited.net.clone();
    let url = limited.url("/meta");
    let job = Job::default();
    let running = job.clone();
    let request = tokio::spawn(async move { net.json_with_job::<Value>(&url, &running).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while limited.net.retry.origins.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        limited.net.json::<Value>(&healthy.url("/meta")),
    )
    .await
    .unwrap()
    .unwrap();
    job.cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("cancelled"));
    let origins = limited.net.retry.origins.lock().unwrap();
    let state = origins.values().next().unwrap();
    assert!(state.until - Instant::now() > Duration::from_secs(3590));
}

#[tokio::test]
async fn a_rate_limited_file_keeps_its_verified_resume_identity() {
    use sha1::Digest;
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let fixture = Fixture::new(Router::new().route(
        "/file",
        get(move |headers: HeaderMap| {
            let counter = counter.clone();
            async move {
                assert_eq!(headers.get("range").unwrap(), "bytes=3-");
                assert_eq!(headers.get("if-range").unwrap(), "\"fixture\"");
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    response(429, Some("0"), "limited")
                } else {
                    HttpResponse::builder()
                        .status(206)
                        .header("etag", "\"fixture\"")
                        .header("content-range", "bytes 3-5/6")
                        .body(Body::from("lo\n"))
                        .unwrap()
                }
            }
        }),
    ))
    .await;
    let item = fixture.file("/file");
    let key = hex::encode(sha2::Sha256::digest(
        format!("{}:{:?}", item.url, item.checksum).as_bytes(),
    ));
    std::fs::write(fixture.net.partial_dir.join(format!("{key}.part")), b"hel").unwrap();
    storage::write_json(
        &fixture.net.partial_dir.join(format!("{key}.json")),
        &super::super::Partial {
            url: item.url.clone(),
            validator: Some("\"fixture\"".into()),
        },
    )
    .unwrap();
    fixture.net.get(&item, &Job::default()).await.unwrap();
    assert_eq!(std::fs::read(&item.target).unwrap(), b"hello\n");
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn repeated_limits_stop_after_the_retry_budget() {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let fixture = Fixture::new(Router::new().fallback(get(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async { response(429, Some("0"), "limited") }
    })))
    .await;
    let error = fixture
        .net
        .json::<Value>(&fixture.url("/meta"))
        .await
        .unwrap_err();
    assert_eq!(requests.load(Ordering::SeqCst), ATTEMPTS as usize);
    assert!(error.downcast_ref::<RetryableStatus>().is_some());
    assert!(crate::download::unavailable(&error));
}
