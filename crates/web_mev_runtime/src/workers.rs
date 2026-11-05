//! Background tasks that stop together when the server does.

use std::{future::Future, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

/// A set of background tasks sharing one stop signal. Each task receives a
/// `watch::Receiver<bool>` that turns `true` when it should finish.
#[derive(Debug)]
pub struct Workers {
    stop: watch::Sender<bool>,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
}

impl Default for Workers {
    fn default() -> Self {
        Self::new()
    }
}

impl Workers {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stop: watch::channel(false).0,
            tasks: Vec::new(),
        }
    }

    /// Starts a task. `name` appears in the logs if it panics or overruns.
    pub fn spawn<F, Fut>(&mut self, name: &'static str, worker: F)
    where
        F: FnOnce(watch::Receiver<bool>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let task = tokio::spawn(worker(self.stop.subscribe()));
        self.tasks.push((name, task));
    }

    /// Asks every task to stop and waits for them, up to `grace` in all. A task
    /// still running after that is aborted. Returns how many tasks panicked or
    /// had to be aborted.
    pub async fn shutdown(self, grace: Duration) -> usize {
        let _ = self.stop.send(true);
        let deadline = tokio::time::Instant::now() + grace;
        let mut failed = 0;
        for (name, mut task) in self.tasks {
            match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    failed += 1;
                    tracing::error!(worker = name, %error, "worker failed");
                }
                Err(_) => {
                    failed += 1;
                    tracing::warn!(worker = name, "worker overran its grace period; aborting");
                    task.abort();
                }
            }
        }
        failed
    }
}

/// Sleeps for `duration` unless stopped first. Returns `true` when the worker
/// should stop, which includes the sender having gone away.
pub async fn sleep_or_stop(stop: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    if *stop.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        _ = stop.changed() => true,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use super::{Workers, sleep_or_stop};

    #[tokio::test]
    async fn stop_reaches_workers_and_shutdown_waits() {
        let finished = Arc::new(AtomicBool::new(false));
        let mut workers = Workers::new();
        let flag = finished.clone();
        workers.spawn("loop", move |mut stop| async move {
            while !sleep_or_stop(&mut stop, Duration::from_secs(3600)).await {}
            tokio::time::sleep(Duration::from_millis(10)).await;
            flag.store(true, Ordering::SeqCst);
        });
        assert_eq!(workers.shutdown(Duration::from_secs(5)).await, 0);
        assert!(finished.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn overrunning_worker_is_counted_and_aborted() {
        let mut workers = Workers::new();
        workers.spawn("stuck", |_stop| async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        assert_eq!(workers.shutdown(Duration::from_secs(1)).await, 1);
    }

    #[tokio::test]
    async fn panicking_worker_is_counted() {
        let mut workers = Workers::new();
        workers.spawn("panics", |_stop| async {
            panic!("boom");
        });
        tokio::task::yield_now().await;
        assert_eq!(workers.shutdown(Duration::from_secs(5)).await, 1);
    }

    #[tokio::test]
    async fn sleep_returns_true_when_already_stopped() {
        let (sender, mut stop) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();
        assert!(sleep_or_stop(&mut stop, Duration::from_secs(3600)).await);
    }

    #[tokio::test(start_paused = true)]
    async fn sleep_returns_false_when_it_runs_out() {
        let (_sender, mut stop) = tokio::sync::watch::channel(false);
        assert!(!sleep_or_stop(&mut stop, Duration::from_secs(1)).await);
    }
}
