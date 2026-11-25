//! A bridge-authenticated, single-use bind intent. Cancellation never waits
//! for the Desktop lifecycle lock or a server request.

use std::{future::Future, sync::Mutex};
use tokio::sync::watch;

use super::core::{BridgeError, err};

#[derive(Default)]
pub(super) struct Intents(Mutex<Option<Intent>>);

struct Intent {
    id: String,
    stop: watch::Sender<bool>,
    claimed: bool,
}

#[derive(Clone, Default)]
pub(super) struct Lease(Option<watch::Receiver<bool>>);

impl Lease {
    pub(super) fn is_legacy(&self) -> bool {
        self.0.is_none()
    }

    pub(super) fn live(&self) -> bool {
        self.0.as_ref().is_none_or(|rx| !*rx.borrow())
    }

    pub(super) fn check(&self) -> Result<(), BridgeError> {
        self.live().then_some(()).ok_or_else(cancelled)
    }

    /// Linearize a local state write with cancellation. Take this only
    /// after acquiring any file locks, and never hold it across an await.
    pub(super) fn publication(&self) -> Result<Option<watch::Ref<'_, bool>>, BridgeError> {
        let guard = self.0.as_ref().map(watch::Receiver::borrow);
        if guard.as_deref() == Some(&true) {
            return Err(cancelled());
        }
        Ok(guard)
    }

    pub(super) async fn ended(&self) {
        match self.0.clone() {
            Some(mut rx) => {
                let _ = rx.wait_for(|stop| *stop).await;
            }
            None => std::future::pending().await,
        }
    }

    pub(super) async fn race<T>(&self, work: impl Future<Output = T>) -> Result<T, BridgeError> {
        tokio::select! {
            biased;
            _ = self.ended() => Err(cancelled()),
            result = work => { self.check()?; Ok(result) },
        }
    }
}

fn cancelled() -> BridgeError {
    err(409, "bind_cancelled", "The device sign-in was cancelled")
}

impl Intents {
    pub(super) fn reserve(&self, id: String) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = slot.replace(Intent {
            id,
            stop: watch::channel(false).0,
            claimed: false,
        }) {
            old.stop.send_replace(true);
        }
    }

    pub(super) fn claim(&self, id: &str) -> Result<Lease, BridgeError> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let intent = slot
            .as_mut()
            .filter(|i| i.id == id && !i.claimed)
            .ok_or_else(cancelled)?;
        intent.claimed = true;
        Ok(Lease(Some(intent.stop.subscribe())))
    }

    pub(super) fn cancel(&self, id: &str) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.as_ref().is_some_and(|intent| intent.id == id)
            && let Some(intent) = slot.take()
        {
            intent.stop.send_replace(true);
        }
    }

    pub(super) fn cancel_all(&self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(intent) = slot.take() {
            intent.stop.send_replace(true);
        }
    }
}
