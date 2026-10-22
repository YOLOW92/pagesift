//! Concurrent chunk fetcher with adaptive retries, jitter backoff, and range support.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Result of downloading a single media segment.
#[derive(Clone, Debug)]
pub struct FetchedChunk {
    pub uri: String,
    pub data: Vec<u8>,
    pub status_code: u16,
    pub retries_taken: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("Max retries ({0}) exceeded for {1}")]
    RetriesExceeded(u32, String),
    #[error("Segment size ({0} bytes) exceeded maximum allowed ({1} bytes)")]
    SizeExceeded(usize, usize),
}

/// Bounded concurrent chunk fetcher.
#[derive(Clone)]
pub struct ChunkFetcher {
    client: reqwest::Client,
    max_concurrency: usize,
    max_retries: u32,
    max_chunk_bytes: usize,
}

impl ChunkFetcher {
    pub fn new(max_concurrency: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            client,
            max_concurrency: max_concurrency.max(1),
            max_retries: 3,
            max_chunk_bytes: 32 * 1024 * 1024, // 32 MB per chunk max
        }
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Fetch a single chunk with byte-range and retry loop.
    pub async fn fetch_one(
        &self,
        uri: &str,
        byte_range: Option<(u64, u64)>,
    ) -> Result<FetchedChunk, FetchError> {
        let mut retries = 0;
        let mut delay_ms = 100u64;

        loop {
            let mut req = self.client.get(uri);
            if let Some((start, len)) = byte_range {
                let end = start + len - 1;
                req = req.header("Range", format!("bytes={}-{}", start, end));
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if resp.status().is_success() || status == 206 {
                        match resp.bytes().await {
                            Ok(bytes) => {
                                if bytes.len() > self.max_chunk_bytes {
                                    return Err(FetchError::SizeExceeded(
                                        bytes.len(),
                                        self.max_chunk_bytes,
                                    ));
                                }
                                return Ok(FetchedChunk {
                                    uri: uri.to_string(),
                                    data: bytes.to_vec(),
                                    status_code: status,
                                    retries_taken: retries,
                                });
                            }
                            Err(e) => {
                                if retries >= self.max_retries {
                                    return Err(FetchError::Http(e.to_string()));
                                }
                            }
                        }
                    } else if status == 429 || status >= 500 {
                        // Transient server error, retry
                        if retries >= self.max_retries {
                            return Err(FetchError::RetriesExceeded(retries, uri.to_string()));
                        }
                    } else {
                        // Client error (404, 403, etc), don't retry endlessly
                        return Err(FetchError::Http(format!("HTTP status {}", status)));
                    }
                }
                Err(e) => {
                    if retries >= self.max_retries {
                        return Err(FetchError::RetriesExceeded(retries, e.to_string()));
                    }
                }
            }

            retries += 1;
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(2000);
        }
    }

    /// Fetch multiple chunks in parallel with concurrency semaphore.
    pub async fn fetch_all(
        &self,
        tasks: Vec<(String, Option<(u64, u64)>)>,
    ) -> Vec<Result<FetchedChunk, FetchError>> {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrency));
        let mut handles = Vec::with_capacity(tasks.len());

        for (uri, range) in tasks {
            let sem = Arc::clone(&semaphore);
            let fetcher = self.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.map_err(|e| FetchError::Http(e.to_string()))?;
                fetcher.fetch_one(&uri, range).await
            }));
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(res) => results.push(res),
                Err(e) => results.push(Err(FetchError::Http(e.to_string()))),
            }
        }
        results
    }
}
