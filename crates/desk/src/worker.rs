//! A thread that owns an accessibility backend, since UIA and AX objects stay on the thread that made them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::lock;

type Job<B> = Box<dyn FnOnce(Result<&mut B, &str>) + Send>;

/// Runs calls on the backend that `open` makes on the worker thread.
pub struct Worker<B> {
    open: fn() -> Result<B, String>,
    jobs: Mutex<(u64, Sender<Job<B>>)>,
}

impl<B: 'static> Worker<B> {
    /// Starts the worker thread; `open` runs there, again before each call while it fails.
    pub fn new(open: fn() -> Result<B, String>) -> Self {
        Self {
            open,
            jobs: Mutex::new((0, spawn(open))),
        }
    }

    /// Runs `f` on the backend and waits at most `limit` for its result; `hold` drops when `f` ends, even after the limit.
    pub async fn run<R, F, H>(&self, limit: Duration, hold: H, f: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: FnOnce(&mut B) -> Result<R, String> + Send + 'static,
        H: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let _cancel = Cancel(dropped.clone());
        let job: Job<B> = Box::new(move |backend| {
            if !dropped.load(Ordering::Relaxed) {
                let _ = tx.send(backend.map_err(str::to_owned).and_then(f));
            }
            drop(hold);
        });
        let (mut sent_on, sender) = self.sender();
        if let Err(back) = sender.send(job) {
            self.restart(sent_on);
            let (generation, sender) = self.sender();
            sent_on = generation;
            sender
                .send(back.0)
                .map_err(|_| "the accessibility worker is gone".to_owned())?;
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.restart(sent_on);
                Err("the accessibility worker stopped".to_owned())
            }
            Err(_) => {
                self.restart(sent_on);
                Err(format!(
                    "no accessibility answer within {} ms; the app may hang, and an action sent to it may still take effect once it answers",
                    limit.as_millis()
                ))
            }
        }
    }

    pub fn post<F>(&self, f: F)
    where
        F: FnOnce(&mut B) + Send + 'static,
    {
        let job: Job<B> = Box::new(move |backend| {
            if let Ok(b) = backend {
                f(b);
            }
        });
        let _ = self.sender().1.send(job);
    }

    fn sender(&self) -> (u64, Sender<Job<B>>) {
        let jobs = lock(&self.jobs);
        (jobs.0, jobs.1.clone())
    }

    fn restart(&self, generation: u64) {
        let mut jobs = lock(&self.jobs);
        if jobs.0 == generation {
            *jobs = (generation + 1, spawn(self.open));
        }
    }
}

/// Sets its flag when dropped, as when a stop drops the call that holds it.
pub(crate) struct Cancel(pub(crate) Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn spawn<B: 'static>(open: fn() -> Result<B, String>) -> Sender<Job<B>> {
    let (tx, rx) = channel::<Job<B>>();
    std::thread::spawn(move || {
        let mut backend = open();
        for job in rx {
            if backend.is_err() {
                backend = open();
            }
            job(backend.as_mut().map_err(|e| e.as_str()));
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn calls_run_on_one_backend_in_order() {
        let w = Worker::new(|| Ok(Vec::<u32>::new()));
        let limit = Duration::from_secs(1);
        w.run(limit, (), |v| {
            v.push(1);
            Ok(())
        })
        .await
        .unwrap();
        w.run(limit, (), |v| {
            v.push(2);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(w.run(limit, (), |v| Ok(v.clone())).await.unwrap(), [1, 2]);
    }

    #[tokio::test]
    async fn a_hung_call_leaves_a_fresh_worker_behind() {
        let w = Worker::new(|| Ok(Vec::<u32>::new()));
        w.run(Duration::from_secs(1), (), |v| {
            v.push(1);
            Ok(())
        })
        .await
        .unwrap();
        let err = w
            .run(Duration::from_millis(50), (), |_| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.contains("within 50 ms"), "{err}");
        let fresh = w.run(Duration::from_secs(1), (), |v| Ok(v.len())).await;
        assert_eq!(fresh.unwrap(), 0);
    }

    #[tokio::test]
    async fn what_a_hung_call_holds_stays_held_until_the_call_ends() {
        let w = Worker::new(|| Ok(()));
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        let held = turn.clone().lock_owned().await;
        let err = w
            .run(Duration::from_millis(50), held, |_| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.contains("within 50 ms"), "{err}");
        assert!(turn.try_lock().is_err());
        let freed = tokio::time::timeout(Duration::from_secs(2), turn.lock()).await;
        assert!(freed.is_ok());
    }

    #[tokio::test]
    async fn a_queued_call_that_times_out_before_it_starts_never_runs() {
        let w = Arc::new(Worker::new(|| Ok(())));
        let hung = Arc::clone(&w);
        let hang = tokio::spawn(async move {
            hung.run(Duration::from_secs(2), (), |_| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let ran = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&ran);
        let err = w
            .run(Duration::from_millis(50), (), move |_| {
                lock(&seen).push(7);
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.contains("within 50 ms"), "{err}");
        hang.await.unwrap().unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(lock(&ran).is_empty(), "{:?}", lock(&ran));
    }

    #[tokio::test]
    async fn callers_queued_behind_one_hang_restart_the_backend_once() {
        static OPENS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let w = Arc::new(Worker::new(|| {
            OPENS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let mut calls = Vec::new();
        for i in 0..5 {
            let w = Arc::clone(&w);
            calls.push(tokio::spawn(async move {
                w.run(Duration::from_millis(100), (), move |_| {
                    if i == 0 {
                        std::thread::sleep(Duration::from_millis(400));
                    }
                    Ok(())
                })
                .await
            }));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for call in calls {
            let _ = call.await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(OPENS.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_backend_that_fails_to_open_fails_each_call() {
        let w: Worker<u32> = Worker::new(|| Err("no accessibility permission".into()));
        let err = w
            .run(Duration::from_secs(1), (), |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(err, "no accessibility permission");
    }
}
