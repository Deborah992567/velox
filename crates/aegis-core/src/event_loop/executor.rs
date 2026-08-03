//! A minimal single-threaded futures executor.
//!
//! Tasks are polled only when they are ready: either immediately after
//! [`Executor::spawn`] or after their waker fires (I/O readiness, timers, or
//! a self-wake). This is the execution half of the reactor/executor pair
//! described in `docs/architecture.md` §3.1 and ADR 0002 — deliberately no
//! work stealing, no preemption, one queue per worker.
//!
//! Everything here is `Rc`/`RefCell` based: futures need not be `Send` and a
//! task never leaves its worker thread. The waker is built from a raw vtable
//! (the [`Wake`] trait would force `Send + Sync` on every task).
//!
//! # Waker reentrancy
//!
//! A task may call `cx.waker().wake_by_ref()` from inside its own `poll`
//! (including through the reactor's readiness dispatch). The scheduled/done
//! flags therefore live in a `Rc<Cell<u8>>` separate from the `Task` itself,
//! and the executor never holds the queue lock or the task borrow across a
//! poll, so waking from within `poll` can never deadlock or double-borrow.
//!
//! # Safety policy
//!
//! The raw waker vtable is the one piece of `unsafe` here, scoped to this
//! file like the syscall zones in [`crate::net`] and [`crate::platform`].
//! Every unsafe block carries a `// SAFETY:` comment explaining the
//! ownership transfer it performs.
#![allow(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, RawWaker, RawWakerVTable, Waker};

/// A heap-allocated future running to completion on the worker thread.
type BoxFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

const SCHEDULED: u8 = 1 << 0;
const DONE: u8 = 1 << 1;

/// One task in the executor's queue.
struct Task {
    future: BoxFuture,
    state: Rc<Cell<u8>>,
}

/// The waker backing for one task: re-queue it when woken, unless finished.
struct TaskWaker {
    queue: Rc<RefCell<VecDeque<Rc<RefCell<Task>>>>>,
    task: Rc<RefCell<Task>>,
    state: Rc<Cell<u8>>,
}

impl TaskWaker {
    fn requeue(&self) {
        let state = self.state.get();
        if state & DONE == 0 && state & SCHEDULED == 0 {
            self.state.set(state | SCHEDULED);
            self.queue.borrow_mut().push_back(Rc::clone(&self.task));
        }
    }

    /// Leak `self` into a `RawWaker`, transferring ownership of one reference
    /// to the returned value.
    fn into_raw_waker(self: Rc<Self>) -> RawWaker {
        // SAFETY: `Rc::into_raw` leaks a clone of the `Rc` and returns a
        // pointer that stays valid until `VTABLE`'s `drop` releases it.
        RawWaker::new(Rc::into_raw(self).cast(), &VTABLE)
    }

    /// # Safety
    ///
    /// `ptr` must be a pointer produced by [`TaskWaker::into_raw_waker`] whose
    /// reference is still owned by a live `RawWaker`.
    unsafe fn from_raw(ptr: *const ()) -> Rc<Self> {
        // SAFETY: the caller upholds the reference-ownership contract; the
        // pointer aliases a live `Rc<TaskWaker>`.
        unsafe { Rc::from_raw(ptr.cast::<Self>()) }
    }
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_raw, wake_raw, wake_by_ref_raw, drop_raw);

/// # Safety
///
/// `ptr` must be a live `Rc<TaskWaker>` pointer from
/// [`TaskWaker::into_raw_waker`]. The returned `RawWaker` owns a new
/// reference.
unsafe fn clone_raw(ptr: *const ()) -> RawWaker {
    let task_waker = unsafe { TaskWaker::from_raw(ptr) };
    let cloned = Rc::clone(&task_waker);
    // The original reference is re-leaked so the new RawWaker and the old one
    // each own one reference.
    let _ = Rc::into_raw(task_waker);
    cloned.into_raw_waker()
}

/// # Safety
///
/// `ptr` must be a live `Rc<TaskWaker>` pointer from
/// [`TaskWaker::into_raw_waker`]. Consumes the reference.
unsafe fn wake_raw(ptr: *const ()) {
    let task_waker = unsafe { TaskWaker::from_raw(ptr) };
    task_waker.requeue();
    // `task_waker` is dropped here, releasing the reference the RawWaker owned.
}

/// # Safety
///
/// `ptr` must be a live `Rc<TaskWaker>` pointer from
/// [`TaskWaker::into_raw_waker`]. Does not consume the reference.
unsafe fn wake_by_ref_raw(ptr: *const ()) {
    // SAFETY: dereferencing the pointer aliases a live `TaskWaker`.
    let task_waker = unsafe { &*ptr.cast::<TaskWaker>() };
    task_waker.requeue();
}

/// # Safety
///
/// `ptr` must be a live `Rc<TaskWaker>` pointer from
/// [`TaskWaker::into_raw_waker`]. Consumes the reference.
unsafe fn drop_raw(ptr: *const ()) {
    drop(unsafe { TaskWaker::from_raw(ptr) });
}

/// A single-threaded executor with one ready queue.
#[derive(Default)]
pub struct Executor {
    queue: Rc<RefCell<VecDeque<Rc<RefCell<Task>>>>>,
}

impl fmt::Debug for Executor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Executor")
            .field("queued", &self.queued())
            .finish()
    }
}

impl Executor {
    /// Create an empty executor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule `future` for immediate polling.
    ///
    /// # Panics
    ///
    /// Panics only if the queue cell is already mutably borrowed, which is a
    /// programming error in the executor itself.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let task = Rc::new(RefCell::new(Task {
            future: Box::pin(future),
            state: Rc::new(Cell::new(SCHEDULED)),
        }));
        self.queue.borrow_mut().push_back(task);
    }

    /// Poll every ready task until the queue is empty.
    ///
    /// Tasks may wake themselves during a poll; the wake re-queues them for
    /// another pass. A task that finishes is marked done and never re-queued.
    ///
    /// # Panics
    ///
    /// Panics only if a task is polled while already mutably borrowed, which
    /// is a programming error in the executor itself.
    pub fn run_ready(&self) {
        loop {
            let next = self.queue.borrow_mut().pop_front();
            let Some(task) = next else {
                break;
            };
            let state = Rc::clone(&task.borrow().state);
            if state.get() & DONE != 0 {
                continue;
            }
            state.set(state.get() & !SCHEDULED);
            let raw = Rc::new(TaskWaker {
                queue: Rc::clone(&self.queue),
                task: Rc::clone(&task),
                state: Rc::clone(&state),
            })
            .into_raw_waker();
            // SAFETY: `raw` owns exactly one reference to a valid TaskWaker
            // that outlives the poll; the waker is dropped right after.
            let waker = unsafe { Waker::from_raw(raw) };
            let mut cx = Context::from_waker(&waker);
            if task.borrow_mut().future.as_mut().poll(&mut cx).is_ready() {
                state.set(state.get() | DONE);
            }
        }
    }

    /// Number of tasks currently queued (not running).
    pub fn queued(&self) -> usize {
        self.queue.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::Executor;
    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    /// A future that yields once (self-wake) before completing.
    struct WakeOnce {
        woke: bool,
    }

    impl Future for WakeOnce {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.woke {
                Poll::Ready(())
            } else {
                self.woke = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    /// A future that parks until an external waker fires (reactor-style).
    struct AwaitWake {
        slot: Rc<RefCell<Option<Waker>>>,
        woke: bool,
    }

    impl Future for AwaitWake {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.woke {
                Poll::Ready(())
            } else {
                self.woke = true;
                *self.slot.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }

    #[test]
    fn immediate_task_runs() {
        let executor = Executor::new();
        let flag = Rc::new(Cell::new(false));
        let task_flag = Rc::clone(&flag);
        executor.spawn(async move {
            task_flag.set(true);
        });
        executor.run_ready();
        assert!(flag.get());
        assert_eq!(executor.queued(), 0);
    }

    #[test]
    fn pending_task_does_not_spin() {
        let executor = Executor::new();
        let flag = Rc::new(Cell::new(false));
        let task_flag = Rc::clone(&flag);
        executor.spawn(std::future::pending());
        executor.spawn(async move {
            task_flag.set(true);
        });
        executor.run_ready();
        assert!(flag.get());
    }

    #[test]
    fn self_waking_task_completes() {
        let executor = Executor::new();
        let flag = Rc::new(Cell::new(false));
        let task_flag = Rc::clone(&flag);
        executor.spawn(async move {
            WakeOnce { woke: false }.await;
            task_flag.set(true);
        });
        executor.run_ready();
        assert!(flag.get());
    }

    #[test]
    fn external_wake_requeues_task() {
        let executor = Executor::new();
        let slot = Rc::new(RefCell::new(None));
        let flag = Rc::new(Cell::new(false));
        let task_flag = Rc::clone(&flag);
        let task_slot = Rc::clone(&slot);
        executor.spawn(async move {
            AwaitWake {
                slot: task_slot,
                woke: false,
            }
            .await;
            task_flag.set(true);
        });
        executor.run_ready();
        assert!(!flag.get(), "task must wait for the wake");
        assert!(slot.borrow().is_some(), "waker must be captured");

        slot.borrow_mut().take().expect("waker present").wake();
        executor.run_ready();
        assert!(flag.get());
    }
}
