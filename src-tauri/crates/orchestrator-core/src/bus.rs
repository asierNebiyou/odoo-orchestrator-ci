//! In-process pub/sub. Deliberately thin — `Core::record_event` is what
//! actually persists an event before broadcasting it, so nothing observes an
//! event that isn't already durable in the log.

use tokio::sync::broadcast;

use crate::events::EventEnvelope;

const CHANNEL_CAPACITY: usize = 256;

#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    /// Broadcast to whoever is currently subscribed. A `SendError` here just
    /// means no one is listening right now, which is fine — the event is
    /// already durable in the log by the time this is called.
    pub fn publish(&self, envelope: EventEnvelope) {
        let _ = self.sender.send(envelope);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// One line of real output from a supervised process.
///
/// Deliberately **not** an `Event`: the event log is the durable record of
/// what the app did, and burying it under thousands of Odoo log lines
/// would destroy the thing that makes Activity readable. Log lines are
/// high-volume, disposable, and only interesting while you're looking at
/// them — so they get their own bounded buffer and their own channel.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogLine {
    pub server_id: uuid::Uuid,
    pub at: chrono::DateTime<chrono::Utc>,
    /// "stdout" or "stderr". Odoo writes its own log to stderr, so this is
    /// how the UI can show the log without the noise of anything the
    /// process happens to print.
    pub stream: String,
    pub line: String,
}

/// Live process output, separate from the domain event bus above.
#[derive(Clone)]
pub struct LogBus {
    sender: broadcast::Sender<LogLine>,
}

impl LogBus {
    pub fn new() -> Self {
        // Bigger than the event channel: a starting Odoo emits hundreds of
        // lines in a burst, and a subscriber that lags briefly should miss
        // nothing rather than get a gap in a traceback.
        let (sender, _) = broadcast::channel(2048);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.sender.subscribe()
    }

    pub fn publish(&self, line: LogLine) {
        let _ = self.sender.send(line);
    }
}

impl Default for LogBus {
    fn default() -> Self {
        Self::new()
    }
}
