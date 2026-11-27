//! Coalesced notifications for the single paper book writer.
use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::time::Duration;

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum Event {
    Feed = 1,
    Strategy = 2,
    Choice = 4,
    Quotes = 8,
    FeedState = 16,
    Preparation = 32,
}

#[derive(Default)]
pub(crate) struct Events(u8);

impl Events {
    pub(crate) fn has_trading_work(&self) -> bool {
        self.0 != 0
    }

    pub(crate) fn contains(&self, event: Event) -> bool {
        self.0 & event as u8 != 0
    }
}

#[derive(Clone)]
pub(crate) struct Notify {
    pending: Arc<Mutex<Events>>,
    sender: SyncSender<()>,
}

impl Notify {
    pub(crate) fn signal(&self, event: Event) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.0 == 0 {
            // One token represents all pending sources. A closed receiver means
            // the controller has exited.
            let _ = self.sender.try_send(());
        }
        pending.0 |= event as u8;
    }
}

pub(crate) struct Inbox {
    notify: Notify,
    receiver: Receiver<()>,
}

impl Inbox {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = sync_channel(1);
        Self {
            notify: Notify {
                sender,
                pending: Arc::default(),
            },
            receiver,
        }
    }

    pub(crate) fn notify(&self) -> Notify {
        self.notify.clone()
    }

    pub(crate) fn wait(&self, timeout: Duration) -> Events {
        if self.receiver.recv_timeout(timeout).is_err() {
            return Events::default();
        }
        let mut pending = self
            .notify
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *pending)
    }
}
