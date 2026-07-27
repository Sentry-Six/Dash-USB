use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub data: serde_json::Value,
}

#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Vec<u8>>,
    client_count: Arc<AtomicUsize>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Hub {
            tx,
            client_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn broadcast(&self, msg_type: &str, data: impl Serialize) {
        // Skip the serialize work when nobody is subscribed.
        if self.tx.receiver_count() == 0 {
            return;
        }
        let msg = Message {
            msg_type: msg_type.to_string(),
            data: match serde_json::to_value(data) {
                Ok(v) => v,
                Err(e) => {
                    warn!("failed to serialize broadcast message: {}", e);
                    return;
                }
            },
        };
        let bytes = match serde_json::to_vec(&msg) {
            Ok(b) => b,
            Err(e) => {
                warn!("failed to marshal broadcast message: {}", e);
                return;
            }
        };
        let _ = self.tx.send(bytes);
    }

    /// Yields the serialized JSON bytes of each broadcast.
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.client_count.fetch_add(1, Ordering::Relaxed);
        let count = self.client_count.load(Ordering::Relaxed);
        debug!("WebSocket client connected ({} total)", count);
        self.tx.subscribe()
    }

    pub fn client_disconnected(&self) {
        let prev = self.client_count.fetch_sub(1, Ordering::Relaxed);
        debug!("WebSocket client disconnected ({} total)", prev - 1);
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}
