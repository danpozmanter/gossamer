//! Runtime support for `std::context` — request-scoped cancellation
//! and deadlines, modeled after Go's `context.Context`.
//! A `Context` is a cheap-to-clone handle carrying a shared
//! cancellation flag plus an optional deadline. Parent/child
//! contexts share storage so cancelling a parent cancels every
//! descendant; children can also add their own deadline narrower
//! than the parent's.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::errors::Error;

#[derive(Debug)]
struct Inner {
    cancelled: AtomicBool,
    deadline: Mutex<Option<Instant>>,
    reason: Mutex<Option<String>>,
    parent: Option<Context>,
}

/// Shared, reference-counted context handle.
#[derive(Debug, Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl Context {
    /// Background context — never cancelled, no deadline. Use as the
    /// root of every request pipeline.
    #[must_use]
    pub fn background() -> Self {
        Self {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                deadline: Mutex::new(None),
                reason: Mutex::new(None),
                parent: None,
            }),
        }
    }

    /// Placeholder context — semantically identical to
    /// [`background`] today, but marks call sites that should
    /// eventually thread a real context through.
    #[must_use]
    pub fn todo() -> Self {
        Self::background()
    }

    /// Returns `true` when this context or any ancestor has been
    /// cancelled, or when the deadline has passed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        if self.inner.cancelled.load(Ordering::Acquire) {
            return true;
        }
        if let Some(deadline) = *self.inner.deadline.lock().unwrap() {
            if Instant::now() >= deadline {
                return true;
            }
        }
        self.inner
            .parent
            .as_ref()
            .is_some_and(Context::is_cancelled)
    }

    /// Returns the cancellation reason if any.
    #[must_use]
    pub fn err(&self) -> Option<Error> {
        if !self.is_cancelled() {
            return None;
        }
        if let Some(reason) = self.inner.reason.lock().unwrap().clone() {
            return Some(Error::new(reason));
        }
        if let Some(parent) = &self.inner.parent {
            return parent.err();
        }
        Some(Error::new("context cancelled"))
    }

    /// Deadline of this context, honouring parent deadlines.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let local = *self.inner.deadline.lock().unwrap();
        match (
            local,
            self.inner.parent.as_ref().and_then(Context::deadline),
        ) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }
}

/// `with_cancel(parent)` returns `(child, cancel)` — invoking
/// `cancel` cancels the child and every descendant.
#[must_use]
pub fn with_cancel(parent: &Context) -> (Context, Cancel) {
    let child = Context {
        inner: Arc::new(Inner {
            cancelled: AtomicBool::new(false),
            deadline: Mutex::new(None),
            reason: Mutex::new(None),
            parent: Some(parent.clone()),
        }),
    };
    let cancel = Cancel {
        inner: Arc::clone(&child.inner),
    };
    (child, cancel)
}

/// `with_deadline(parent, deadline)` returns a child context whose
/// `is_cancelled` flips `true` when `deadline` elapses.
#[must_use]
pub fn with_deadline(parent: &Context, deadline: Instant) -> Context {
    Context {
        inner: Arc::new(Inner {
            cancelled: AtomicBool::new(false),
            deadline: Mutex::new(Some(deadline)),
            reason: Mutex::new(None),
            parent: Some(parent.clone()),
        }),
    }
}

/// `with_timeout(parent, dur)` returns a child context whose
/// deadline is `now + dur`.
#[must_use]
pub fn with_timeout(parent: &Context, duration: Duration) -> Context {
    with_deadline(parent, Instant::now() + duration)
}

/// Cancel handle returned by [`with_cancel`]. Dropping the handle
/// does **not** cancel the context; call [`cancel`][Cancel::cancel]
/// explicitly (mirrors Go's idiom).
pub struct Cancel {
    inner: Arc<Inner>,
}

impl Cancel {
    /// Cancels the associated context with the supplied reason.
    pub fn cancel_with(&self, reason: impl Into<String>) {
        self.inner.cancelled.store(true, Ordering::Release);
        *self.inner.reason.lock().unwrap() = Some(reason.into());
    }

    /// Cancels the associated context with a generic reason.
    pub fn cancel(&self) {
        self.cancel_with("context cancelled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_is_never_cancelled() {
        let ctx = Context::background();
        assert!(!ctx.is_cancelled());
        assert!(ctx.err().is_none());
    }

    #[test]
    fn cancel_flags_child_and_descendants() {
        let root = Context::background();
        let (child, cancel) = with_cancel(&root);
        let (grandchild, _) = with_cancel(&child);
        cancel.cancel_with("done");
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        let err = grandchild.err().unwrap();
        assert!(err.message().contains("done"));
    }

    #[test]
    fn deadline_expires_context() {
        let root = Context::background();
        let ctx = with_timeout(&root, Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(20));
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn child_deadline_is_earliest_of_chain() {
        let root = Context::background();
        let parent_deadline = Instant::now() + Duration::from_secs(60);
        let parent = with_deadline(&root, parent_deadline);
        let child = with_timeout(&parent, Duration::from_millis(5));
        let deadline = child.deadline().unwrap();
        assert!(deadline <= parent_deadline);
    }
}
