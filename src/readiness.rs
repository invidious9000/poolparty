//! Local service readiness, independent of provider capacity and credentials.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::ports::Ledger;

pub struct Readiness {
    serving: AtomicBool,
    ledger: Arc<dyn Ledger>,
}

impl Readiness {
    /// The service lifecycle activates readiness after initialization completes.
    pub fn new(ledger: Arc<dyn Ledger>) -> Self {
        Self {
            serving: AtomicBool::new(false),
            ledger,
        }
    }

    pub async fn ready(&self) -> bool {
        if !self.serving.load(Ordering::Acquire) {
            return false;
        }
        let usable = matches!(
            tokio::time::timeout(Duration::from_millis(250), self.ledger.check_ready()).await,
            Ok(Ok(()))
        );
        usable && self.serving.load(Ordering::Acquire)
    }

    pub(crate) fn serving(self: &Arc<Self>) -> ServingGuard {
        self.serving.store(true, Ordering::Release);
        ServingGuard(self.clone())
    }

    pub(crate) fn stop(&self) {
        self.serving.store(false, Ordering::Release);
    }
}

pub(crate) struct ServingGuard(Arc<Readiness>);
impl Drop for ServingGuard {
    fn drop(&mut self) {
        self.0.stop();
    }
}
