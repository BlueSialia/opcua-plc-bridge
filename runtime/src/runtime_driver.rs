//! Runtime wrapper around `ProtocolDriver`: polling, exponential backoff, health
//! emission, staleness checks, and shutdown coordination.

use std::sync::Arc;
use std::time::Duration;

use chrono::Duration as ChronoDuration;
use core_model::{TagQuality, TagRegistry};
use driver_common::ProtocolDriver;
use tokio::sync::{mpsc, watch, Mutex};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, error, info, warn};

use rand::RngExt;
use serde_json::Value as JsonValue;

/// Runtime wrapper around a protocol driver, adding polling, backoff, health
/// emission, staleness checks, and coordinated shutdown.
pub struct RuntimeDriver {
    /// Logical name for the driver/PLC instance (for logs and health).
    pub name: String,

    /// Shared protocol driver implementation.
    driver: Arc<dyn ProtocolDriver>,

    /// Poll interval for scheduling `read_cycle`.
    poll_interval: Duration,

    /// Optional health event sender.
    health_tx: Option<mpsc::Sender<JsonValue>>,

    /// Internal shutdown channel used to notify the background poller.
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,

    /// Background task handle for the polling loop (spawned on `start`).
    poll_handle: Mutex<Option<JoinHandle<()>>>,

    /// Maximum backoff (upper bound in seconds).
    max_backoff_secs: u64,

    /// Shared tag registry for staleness checks and quality updates.
    registry: Arc<TagRegistry>,
}

impl RuntimeDriver {
    /// Create a new `RuntimeDriver`.
    pub fn new(
        driver: Arc<dyn ProtocolDriver>,
        name: impl Into<String>,
        poll_interval: Duration,
        registry: Arc<TagRegistry>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            name: name.into(),
            driver,
            poll_interval,
            health_tx: None,
            shutdown_tx,
            shutdown_rx,
            poll_handle: Mutex::new(None),
            max_backoff_secs: 30,
            registry,
        }
    }

    /// Set an optional health sender.
    pub fn set_health_sender(&mut self, tx: mpsc::Sender<JsonValue>) {
        self.health_tx = Some(tx);
    }

    /// Start background poller (idempotent).
    pub async fn start(&self) {
        let mut guard = self.poll_handle.lock().await;
        if guard.is_some() {
            debug!(driver = %self.name, "poller already running");
            return;
        }

        let driver = self.driver.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();
        let poll_interval = self.poll_interval;
        let name = self.name.clone();
        let health_tx = self.health_tx.clone();
        let max_backoff = self.max_backoff_secs;
        let registry = self.registry.clone();

        // Compute an initial jitter here (outside the spawned task) so we don't
        // create a non-Send RNG inside the async task. This avoids the
        // `future cannot be sent between threads safely` error.
        let initial_jitter_ms: u64 = if poll_interval.as_millis() > 0 {
            let mut rng = rand::rng();
            let max_jitter = poll_interval.as_millis() as u64;
            rng.random_range(0..max_jitter)
        } else {
            0
        };

        let handle = tokio::spawn(async move {
            // Start with zero backoff
            let mut backoff_secs = 0u64;

            // Use an interval ticker with skip behavior to schedule periodic polls.
            let mut ticker = time::interval(poll_interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

            // Apply the previously-computed initial jitter (if any) before the first poll tick.
            if initial_jitter_ms > 0 {
                debug!(driver=%name, jitter_ms = initial_jitter_ms, "initial jitter before first poll");
                time::sleep(Duration::from_millis(initial_jitter_ms)).await;
            }

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        // If we previously had a backoff set, respect it before running the next cycle.
                        // Make the backoff interruptible so shutdown can break out of long sleeps.
                        if backoff_secs > 0 {
                            debug!(driver=%name, backoff = backoff_secs, "backoff active, sleeping before next poll");
                            tokio::select! {
                                _ = time::sleep(Duration::from_secs(backoff_secs)) => { /* backoff elapsed, continue */ }
                                _ = shutdown_rx.changed() => {
                                    debug!(driver=%name, "shutdown requested during backoff, exiting poll loop");
                                    break;
                                }
                            }
                        }

                        // Run a single read cycle
                        match driver.read_cycle().await {
                            Ok(()) => {
                                // Reset backoff on success
                                backoff_secs = 0;
                                debug!(driver=%name, "read_cycle succeeded");

                                // Check for stale tags belonging to this PLC
                                Self::check_stale_tags(&name, &registry);

                                // Query optional health and emit if present (best-effort, non-blocking)
                                if let Some(tx) = &health_tx {
                                    match driver.health().await {
                                        Ok(Some(h)) => {
                                            let mut map = serde_json::Map::new();
                                            map.insert("plc".to_string(), JsonValue::String(name.clone()));
                                            map.insert("status".to_string(), JsonValue::String("ok".to_string()));
                                            // Normalize detail to an object with { message, data }
                                            let mut detail = serde_json::Map::new();
                                            detail.insert("message".to_string(), JsonValue::String("ok".to_string()));
                                            detail.insert("data".to_string(), h);
                                            map.insert("detail".to_string(), JsonValue::Object(detail));
                                            let _ = tx.try_send(JsonValue::Object(map));
                                        }
                                        Ok(None) => { /* nothing to emit */ }
                                        Err(e) => {
                                            warn!(driver=%name, error = ?e, "health() failed after successful cycle");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!(driver=%name, error = ?e, "read_cycle failed");

                                // Emit health event with failure (if requested); normalize detail to object
                                if let Some(tx) = &health_tx {
                                    let mut map = serde_json::Map::new();
                                    map.insert("plc".to_string(), JsonValue::String(name.clone()));
                                    map.insert("status".to_string(), JsonValue::String("error".to_string()));
                                    let mut detail = serde_json::Map::new();
                                    detail.insert("message".to_string(), JsonValue::String(format!("{}", e)));
                                    detail.insert("data".to_string(), JsonValue::Null);
                                    map.insert("detail".to_string(), JsonValue::Object(detail));
                                    let _ = tx.try_send(JsonValue::Object(map));
                                }

                                // Calculate next backoff: if currently zero, start at 1s, else double up to max.
                                if backoff_secs == 0 {
                                    backoff_secs = 1;
                                } else {
                                    backoff_secs = std::cmp::min(backoff_secs.saturating_mul(2), max_backoff);
                                }

                                // Continue; next tick will schedule the following cycle
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        debug!(driver=%name, "shutdown requested, exiting poll loop");
                        break;
                    }
                }
            }

            info!(driver=%name, "poller task exiting");
        });

        *guard = Some(handle);
        info!(driver = %self.name, "started runtime driver poller");
    }

    /// Stop background poller and wait for it to finish.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);

        // Take the handle and await it
        let mut h = self.poll_handle.lock().await;
        if let Some(handle) = h.take() {
            // Await the join; ignore panics but log them.
            match handle.await {
                Ok(_) => debug!(driver=%self.name, "poller stopped gracefully"),
                Err(join_err) => {
                    warn!(driver=%self.name, error = ?join_err, "poller task join error")
                }
            }
        } else {
            debug!(driver=%self.name, "poller was not running");
        }
    }

    /// Check all tags belonging to `plc_name` and mark any past their
    /// configured `stale_after_ms` as `TagQuality::Stale`.
    fn check_stale_tags(plc_name: &str, registry: &TagRegistry) {
        use tracing::trace;
        for def in registry.all_definitions_sorted() {
            if def.plc_name.as_ref() != plc_name {
                continue;
            }
            let timeout_ms = match def.stale_after_ms() {
                Some(t) => t,
                None => continue,
            };
            let tag = match registry.get_tag(def.id_str()) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let timeout = ChronoDuration::milliseconds(timeout_ms as i64);
            if tag.is_stale(timeout) {
                // Only log and update if the tag isn't already marked Stale
                if tag.quality != TagQuality::Stale {
                    trace!(tag = %def.id_str(), plc = %plc_name, "marking tag stale");
                    let _ = registry.set_tag_quality(def.id_str(), TagQuality::Stale);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use core_model::TagValue;
    use driver_common::DynDriverError;
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::mpsc;

    struct DummyProtoDriver;

    #[async_trait]
    impl ProtocolDriver for DummyProtoDriver {
        async fn read_cycle(&self) -> Result<(), DynDriverError> {
            Ok(())
        }

        async fn submit_write(
            &self,
            _tag_id: &str,
            _value: TagValue,
        ) -> Result<(), DynDriverError> {
            Ok(())
        }

        async fn health(&self) -> Result<Option<JsonValue>, DynDriverError> {
            Ok(Some(json!({"uptime": 42})))
        }
    }

    /// #feature DRV-MODBUS
    #[tokio::test]
    async fn runtime_driver_start_stop() {
        let proto = Arc::new(DummyProtoDriver) as Arc<dyn ProtocolDriver>;
        let defs = vec![core_model::TagDefinition::new(
            "t1",
            "t1",
            "D100",
            core_model::TagDataType::UInt16,
            "test-plc",
        )];
        let registry =
            Arc::new(core_model::TagRegistry::from_definitions(&defs).expect("build registry"));
        let mut rd = RuntimeDriver::new(proto, "test-plc", Duration::from_millis(10), registry);
        let (tx, mut rx) = mpsc::channel::<JsonValue>(8);
        rd.set_health_sender(tx);
        rd.start().await;
        // Let the poller run a couple iterations
        tokio::time::sleep(Duration::from_millis(50)).await;
        rd.stop().await;

        // health channel may have messages; just ensure it is operable
        while let Ok(Some(_)) = rx.try_recv().map(Some) {
            // drain
        }
    }
}
