//! Tokio contains unwinding panics; this boundary gives the UI a stable, private error.
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

pub struct Runtime(tokio::runtime::Runtime);
impl From<tokio::runtime::Runtime> for Runtime {
    fn from(runtime: tokio::runtime::Runtime) -> Self {
        Self(runtime)
    }
}
impl Runtime {
    pub fn spawn<F>(&self, future: F) -> Task<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Task(self.0.spawn(future))
    }
    pub fn spawn_blocking<F, T>(&self, work: F) -> Task<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        Task(self.0.spawn_blocking(work))
    }
}
pub struct Task<T>(tokio::task::JoinHandle<T>);
impl<T> Task<T> {
    /// Stops the task at its next await point, dropping what it holds (its connections too).
    pub fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.0.abort_handle()
    }
}
#[derive(Debug, Clone, Copy)]
pub enum WorkerError {
    Panicked,
    Cancelled,
}
impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Panicked => "A background task failed unexpectedly. Reload or reconnect before retrying; some changes may have been saved.",
            Self::Cancelled => "The background task was cancelled.",
        })
    }
}
impl std::error::Error for WorkerError {}
impl<T> Future for Task<T> {
    type Output = Result<T, WorkerError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(|r| {
            r.map_err(|e| {
                if e.is_panic() {
                    WorkerError::Panicked
                } else {
                    WorkerError::Cancelled
                }
            })
        })
    }
}
/// A panic's payload can contain credentials, SQL or parameter values. Never print it.
/// Tokio's default unwind handling remains enabled (no abort-on-panic release profile).
pub fn install_private_panic_hook() {
    std::panic::set_hook(Box::new(|_| {
        eprintln!("Telecat: unexpected internal failure (details withheld).");
        // A UI-thread panic aborts the app; keep the user's staged edits on disk first.
        // Worker panics become errors (see `Runtime`) and lose nothing.
        let ui_thread = std::thread::current().name() == Some("main");
        if let Some(root) = ui_thread
            .then(crate::store::Profile::default_path)
            .and_then(Result::ok)
        {
            if let Some(path) = crate::recovery::write(&root) {
                eprintln!(
                    "Telecat: unsaved staged changes written to {}",
                    path.display()
                );
            }
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_panics_are_errors_and_runtime_remains_usable() {
        let runtime: Runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
            .into();
        let a = runtime.spawn(async {
            panic!("private SQL parameter");
        });
        let b = runtime.spawn_blocking(|| panic!("private password"));
        for error in [
            futures::executor::block_on(a).unwrap_err(),
            futures::executor::block_on(b).unwrap_err(),
        ] {
            assert!(matches!(error, WorkerError::Panicked));
            assert!(!error.to_string().contains("private"));
            assert!(error.to_string().contains("Reload or reconnect"));
        }
        assert_eq!(
            futures::executor::block_on(runtime.spawn(async { 42 })).unwrap(),
            42
        );
    }
}
