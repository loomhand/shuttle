//! A thread that owns an accessibility backend, since UIA and AX objects stay on the thread that made them;
//! a call that outlives its limit leaves that thread behind and the next call starts a fresh one.

use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use tokio::sync::oneshot;

type Job<B> = Box<dyn FnOnce(Result<&mut B, &str>) + Send>;

/// Runs calls on the backend that `open` makes on the worker thread.
pub struct Worker<B> {
    open: fn() -> Result<B, String>,
    jobs: Mutex<Sender<Job<B>>>,
}

impl<B: 'static> Worker<B> {
    /// Starts the worker thread; `open` runs there, again before each call while it fails.
    pub fn new(open: fn() -> Result<B, String>) -> Self {
        Self {
            open,
            jobs: Mutex::new(spawn(open)),
        }
    }

    /// Runs `f` on the backend and waits at most `limit` for its result.
    pub async fn run<R, F>(&self, limit: Duration, f: F) -> Result<R, String>
    where
        R: Send + 'static,
        F: FnOnce(&mut B) -> Result<R, String> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job<B> = Box::new(move |backend| {
            let _ = tx.send(backend.map_err(str::to_owned).and_then(f));
        });
        if let Err(back) = self.sender().send(job) {
            self.restart();
            self.sender()
                .send(back.0)
                .map_err(|_| "the accessibility worker is gone".to_owned())?;
        }
        match tokio::time::timeout(limit, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.restart();
                Err("the accessibility worker stopped".to_owned())
            }
            Err(_) => {
                self.restart();
                Err(format!(
                    "no accessibility answer within {} ms; the app may hang",
                    limit.as_millis()
                ))
            }
        }
    }

    fn sender(&self) -> Sender<Job<B>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn restart(&self) {
        *self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = spawn(self.open);
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
    use super::*;

    #[tokio::test]
    async fn calls_run_on_one_backend_in_order() {
        let w = Worker::new(|| Ok(Vec::<u32>::new()));
        let limit = Duration::from_secs(1);
        w.run(limit, |v| {
            v.push(1);
            Ok(())
        })
        .await
        .unwrap();
        w.run(limit, |v| {
            v.push(2);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(w.run(limit, |v| Ok(v.clone())).await.unwrap(), [1, 2]);
    }

    #[tokio::test]
    async fn a_hung_call_leaves_a_fresh_worker_behind() {
        let w = Worker::new(|| Ok(Vec::<u32>::new()));
        w.run(Duration::from_secs(1), |v| {
            v.push(1);
            Ok(())
        })
        .await
        .unwrap();
        let err = w
            .run(Duration::from_millis(50), |_| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(err.contains("within 50 ms"), "{err}");
        let fresh = w.run(Duration::from_secs(1), |v| Ok(v.len())).await;
        assert_eq!(fresh.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_backend_that_fails_to_open_fails_each_call() {
        let w: Worker<u32> = Worker::new(|| Err("no accessibility permission".into()));
        let err = w.run(Duration::from_secs(1), |_| Ok(())).await.unwrap_err();
        assert_eq!(err, "no accessibility permission");
    }
}
