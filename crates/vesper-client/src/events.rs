use serde_json::Value;

/// Receive serialized client events on the calling or worker thread.
pub trait EventSink: Send + Sync {
    /// Forward the event name and JSON payload without reentering task operations.
    fn emit(&self, event: &str, payload: Value);
}

/// Discard events for headless callers.
#[derive(Default)]
pub struct NoopEventSink;

impl EventSink for NoopEventSink {
    fn emit(&self, _event: &str, _payload: Value) {}
}
