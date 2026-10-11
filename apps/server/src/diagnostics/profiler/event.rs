use serde_json::Value;

/// One profiler record. The store wraps it in the versioned envelope.
#[derive(Clone, Debug)]
pub struct ProfilerEvent {
    pub kind: &'static str,
    pub source: String,
    pub payload: Value,
}
