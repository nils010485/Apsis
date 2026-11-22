use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

/// Default maximum time `db_query` and `job_status` wait inline before returning `running` (20s < 30s MCP client timeouts).
pub const DEFAULT_INLINE_WAIT_MS: u64 = 20_000;
const FINISHED_JOB_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub enum JobStatus {
    Running,
    Completed(String),
    Failed(String),
    Cancelled,
}

struct JobEntry {
    preset: String,
    rx: watch::Receiver<JobStatus>,
    tx: watch::Sender<JobStatus>,
    handle: JoinHandle<()>,
    finished_at: Option<Instant>,
}

#[derive(Default)]
pub struct JobManager {
    jobs: Arc<Mutex<HashMap<String, JobEntry>>>,
}

pub enum WaitOutcome {
    Running { job_id: String },
    Completed(String),
    Failed(String),
    Cancelled,
}

impl JobManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve_wait_duration(wait_ms: Option<u64>) -> Duration {
        let ms = wait_ms.unwrap_or_else(|| {
            std::env::var("APSIS_INLINE_WAIT_MS")
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(DEFAULT_INLINE_WAIT_MS)
        });
        Duration::from_millis(ms)
    }

    pub async fn run_with_wait<F>(&self, preset: String, wait: Duration, fut: F) -> WaitOutcome
    where
        F: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.prune_expired().await;

        let job_id = Uuid::new_v4().to_string();
        let (tx, mut rx) = watch::channel(JobStatus::Running);
        let jobs_ref = Arc::clone(&self.jobs);
        let task_job_id = job_id.clone();
        let task_tx = tx.clone();

        let handle = tokio::spawn(async move {
            let outcome = fut.await;
            let status = match outcome {
                Ok(text) => JobStatus::Completed(text),
                Err(err) => JobStatus::Failed(err),
            };
            let _ = task_tx.send(status);
            let mut guard = jobs_ref.lock().await;
            if let Some(entry) = guard.get_mut(&task_job_id) {
                entry.finished_at = Some(Instant::now());
            }
        });

        {
            let mut guard = self.jobs.lock().await;
            guard.insert(
                job_id.clone(),
                JobEntry {
                    preset,
                    rx: rx.clone(),
                    tx,
                    handle,
                    finished_at: None,
                },
            );
        }

        self.wait_inner(&job_id, &mut rx, wait).await
    }

    pub async fn poll(
        &self,
        job_id: &str,
        wait: Duration,
        cancel: bool,
    ) -> Option<(String, WaitOutcome)> {
        self.prune_expired().await;

        let (preset, mut rx) = {
            let mut guard = self.jobs.lock().await;
            if cancel {
                let entry = guard.remove(job_id)?;
                entry.handle.abort();
                let _ = entry.tx.send(JobStatus::Cancelled);
                return Some((entry.preset, WaitOutcome::Cancelled));
            }
            let entry = guard.get(job_id)?;
            (entry.preset.clone(), entry.rx.clone())
        };

        let outcome = self.wait_inner(job_id, &mut rx, wait).await;
        Some((preset, outcome))
    }

    pub async fn cancel_for_preset(&self, preset: &str) {
        let mut guard = self.jobs.lock().await;
        let ids: Vec<String> = guard
            .iter()
            .filter(|(_, entry)| entry.preset == preset)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(entry) = guard.remove(&id) {
                entry.handle.abort();
                let _ = entry.tx.send(JobStatus::Cancelled);
            }
        }
    }

    async fn wait_inner(
        &self,
        job_id: &str,
        rx: &mut watch::Receiver<JobStatus>,
        wait: Duration,
    ) -> WaitOutcome {
        let current = match tokio::time::timeout(wait, async {
            rx.wait_for(|status| !matches!(status, JobStatus::Running))
                .await
                .map(|guard| guard.clone())
        })
        .await
        {
            Ok(Ok(status)) => status,
            Ok(Err(_)) | Err(_) => rx.borrow().clone(),
        };

        match current {
            JobStatus::Running => WaitOutcome::Running {
                job_id: job_id.to_string(),
            },
            JobStatus::Completed(text) => {
                self.jobs.lock().await.remove(job_id);
                WaitOutcome::Completed(text)
            }
            JobStatus::Failed(err) => {
                self.jobs.lock().await.remove(job_id);
                WaitOutcome::Failed(err)
            }
            JobStatus::Cancelled => {
                self.jobs.lock().await.remove(job_id);
                WaitOutcome::Cancelled
            }
        }
    }

    async fn prune_expired(&self) {
        let now = Instant::now();
        let mut guard = self.jobs.lock().await;
        guard.retain(|_, entry| {
            entry
                .finished_at
                .is_none_or(|done_at| now.saturating_duration_since(done_at) < FINISHED_JOB_TTL)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_inline_when_fast_and_job_id_when_slow() {
        let mgr = JobManager::new();

        let fast = mgr
            .run_with_wait("pg".to_string(), Duration::from_millis(500), async {
                Ok("fast-ok".to_string())
            })
            .await;
        assert!(matches!(fast, WaitOutcome::Completed(ref s) if s == "fast-ok"));

        let slow = mgr
            .run_with_wait("mongo".to_string(), Duration::from_millis(20), async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok("slow-ok".to_string())
            })
            .await;
        let job_id = match slow {
            WaitOutcome::Running { job_id } => job_id,
            _ => panic!("expected Running outcome"),
        };

        let (_, polled) = mgr
            .poll(&job_id, Duration::from_millis(500), false)
            .await
            .unwrap();
        assert!(matches!(polled, WaitOutcome::Completed(ref s) if s == "slow-ok"));
        assert!(
            mgr.poll(&job_id, Duration::from_millis(10), false)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancel_aborts_running_job() {
        let mgr = JobManager::new();
        let res = mgr
            .run_with_wait("mongo".to_string(), Duration::from_millis(10), async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok("never".to_string())
            })
            .await;
        let job_id = match res {
            WaitOutcome::Running { job_id } => job_id,
            _ => panic!("expected Running"),
        };

        let (_, cancelled) = mgr
            .poll(&job_id, Duration::from_millis(0), true)
            .await
            .unwrap();
        assert!(matches!(cancelled, WaitOutcome::Cancelled));
    }
}
