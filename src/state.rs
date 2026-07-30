//! Lifetime limits and transfer statistics.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Process-wide run flag; cleared by Ctrl+C / lifetime limits.
pub(crate) static CTRL_C_RUNNING: AtomicBool = AtomicBool::new(true);

pub(crate) struct TransferStats {
    pub(crate) downloads: AtomicU64,
    pub(crate) download_bytes: AtomicU64,
    pub(crate) uploads: AtomicU64,
    pub(crate) upload_bytes: AtomicU64,
}

impl TransferStats {
    pub(crate) fn new() -> Self {
        Self {
            downloads: AtomicU64::new(0),
            download_bytes: AtomicU64::new(0),
            uploads: AtomicU64::new(0),
            upload_bytes: AtomicU64::new(0),
        }
    }

    pub(crate) fn record_download(&self, bytes: u64) {
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.download_bytes.fetch_add(bytes, Ordering::SeqCst);
    }

    pub(crate) fn record_upload(&self, bytes: u64) {
        self.uploads.fetch_add(1, Ordering::SeqCst);
        self.upload_bytes.fetch_add(bytes, Ordering::SeqCst);
    }

    pub(crate) fn summary_line(&self) -> String {
        let d = self.downloads.load(Ordering::SeqCst);
        let db = self.download_bytes.load(Ordering::SeqCst);
        let u = self.uploads.load(Ordering::SeqCst);
        let ub = self.upload_bytes.load(Ordering::SeqCst);
        format!(
            "stats: {d} download(s) ({db} bytes), {u} upload(s) ({ub} bytes)"
        )
    }
}

pub(crate) struct LifetimeState {
    pub(crate) one_shot: bool,
    pub(crate) max_downloads: Option<u64>,
    pub(crate) max_uploads: Option<u64>,
    pub(crate) downloads: AtomicU64,
    pub(crate) uploads: AtomicU64,
    pub(crate) started: std::time::Instant,
    pub(crate) expire: Option<Duration>,
}

impl LifetimeState {
    pub(crate) fn new(
        one_shot: bool,
        expire: Option<Duration>,
        max_downloads: Option<u64>,
        max_uploads: Option<u64>,
    ) -> Self {
        Self {
            one_shot,
            max_downloads,
            max_uploads,
            downloads: AtomicU64::new(0),
            uploads: AtomicU64::new(0),
            started: std::time::Instant::now(),
            expire,
        }
    }

    pub(crate) fn expired(&self) -> bool {
        if let Some(d) = self.expire {
            self.started.elapsed() >= d
        } else {
            false
        }
    }

    pub(crate) fn should_stop(&self) -> bool {
        if self.expired() {
            return true;
        }
        if let Some(n) = self.max_downloads {
            if self.downloads.load(Ordering::SeqCst) >= n {
                return true;
            }
        }
        if let Some(n) = self.max_uploads {
            if self.uploads.load(Ordering::SeqCst) >= n {
                return true;
            }
        }
        false
    }

    pub(crate) fn record_download(&self) {
        let n = self.downloads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.one_shot || self.max_downloads.map(|m| n >= m).unwrap_or(false) {
            CTRL_C_RUNNING.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn record_upload(&self) {
        let n = self.uploads.fetch_add(1, Ordering::SeqCst) + 1;
        if self.one_shot || self.max_uploads.map(|m| n >= m).unwrap_or(false) {
            CTRL_C_RUNNING.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn stop_reason(&self) -> Option<&'static str> {
        if self.one_shot
            && (self.downloads.load(Ordering::SeqCst) > 0
                || self.uploads.load(Ordering::SeqCst) > 0)
        {
            return Some("one-shot transfer completed");
        }
        if self.expired() {
            return Some("expire duration reached");
        }
        if let Some(n) = self.max_downloads {
            if self.downloads.load(Ordering::SeqCst) >= n {
                return Some("max-downloads reached");
            }
        }
        if let Some(n) = self.max_uploads {
            if self.uploads.load(Ordering::SeqCst) >= n {
                return Some("max-uploads reached");
            }
        }
        None
    }
}


