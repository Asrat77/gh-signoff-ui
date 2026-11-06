//! Settings work that waits on `gh`, each job on its own thread.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use signoff_core::ProductService;

use super::{Effects, Settings};

/// A job for a background thread: it gets the service and returns what to
/// do with the result on the UI thread.
type Work = Box<dyn FnOnce(&ProductService) -> Done + Send>;

/// What finished, applied on the UI thread.
pub(crate) type Done = Box<dyn FnOnce(&mut Settings, &egui::Context, &mut Effects) + Send>;

/// Runs settings work off the UI thread.
pub(crate) struct Jobs {
    service: Arc<ProductService>,
    sender: Sender<Done>,
    inbox: Receiver<Done>,
    wake: Arc<dyn Fn() + Send + Sync>,
    running: usize,
}

impl Jobs {
    pub(crate) fn new(service: Arc<ProductService>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, inbox) = mpsc::channel();
        Self { service, sender, inbox, wake: Arc::new(wake), running: 0 }
    }

    /// Runs `work` on its own thread; what it returns is applied by the next
    /// [`Jobs::finish`].
    pub(crate) fn start(&mut self, work: impl FnOnce(&ProductService) -> Done + Send + 'static) {
        let work: Work = Box::new(work);
        let (service, sender, wake) = (self.service.clone(), self.sender.clone(), self.wake.clone());
        let spawned = thread::Builder::new().name("signoff-settings".into()).spawn(move || {
            let _ = sender.send(work(&service));
            wake();
        });
        match spawned {
            Ok(_) => self.running += 1,
            Err(err) => log::error!("could not start a settings job: {err}"),
        }
    }

    /// Whether a job is still running.
    pub(crate) fn busy(&self) -> bool {
        self.running > 0
    }

    /// Every job that finished since the last call, to apply.
    pub(crate) fn finished(&mut self, ctx: &egui::Context) -> Vec<Done> {
        let done: Vec<Done> = self.inbox.try_iter().collect();
        self.running = self.running.saturating_sub(done.len());
        if self.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        done
    }
}
