//! Downloading: connection pooling, bounded concurrency, retry, resume, and
//! verification on the way into the content-addressed store.
//!
//! Four decisions shape this module:
//!
//! - **One client, process-wide.** `reqwest::Client` owns the connection pool;
//!   constructing one per download would give up keep-alive and HTTP/2
//!   multiplexing on the very workload that needs them most - an asset index
//!   is three thousand small files from one host.
//! - **Concurrency is bounded by a semaphore**, not by spawning freely. Three
//!   thousand simultaneous sockets is a way to get rate-limited, not a way to
//!   go faster.
//! - **The body is streamed to disk**, never buffered whole. The client jar is
//!   31 MB; buffering it would show up directly in the idle memory budget.
//! - **Nothing is trusted without its hash.** Verification happens in
//!   `ds-store` on the way in, so a truncated transfer or a bad mirror cannot
//!   become a file the JVM later fails on for reasons nobody can explain.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ds_store::{Algorithm, Store, StoreError};
use futures_util::StreamExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Semaphore;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("network error fetching {url}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("{url} returned HTTP {status}")]
    Http { url: String, status: u16 },

    #[error("io error writing {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error("gave up on {url} after {attempts} attempts")]
    Exhausted {
        url: String,
        attempts: u32,
        #[source]
        source: Box<NetError>,
    },
}

impl NetError {
    /// Whether another attempt could plausibly succeed.
    ///
    /// Deliberately narrow. Retrying a 404 just delays the same failure, and
    /// retrying a hash mismatch re-downloads a file the server is serving
    /// wrongly.
    fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Http { status, .. } => *status >= 500 || *status == 408 || *status == 429,
            Self::Io { .. } | Self::Store(_) | Self::Exhausted { .. } => false,
        }
    }
}

type Result<T> = std::result::Result<T, NetError>;

/// One file to fetch, with the hash it must match.
#[derive(Debug, Clone)]
pub struct Artifact {
    pub url: String,
    pub hash: String,
    pub algorithm: Algorithm,
    /// Expected size, used only for progress reporting. Not a correctness
    /// check - the hash is.
    pub size: u64,
}

impl Artifact {
    pub fn sha1(url: impl Into<String>, hash: impl Into<String>, size: u64) -> Self {
        Self {
            url: url.into(),
            hash: hash.into(),
            algorithm: Algorithm::Sha1,
            size,
        }
    }
}

/// How far along a batch is.
///
/// Emitted raw and often; coalescing for the UI happens at the app boundary,
/// because a channel is cheap and a webview repaint is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub completed: u64,
    pub total: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Downloader {
    client: reqwest::Client,
    permits: Arc<Semaphore>,
    max_attempts: u32,
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new(Self::default_concurrency(), 4)
    }
}

impl Downloader {
    pub fn new(concurrency: usize, max_attempts: u32) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("deepslate/", env!("CARGO_PKG_VERSION")))
            // Applies to the whole request including the body, so it has to be
            // generous enough for a 31 MB jar on a slow connection.
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_default();

        Self {
            client,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            max_attempts: max_attempts.max(1),
        }
    }

    /// `cpus * 2`, capped at 16.
    ///
    /// Downloads are latency-bound rather than CPU-bound, so more than cores
    /// helps - but past about sixteen the gain disappears and the odds of being
    /// rate-limited do not.
    fn default_concurrency() -> usize {
        if let Some(override_value) = std::env::var("DEEPSLATE_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
        {
            return override_value;
        }
        std::thread::available_parallelism()
            .map(|n| n.get() * 2)
            .unwrap_or(8)
            .min(16)
    }

    /// Fetch one artifact into the store, or return its existing path.
    ///
    /// A hash already present short-circuits before any network call, which is
    /// what makes a second launch of the same version instant.
    pub async fn fetch(&self, store: &Store, artifact: &Artifact) -> Result<PathBuf> {
        if store.contains(&artifact.hash, artifact.algorithm) {
            return Ok(store.path_for(&artifact.hash, artifact.algorithm)?);
        }

        let _permit = self.permits.acquire().await;

        let mut attempt = 0;
        let mut last: Option<NetError> = None;

        while attempt < self.max_attempts {
            attempt += 1;
            match self.attempt(store, artifact).await {
                Ok(path) => return Ok(path),
                // A 404, or content that failed verification, will not improve
                // with another go.
                Err(error) if !error.is_retryable() => return Err(error),
                Err(error) => {
                    last = Some(error);
                    if attempt < self.max_attempts {
                        // Exponential backoff without jitter: concurrency is
                        // already capped by the semaphore, so there is no herd
                        // here to scatter.
                        let wait = Duration::from_millis(200 * (1 << (attempt - 1).min(5)));
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }

        Err(NetError::Exhausted {
            url: artifact.url.clone(),
            attempts: attempt,
            source: Box::new(last.unwrap_or(NetError::Http {
                url: artifact.url.clone(),
                status: 0,
            })),
        })
    }

    async fn attempt(&self, store: &Store, artifact: &Artifact) -> Result<PathBuf> {
        // The staging directory is created once by Store::open; creating it per
        // file cost 16% of total worker time for no benefit.
        let staging = store.staging_path_for(&artifact.hash, artifact.algorithm)?;

        // Resume from whatever a previous attempt managed to write.
        let already = tokio::fs::metadata(&staging)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let mut request = self.client.get(&artifact.url);
        if already > 0 {
            request = request.header("Range", format!("bytes={already}-"));
        }

        let response = request.send().await.map_err(|source| NetError::Transport {
            url: artifact.url.clone(),
            source,
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(NetError::Http {
                url: artifact.url.clone(),
                status: status.as_u16(),
            });
        }

        // A server that ignores Range answers 200 with the whole file. Appending
        // that to what we already have would produce a corrupt double-length
        // file, so start over instead.
        let append = already > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&staging)
            .await
            .map_err(|source| NetError::Io {
                path: staging.clone(),
                source,
            })?;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| NetError::Transport {
                url: artifact.url.clone(),
                source,
            })?;
            file.write_all(&chunk)
                .await
                .map_err(|source| NetError::Io {
                    path: staging.clone(),
                    source,
                })?;
        }
        file.flush().await.map_err(|source| NetError::Io {
            path: staging.clone(),
            source,
        })?;
        drop(file);

        // Verification and the atomic move both belong to the store.
        let path = store.insert_file(&staging, &artifact.hash, artifact.algorithm)?;

        // No cleanup pass: insert_file consumes the staging file, by rename or
        // by copy-then-remove. The previous unconditional remove_file always
        // failed - the file had already been renamed away - and cost 7.2ms of
        // worker time per file doing it.

        Ok(path)
    }

    /// Fetch a document that has no hash to verify against.
    ///
    /// The top-level version list has no published hash, so it can only be
    /// fetched and trusted over TLS. Per-version JSONs DO have one, published
    /// in that list - those should go through [`Self::fetch`] instead so they
    /// are verified and cached like any other artifact.
    ///
    /// Retries on the same narrow set as [`Self::fetch`]: 5xx, 408 and 429.
    pub async fn fetch_text(&self, url: &str) -> Result<String> {
        let _permit = self.permits.acquire().await;

        let mut attempt = 0;
        let mut last: Option<NetError> = None;

        while attempt < self.max_attempts {
            attempt += 1;
            match self.attempt_text(url).await {
                Ok(body) => return Ok(body),
                Err(error) if !error.is_retryable() => return Err(error),
                Err(error) => {
                    last = Some(error);
                    if attempt < self.max_attempts {
                        let wait = Duration::from_millis(200 * (1 << (attempt - 1).min(5)));
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        }

        Err(NetError::Exhausted {
            url: url.to_owned(),
            attempts: attempt,
            source: Box::new(last.unwrap_or(NetError::Http {
                url: url.to_owned(),
                status: 0,
            })),
        })
    }

    async fn attempt_text(&self, url: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|source| NetError::Transport {
                url: url.to_owned(),
                source,
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(NetError::Http {
                url: url.to_owned(),
                status: status.as_u16(),
            });
        }

        response.text().await.map_err(|source| NetError::Transport {
            url: url.to_owned(),
            source,
        })
    }

    /// Fetch many artifacts concurrently, reporting progress as each lands.
    ///
    /// Returns on the first failure rather than pressing on: a version missing
    /// one library is not launchable, so continuing would only produce a longer
    /// wait before the same error.
    pub async fn fetch_all<F>(
        &self,
        store: &Store,
        artifacts: &[Artifact],
        mut on_progress: F,
    ) -> Result<Vec<PathBuf>>
    where
        F: FnMut(Progress),
    {
        let total = artifacts.len() as u64;
        let mut completed = 0;
        let mut bytes = 0;
        let mut paths = Vec::with_capacity(artifacts.len());

        let mut pending = futures_util::stream::iter(artifacts.iter().map(|artifact| async move {
            let path = self.fetch(store, artifact).await;
            (artifact.size, path)
        }))
        .buffer_unordered(self.permits.available_permits().max(1));

        while let Some((size, result)) = pending.next().await {
            paths.push(result?);
            completed += 1;
            bytes += size;
            on_progress(Progress {
                completed,
                total,
                bytes,
            });
        }

        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path as path_matcher};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HELLO_SHA1: &str = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn downloader() -> Downloader {
        Downloader::new(4, 3)
    }

    #[tokio::test]
    async fn fetches_verifies_and_stores() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/a.jar"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);

        let path = downloader().fetch(&store, &artifact).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert!(store.contains(HELLO_SHA1, Algorithm::Sha1));
    }

    /// The whole point: a server serving the wrong bytes must not poison the
    /// cache.
    #[tokio::test]
    async fn content_that_fails_verification_is_not_stored() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"corrupted".to_vec()))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);

        let err = downloader().fetch(&store, &artifact).await.unwrap_err();
        assert!(matches!(err, NetError::Store(_)), "{err:?}");
        assert!(!store.contains(HELLO_SHA1, Algorithm::Sha1));
    }

    /// An object already in the store must not touch the network at all - this
    /// is what makes relaunching a version instant.
    #[tokio::test]
    async fn an_already_stored_object_makes_no_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let (_dir, store) = store();
        store.insert(b"hello", HELLO_SHA1, Algorithm::Sha1).unwrap();

        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);
        downloader().fetch(&store, &artifact).await.unwrap();
    }

    #[tokio::test]
    async fn a_server_error_is_retried_and_can_succeed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);

        downloader().fetch(&store, &artifact).await.unwrap();
        assert!(store.contains(HELLO_SHA1, Algorithm::Sha1));
    }

    /// Retrying a 404 only delays the same failure.
    #[tokio::test]
    async fn a_missing_file_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifact = Artifact::sha1(format!("{}/gone.jar", server.uri()), HELLO_SHA1, 5);

        let err = downloader().fetch(&store, &artifact).await.unwrap_err();
        assert!(matches!(err, NetError::Http { status: 404, .. }), "{err:?}");
    }

    #[tokio::test]
    async fn persistent_failure_reports_how_many_attempts_were_made() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);

        let err = Downloader::new(4, 2)
            .fetch(&store, &artifact)
            .await
            .unwrap_err();
        match err {
            NetError::Exhausted { attempts, .. } => assert_eq!(attempts, 2),
            other => panic!("expected Exhausted, got {other:?}"),
        }
    }

    /// A partial file from an interrupted run is continued, not restarted.
    #[tokio::test]
    async fn an_interrupted_download_resumes_with_a_range_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("Range", "bytes=2-"))
            .respond_with(ResponseTemplate::new(206).set_body_bytes(b"llo".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let staging = store.staging_path_for(HELLO_SHA1, Algorithm::Sha1).unwrap();
        std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
        std::fs::write(&staging, b"he").unwrap();

        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);
        let path = downloader().fetch(&store, &artifact).await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    /// A server that ignores Range answers 200 with the whole body. Appending
    /// that would produce a double-length corrupt file.
    #[tokio::test]
    async fn a_server_ignoring_range_restarts_rather_than_appending() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let staging = store.staging_path_for(HELLO_SHA1, Algorithm::Sha1).unwrap();
        std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
        std::fs::write(&staging, b"he").unwrap();

        let artifact = Artifact::sha1(format!("{}/a.jar", server.uri()), HELLO_SHA1, 5);
        let path = downloader().fetch(&store, &artifact).await.unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"hello",
            "the ignored range produced a corrupt file"
        );
    }

    #[tokio::test]
    async fn a_batch_reports_progress_for_every_artifact() {
        let server = MockServer::start().await;
        for (name, body) in [("a", "hello"), ("b", "world"), ("c", "there")] {
            Mock::given(method("GET"))
                .and(path_matcher(format!("/{name}")))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_bytes().to_vec()))
                .mount(&server)
                .await;
        }

        let (_dir, store) = store();
        let artifacts = vec![
            Artifact::sha1(format!("{}/a", server.uri()), HELLO_SHA1, 5),
            Artifact::sha1(
                format!("{}/b", server.uri()),
                "7c211433f02071597741e6ff5a8ea34789abbf43",
                5,
            ),
            Artifact::sha1(
                format!("{}/c", server.uri()),
                "490528f36debf7c15cea5e9a9d1ea024cf6b2921",
                5,
            ),
        ];

        let mut seen = Vec::new();
        let paths = downloader()
            .fetch_all(&store, &artifacts, |p| seen.push(p))
            .await
            .unwrap();

        assert_eq!(paths.len(), 3);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen.last().unwrap().completed, 3);
        assert_eq!(seen.last().unwrap().total, 3);
        assert_eq!(seen.last().unwrap().bytes, 15);
    }

    /// One missing library means the version cannot launch, so the batch stops
    /// rather than spending time on the rest.
    #[tokio::test]
    async fn a_batch_fails_fast_on_a_missing_artifact() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let (_dir, store) = store();
        let artifacts = vec![Artifact::sha1(
            format!("{}/gone", server.uri()),
            HELLO_SHA1,
            5,
        )];

        assert!(downloader()
            .fetch_all(&store, &artifacts, |_| {})
            .await
            .is_err());
    }
}
