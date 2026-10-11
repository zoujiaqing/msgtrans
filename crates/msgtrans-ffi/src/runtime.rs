//! The Tokio runtime msgtrans runs on, and the bridge that lets foreign
//! executors drive msgtrans futures.

use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll};

// Two workers: a client holds one connection, and the runtime only has to
// drive its socket, timers and the transport's internal tasks. More threads
// would mostly cost wakeups and memory on a phone.
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("msgtrans-ffi")
        .enable_all()
        .build()
        .expect("failed to start the msgtrans-ffi Tokio runtime")
});

pin_project_lite::pin_project! {
    /// Polls `fut` with the msgtrans runtime entered.
    ///
    /// UniFFI hands exported futures to the foreign executor (Kotlin
    /// coroutines, Swift tasks), which carries no Tokio context, so msgtrans's
    /// timers, socket registration and `tokio::spawn` would panic there.
    /// Entering the runtime on every poll supplies that context while the
    /// future stays owned by the caller: cancelling the coroutine drops the
    /// future and the in-flight operation with it, rather than leaving a
    /// detached task running whose result nobody will read.
    pub(crate) struct OnRuntime<F> {
        #[pin]
        fut: F,
    }
}

impl<F: Future> Future for OnRuntime<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let _guard = RUNTIME.enter();
        self.project().fut.poll(cx)
    }
}

pub(crate) fn on_runtime<F: Future>(fut: F) -> OnRuntime<F> {
    OnRuntime { fut }
}

/// Run a future to completion on the calling thread, with the runtime entered.
///
/// Only for futures that complete without waiting on I/O or timers (building
/// a client, taking its event stream): nothing here parks the thread for
/// longer than an uncontended lock acquisition. It uses its own executor
/// rather than `Runtime::block_on` so it is also safe to call from inside an
/// unrelated Tokio runtime.
pub(crate) fn run_immediate<F: Future>(fut: F) -> F::Output {
    let _guard = RUNTIME.enter();
    futures::executor::block_on(fut)
}

/// Run a synchronous closure with the runtime entered, for msgtrans calls
/// that spawn internally (e.g. `respond_detached`).
pub(crate) fn enter<R>(f: impl FnOnce() -> R) -> R {
    let _guard = RUNTIME.enter();
    f()
}
