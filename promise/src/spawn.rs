use anyhow::{anyhow, Result};
use async_executor::Executor;
use flume::{bounded, unbounded, Receiver, TryRecvError};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

pub use async_task::{Runnable, Task};
pub type SpawnFunc = Box<dyn FnOnce() + Send>;
pub type ScheduleFunc = Box<dyn Fn(Runnable) + Send + Sync + 'static>;

fn no_scheduler_configured(_: Runnable) {
    panic!("no scheduler has been configured");
}

lazy_static::lazy_static! {
    static ref ON_MAIN_THREAD: Mutex<ScheduleFunc> = Mutex::new(Box::new(no_scheduler_configured));
    static ref ON_MAIN_THREAD_LOW_PRI: Mutex<ScheduleFunc> = Mutex::new(Box::new(no_scheduler_configured));
    static ref SCOPED_EXECUTOR: Mutex<Option<Arc<Executor<'static>>>> = Mutex::new(None);
}

static SCHEDULER_CONFIGURED: AtomicBool = AtomicBool::new(false);

fn schedule_runnable(runnable: Runnable, high_pri: bool) {
    let func = if high_pri {
        ON_MAIN_THREAD.lock()
    } else {
        ON_MAIN_THREAD_LOW_PRI.lock()
    }
    .unwrap();
    func(runnable);
}

pub fn is_scheduler_configured() -> bool {
    SCHEDULER_CONFIGURED.load(Ordering::Relaxed)
}

/// Set callbacks for scheduling normal and low priority futures.
/// Why this and not "just tokio"?  In a GUI application there is typically
/// a special GUI processing loop that may need to run on the "main thread",
/// so we can't just run a tokio/mio loop in that context.
/// This particular crate has no real knowledge of how that plumbing works,
/// it just provides the abstraction for scheduling the work.
/// This function allows the embedding application to set that up.
pub fn set_schedulers(main: ScheduleFunc, low_pri: ScheduleFunc) {
    *ON_MAIN_THREAD.lock().unwrap() = Box::new(main);
    *ON_MAIN_THREAD_LOW_PRI.lock().unwrap() = Box::new(low_pri);
    SCHEDULER_CONFIGURED.store(true, Ordering::Relaxed);
}

/// Spawn a new thread to execute the provided function.
/// Returns a JoinHandle that implements the Future trait
/// and that can be used to await and yield the return value
/// from the thread.
/// Can be called from any thread.
pub fn spawn_into_new_thread<F, T>(f: F) -> Task<Result<T>>
where
    F: FnOnce() -> Result<T>,
    F: Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = bounded(1);

    // Holds the waker that may later observe
    // during the Future::poll call.
    struct WakerHolder {
        waker: Mutex<Option<Waker>>,
    }

    let holder = Arc::new(WakerHolder {
        waker: Mutex::new(None),
    });

    let thread_waker = Arc::clone(&holder);
    std::thread::spawn(move || {
        // Wake the poller from a drop guard rather than inline at the end of
        // this closure, so that it happens on every exit from this thread --
        // including `f()` panicking and unwinding straight past here. `poll`
        // below already knows how to report a thread that produced no result
        // (it turns the disconnected channel into an error), but it only gets
        // the chance if something wakes it up to look; without this the future
        // would stay parked for the rest of the process's life.
        struct WakeOnExit(Arc<WakerHolder>);

        impl Drop for WakeOnExit {
            fn drop(&mut self) {
                // Tolerate a poisoned lock rather than unwrapping: this may
                // run while already unwinding, where panicking a second time
                // aborts the process. `wake()` below is left unguarded --
                // `Waker`'s contract says it doesn't panic, and the only way
                // it could here is a poisoned scheduler mutex, which has
                // already broken every other spawn in the process.
                let mut waker = match self.0.waker.lock() {
                    Ok(waker) => waker,
                    Err(err) => err.into_inner(),
                };
                // If someone polled the thread before we got here,
                // they will have populated the waker; extract it
                // and wake up the scheduler so that it will poll
                // the result again.
                if let Some(waker) = waker.take() {
                    waker.wake();
                }
            }
        }

        let _wake_on_exit = WakeOnExit(thread_waker);

        {
            // Rebinding moves the sender into this inner scope, so that it is
            // dropped before the guard above wakes anyone. The order matters:
            // a poll racing in between a wake and the sender's drop would find
            // the channel empty but still connected, re-register its waker,
            // and go back to sleep with nothing left to wake it.
            let tx = tx;
            // Run the thread
            let res = f();
            // Pass the result back, but don't panic if the receiving future
            // was already dropped/cancelled (channel disconnected).
            let _ = tx.send(res);
        }
    });

    struct PendingResult<T> {
        rx: Receiver<Result<T>>,
        holder: Arc<WakerHolder>,
    }

    impl<T> std::future::Future for PendingResult<T> {
        type Output = Result<T>;

        fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context) -> Poll<Self::Output> {
            // Check if result is already available
            match self.rx.try_recv() {
                Ok(res) => Poll::Ready(res),
                Err(TryRecvError::Disconnected) => {
                    Poll::Ready(Err(anyhow!("thread terminated without providing a result")))
                }
                Err(TryRecvError::Empty) => {
                    // Register the waker, then re-check: closes the race
                    // window where the worker sends between the first
                    // `try_recv` above and the waker being stored, which
                    // would otherwise have nothing left to wake it.
                    let mut waker = self.holder.waker.lock().unwrap();
                    waker.replace(cx.waker().clone());
                    drop(waker);

                    match self.rx.try_recv() {
                        Ok(res) => Poll::Ready(res),
                        Err(TryRecvError::Disconnected) => Poll::Ready(Err(anyhow!(
                            "thread terminated without providing a result"
                        ))),
                        Err(TryRecvError::Empty) => Poll::Pending,
                    }
                }
            }
        }
    }

    spawn_into_main_thread(PendingResult { rx, holder })
}

fn get_scoped() -> Option<Arc<Executor<'static>>> {
    SCOPED_EXECUTOR.lock().unwrap().as_ref().map(Arc::clone)
}

/// Spawn a future into the main thread; it will be polled in the
/// main thread.
/// This function can be called from any thread.
/// If you are on the main thread already, consider using
/// spawn() instead to lift the `Send` requirement.
pub fn spawn_into_main_thread<F, R>(future: F) -> Task<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    if let Some(executor) = get_scoped() {
        return executor.spawn(future);
    }
    let (runnable, task) = async_task::spawn(future, |runnable| schedule_runnable(runnable, true));
    runnable.schedule();
    task
}

/// Spawn a future into the main thread; it will be polled in
/// the main thread in the low priority queue--all other normal
/// priority items will be drained before considering low priority
/// spawns.
/// If you are on the main thread already, consider using `spawn_with_low_priority`
/// instead to lift the `Send` requirement.
pub fn spawn_into_main_thread_with_low_priority<F, R>(future: F) -> Task<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    if let Some(executor) = get_scoped() {
        return executor.spawn(future);
    }
    let (runnable, task) = async_task::spawn(future, |runnable| schedule_runnable(runnable, false));
    runnable.schedule();
    task
}

/// Spawn a future with normal priority.
pub fn spawn<F, R>(future: F) -> Task<R>
where
    F: Future<Output = R> + 'static,
    R: 'static,
{
    let (runnable, task) =
        async_task::spawn_local(future, |runnable| schedule_runnable(runnable, true));
    runnable.schedule();
    task
}

/// Spawn a future with low priority; it will be polled only after
/// all other normal priority items are processed.
pub fn spawn_with_low_priority<F, R>(future: F) -> Task<R>
where
    F: Future<Output = R> + 'static,
    R: 'static,
{
    let (runnable, task) =
        async_task::spawn_local(future, |runnable| schedule_runnable(runnable, false));
    runnable.schedule();
    task
}

/// Block the current thread until the passed future completes.
pub use async_io::block_on;

pub struct SimpleExecutor {
    rx: Receiver<SpawnFunc>,
}

impl SimpleExecutor {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();

        let tx_main = tx.clone();
        let tx_low = tx.clone();
        let queue_func = move |f: SpawnFunc| {
            tx_main.send(f).ok();
        };
        let queue_func_low = move |f: SpawnFunc| {
            tx_low.send(f).ok();
        };
        set_schedulers(
            Box::new(move |task| {
                queue_func(Box::new(move || {
                    task.run();
                }))
            }),
            Box::new(move |task| {
                queue_func_low(Box::new(move || {
                    task.run();
                }))
            }),
        );
        Self { rx }
    }

    pub fn tick(&self) -> anyhow::Result<()> {
        match self.rx.recv() {
            Ok(func) => func(),
            Err(err) => anyhow::bail!("while waiting for events: {:?}", err),
        };
        Ok(())
    }
}

pub struct ScopedExecutor {}

impl ScopedExecutor {
    pub fn new() -> Self {
        SCOPED_EXECUTOR
            .lock()
            .unwrap()
            .replace(Arc::new(Executor::new()));

        Self {}
    }

    pub async fn run<T>(&self, future: impl Future<Output = T>) -> T {
        get_scoped()
            .expect("SCOPED_EXECUTOR to be alive as long as ScopedExecutor")
            .run(future)
            .await
    }
}

impl Drop for ScopedExecutor {
    fn drop(&mut self) {
        SCOPED_EXECUTOR.lock().unwrap().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ScopedExecutor` publishes itself into the process-global
    /// `SCOPED_EXECUTOR` for the duration of its lifetime, so two of these
    /// tests running concurrently (the default for `cargo test`) would
    /// stomp on each other's executor. Serializes this module's tests
    /// against each other while leaving the rest of the crate free to run
    /// in parallel as normal.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Sanity check on the ordinary path, so a future regression that
    /// breaks the channel or the wake outright (eg. removing `tx.send`, or
    /// the wake it triggers) fails here instead of only in a harder-to-read
    /// hang somewhere else in the tree.
    #[test]
    fn spawn_into_new_thread_returns_the_closures_result() {
        let _guard = TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let scoped = ScopedExecutor::new();
        let result = block_on(scoped.run(async { spawn_into_new_thread(|| Ok(42)).await }));
        assert_eq!(result.unwrap(), 42);
    }

    /// Regression test for the hang `WakeOnExit` fixes: before it existed, a
    /// panic in `f` unwound past both the send and the wake, so nothing
    /// ever polled this future again and it stayed `Pending` forever. That
    /// failure mode is a hang, not a wrong answer, so this test is only a
    /// regression test if it can actually distinguish the two outcomes --
    /// which it does by construction: on the old code this test would never
    /// return, and would show up as a hung test process rather than a
    /// clean failure. On the fixed code it returns an error deterministically,
    /// regardless of how the panicking thread happens to be scheduled.
    #[test]
    fn spawn_into_new_thread_panic_reports_an_error_instead_of_hanging() {
        let _guard = TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let scoped = ScopedExecutor::new();
        let result: anyhow::Result<()> = block_on(scoped.run(async {
            spawn_into_new_thread(|| -> anyhow::Result<()> {
                panic!("intentional test panic, exercising the WakeOnExit unwind path")
            })
            .await
        }));
        assert!(
            result.is_err(),
            "a thread that panics before producing a result must report an error \
             (\"thread terminated without providing a result\"), not hang forever"
        );
    }
}
