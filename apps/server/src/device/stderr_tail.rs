//! The bounded end of a helper process's stderr, kept to explain why it failed.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{io::AsyncReadExt, process::ChildStderr, task::JoinHandle};

const STDERR_TAIL_BYTES: usize = 4 * 1024;
/// How long an exited helper's stderr may take to reach end of file before it is read.
pub(crate) const STDERR_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// The last [`STDERR_TAIL_BYTES`] a helper wrote to stderr.
#[derive(Clone, Default)]
pub(crate) struct StderrTail(Arc<Mutex<VecDeque<u8>>>);

impl StderrTail {
    /// Reads `pipe` to its end, which also keeps a chatty helper from blocking on a full
    /// pipe for as long as it runs.
    pub(crate) fn drain(&self, mut pipe: ChildStderr) -> JoinHandle<()> {
        let tail = self.0.clone();
        tokio::spawn(async move {
            let mut buffer = [0u8; 1024];
            while let Ok(read) = pipe.read(&mut buffer).await {
                if read == 0 {
                    return;
                }
                let mut tail = tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tail.extend(&buffer[..read]);
                let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                tail.drain(..excess);
            }
        })
    }

    pub(crate) fn text(&self) -> Option<String> {
        let tail = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes: Vec<u8> = tail.iter().copied().collect();
        let text = String::from_utf8_lossy(&bytes).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
}
