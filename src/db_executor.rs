//! One bounded owner thread for HTTP-facing SQLite work.

use std::{
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;
use tokio::sync::oneshot;

use crate::{Store, StoreError};

const DEFAULT_QUEUE_CAPACITY: usize = 64;
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(6);

type Job = Box<dyn FnOnce(&mut Store) + Send + 'static>;

enum Command {
    Run(Job),
    Shutdown,
}

#[derive(Debug, Error)]
pub enum DbExecutorError {
    #[error("could not open the HTTP database owner: {0}")]
    Open(#[source] StoreError),
    #[error("HTTP database command failed: {0}")]
    Store(#[from] StoreError),
    #[error("HTTP database command queue is full")]
    QueueFull,
    #[error("HTTP database executor is shutting down")]
    Shutdown,
    #[error("HTTP database executor thread or command panicked")]
    Panicked,
    #[error("could not start HTTP database executor thread: {0}")]
    Thread(#[source] io::Error),
    #[error("HTTP database executor did not finish its draining shutdown within the bound")]
    ShutdownTimedOut,
}

#[derive(Debug)]
struct Inner {
    sender: SyncSender<Command>,
    accepting: AtomicBool,
    shutdown_sent: AtomicBool,
    shutdown_lock: Mutex<()>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Debug, Clone)]
pub struct DbExecutor {
    inner: Arc<Inner>,
}

impl DbExecutor {
    pub fn start(database: PathBuf) -> Result<Self, DbExecutorError> {
        Self::start_with_capacity(database, DEFAULT_QUEUE_CAPACITY)
    }

    fn start_with_capacity(database: PathBuf, capacity: usize) -> Result<Self, DbExecutorError> {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let (started_sender, started_receiver) = mpsc::sync_channel(1);
        let join = thread::Builder::new()
            .name("bokkie-http-db".to_owned())
            .spawn(move || {
                let mut store = match Store::open_compatible(database) {
                    Ok(store) => {
                        let _ = started_sender.send(Ok(()));
                        store
                    }
                    Err(error) => {
                        let _ = started_sender.send(Err(error));
                        return;
                    }
                };
                while let Ok(command) = receiver.recv() {
                    match command {
                        Command::Run(job) => job(&mut store),
                        Command::Shutdown => break,
                    }
                }
            })
            .map_err(DbExecutorError::Thread)?;
        match started_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(Inner {
                    sender,
                    accepting: AtomicBool::new(true),
                    shutdown_sent: AtomicBool::new(false),
                    shutdown_lock: Mutex::new(()),
                    join: Mutex::new(Some(join)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(DbExecutorError::Open(error))
            }
            Err(_) => {
                let _ = join.join();
                Err(DbExecutorError::Panicked)
            }
        }
    }

    pub async fn execute<T, F>(&self, operation: F) -> Result<T, DbExecutorError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T, StoreError> + Send + 'static,
    {
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(DbExecutorError::Shutdown);
        }
        let (result_sender, result_receiver) = oneshot::channel();
        let job = Command::Run(Box::new(move |store| {
            let result = catch_unwind(AssertUnwindSafe(|| operation(store)))
                .map_err(|_| DbExecutorError::Panicked)
                .and_then(|result| result.map_err(DbExecutorError::Store));
            let _ = result_sender.send(result);
        }));
        match self.inner.sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(DbExecutorError::QueueFull),
            Err(TrySendError::Disconnected(_)) => return Err(DbExecutorError::Shutdown),
        }
        result_receiver
            .await
            .unwrap_or(Err(DbExecutorError::Panicked))
    }

    /// Stop admission, drain commands already accepted, and join within a
    /// fixed bound. Calling shutdown more than once is harmless.
    pub fn shutdown(&self) -> Result<(), DbExecutorError> {
        self.shutdown_with_timeout(DEFAULT_SHUTDOWN_TIMEOUT)
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> Result<(), DbExecutorError> {
        let _shutdown_guard = self
            .inner
            .shutdown_lock
            .lock()
            .expect("database executor shutdown lock poisoned");
        let deadline = Instant::now() + timeout;
        self.inner.accepting.store(false, Ordering::Release);
        if !self.inner.shutdown_sent.load(Ordering::Acquire) {
            let mut command = Command::Shutdown;
            loop {
                match self.inner.sender.try_send(command) {
                    Ok(()) => {
                        self.inner.shutdown_sent.store(true, Ordering::Release);
                        break;
                    }
                    Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                        command = returned;
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(TrySendError::Full(_)) => return Err(DbExecutorError::ShutdownTimedOut),
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
        }

        loop {
            let finished = self
                .inner
                .join
                .lock()
                .expect("database executor join lock poisoned")
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished);
            if finished {
                break;
            }
            if Instant::now() >= deadline {
                return Err(DbExecutorError::ShutdownTimedOut);
            }
            thread::sleep(Duration::from_millis(2));
        }
        if let Some(join) = self
            .inner
            .join
            .lock()
            .expect("database executor join lock poisoned")
            .take()
        {
            join.join().map_err(|_| DbExecutorError::Panicked)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use tempfile::TempDir;

    use super::*;

    fn executor(capacity: usize) -> (TempDir, DbExecutor) {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("executor.sqlite");
        drop(Store::open(&path).unwrap());
        let executor = DbExecutor::start_with_capacity(path, capacity).unwrap();
        (directory, executor)
    }

    #[tokio::test]
    async fn command_panic_is_typed_and_owner_thread_survives() {
        let (_directory, executor) = executor(2);
        let error = executor
            .execute::<(), _>(|_| panic!("test command panic"))
            .await
            .unwrap_err();
        assert!(matches!(error, DbExecutorError::Panicked));
        assert_eq!(
            executor
                .execute(|store| Ok(store.list()?.len()))
                .await
                .unwrap(),
            0
        );
        executor.shutdown().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_queue_and_draining_shutdown_have_typed_results() {
        let (_directory, executor) = executor(1);
        let barrier = Arc::new(Barrier::new(2));
        let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
        let blocked = {
            let executor = executor.clone();
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                executor
                    .execute(move |_| {
                        entered_sender.send(()).unwrap();
                        barrier.wait();
                        Ok(1)
                    })
                    .await
            })
        };
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let queued = {
            let executor = executor.clone();
            tokio::spawn(async move { executor.execute(|_| Ok(2)).await })
        };
        thread::sleep(Duration::from_millis(20));
        assert!(matches!(
            executor.execute(|_| Ok(3)).await,
            Err(DbExecutorError::QueueFull)
        ));
        let shutdown = {
            let executor = executor.clone();
            thread::spawn(move || executor.shutdown())
        };
        thread::sleep(Duration::from_millis(20));
        assert!(matches!(
            executor.execute(|_| Ok(4)).await,
            Err(DbExecutorError::Shutdown)
        ));
        barrier.wait();
        shutdown.join().unwrap().unwrap();
        assert_eq!(blocked.await.unwrap().unwrap(), 1);
        assert_eq!(queued.await.unwrap().unwrap(), 2);
        assert!(matches!(
            executor.execute(|_| Ok(5)).await,
            Err(DbExecutorError::Shutdown)
        ));
    }
}
