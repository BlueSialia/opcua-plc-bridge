//! Modbus/TCP driver: connects to a Modbus server, performs read cycles, handles writes,
//! updates the TagRegistry and emits health events.

use async_trait::async_trait;
use byteorder::{BigEndian, ReadBytesExt};
use chrono::Utc;
use core_model::{TagDataType, TagQuality, TagRegistry, TagValue, WordOrder};
use std::collections::HashMap;
use std::io::Cursor;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use tokio_modbus::prelude::*;
use tracing::{debug, error, instrument, warn};

use crate::config::ModbusConfig;
use crate::errors::DriverError;
use crate::mapping::{ModbusFunction, ModbusMapping};
use crate::write_request::WriteRequest;

/// Re-export client Context type for internal use.
pub type ClientContext = tokio_modbus::client::Context;

use tokio_modbus::slave::Slave;

use driver_common::{DynDriverError, ProtocolDriver};

/// Modbus driver holding connection state and interacting with the TagRegistry.
pub struct ModbusDriver {
    config: ModbusConfig,
    registry: Arc<TagRegistry>,
    write_tx: mpsc::Sender<WriteRequest>,
    write_rx: Arc<Mutex<mpsc::Receiver<WriteRequest>>>,
    /// Persistent Modbus client context.
    client: Arc<Mutex<Option<ClientContext>>>,
    /// Optional health sender (JSON).
    health_tx: Arc<Mutex<Option<mpsc::Sender<serde_json::Value>>>>,
}

impl ModbusDriver {
    /// Create a new driver instance.
    pub fn new(config: ModbusConfig, registry: Arc<TagRegistry>) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            config,
            registry,
            write_tx: tx,
            write_rx: Arc::new(Mutex::new(rx)),
            client: Arc::new(Mutex::new(None)),
            health_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Validate the current driver configuration (mappings).
    ///
    /// Checks implemented:
    /// - zero-length reads (quantity == 0)
    /// - bit_offset bounds (0..16)
    /// - data_type vs quantity compatibility (e.g. Float/Int32/UInt32 need >=2, Double needs >=4)
    /// - writable flag must not be set for read-only functions (InputRegisters, DiscreteInputs)
    /// - word_order usages that require multi-word quantities (WordOrder::CDAB, WordOrder::DCBA)
    /// - overlapping address ranges per Modbus function
    pub async fn set_health_sender(&self, tx: mpsc::Sender<serde_json::Value>) {
        let mut guard = self.health_tx.lock().await;
        *guard = Some(tx);
    }

    /// Send a small JSON health event if a health sender is configured.
    ///
    /// The emitted JSON normalizes `detail` as an object:
    /// {
    ///   "plc": "<name>",
    ///   "status": "<ok|error>",
    ///   "detail": { "message": "<text>", "data": <value|null> }
    /// }
    /// Use non-blocking `try_send` so health emission does not backpressure driver tasks.
    async fn send_health_event(&self, status: &str, detail: Option<String>) {
        let guard = self.health_tx.lock().await;
        if let Some(tx) = &*guard {
            // Build normalized detail object
            let mut detail_map = serde_json::Map::new();
            match detail {
                Some(d) => {
                    // If a textual detail was provided, place it in `message`.
                    detail_map.insert("message".to_string(), serde_json::Value::String(d));
                    // No structured data available in this call-site, set data to null.
                    detail_map.insert("data".to_string(), serde_json::Value::Null);
                }
                None => {
                    // Use status as message when no extra detail was provided.
                    detail_map.insert(
                        "message".to_string(),
                        serde_json::Value::String(status.to_string()),
                    );
                    detail_map.insert("data".to_string(), serde_json::Value::Null);
                }
            }

            let obj = serde_json::Value::Object(serde_json::Map::from_iter([
                (
                    "plc".to_string(),
                    serde_json::Value::String(self.config.name.clone()),
                ),
                (
                    "status".to_string(),
                    serde_json::Value::String(status.to_string()),
                ),
                ("detail".to_string(), serde_json::Value::Object(detail_map)),
            ]));

            // best-effort, non-blocking send; ignore errors (sender full / closed)
            let _ = tx.try_send(obj);
        }
    }

    /// Obtain a sender to queue write requests.
    pub fn write_sender(&self) -> mpsc::Sender<WriteRequest> {
        self.write_tx.clone()
    }

    /// Internal helper to perform a reconnect with backoff and return a connected client.
    async fn connect_with_backoff(&self) -> Result<ClientContext, DriverError> {
        let mut backoff = 1u64;
        loop {
            match tokio_modbus::client::tcp::connect(self.config.endpoint).await {
                Ok(mut ctx) => {
                    // Set the Modbus unit (slave) as requested by the user configuration.
                    // This is required for multi-slave environments to target the correct device.
                    ctx.set_slave(Slave(self.config.unit_id));
                    debug!(endpoint = %self.config.endpoint, unit = self.config.unit_id, "Connected to Modbus TCP");
                    return Ok(ctx);
                }
                Err(e) => {
                    warn!(error = ?e, endpoint = %self.config.endpoint, "Failed to connect to Modbus TCP, retrying");
                    time::sleep(Duration::from_secs(backoff)).await;
                    backoff =
                        std::cmp::min(self.config.max_backoff_secs, backoff.saturating_mul(2));
                }
            }
        }
    }

    /// Decode registers (Vec<u16>) into raw bytes (big-endian per Modbus spec).
    fn registers_to_bytes(registers: &[u16]) -> Vec<u8> {
        let mut b = Vec::with_capacity(registers.len() * 2);
        for r in registers {
            b.push((r >> 8) as u8);
            b.push((r & 0xFF) as u8);
        }
        b
    }

    /// Apply configured `WordOrder` to raw bytes (words are 2 bytes each).
    ///
    /// Delegates to the `WordOrder` helpers provided by the core-model crate.
    fn apply_byte_order(mut bytes: Vec<u8>, order: &WordOrder) -> Vec<u8> {
        order.apply_to_bytes(&mut bytes);
        bytes
    }

    /// Decode raw bytes into a TagValue using the declared `TagDataType` for the mapping.
    ///
    /// This removes reliance on the runtime-stored value variant and uses the explicit
    /// mapping-declared data type to perform decoding deterministically.
    fn decode_bytes_to_tagvalue(
        data_type: &TagDataType,
        bytes: &[u8],
        _function: &ModbusFunction,
        bit_offset: u8,
    ) -> Result<TagValue, DriverError> {
        // Use big endian reads for words
        let mut cur = Cursor::new(bytes);
        match data_type {
            TagDataType::Bool => {
                // Bool: extract first word and then bit_offset
                let w = cur.read_u16::<BigEndian>().unwrap_or(0);
                let b = ((w >> bit_offset) & 0x1) != 0;
                Ok(TagValue::Bool(b))
            }
            TagDataType::UInt16 => {
                let v = cur.read_u16::<BigEndian>().unwrap_or(0);
                Ok(TagValue::UInt16(v))
            }
            TagDataType::Int16 => {
                let v = cur.read_i16::<BigEndian>().unwrap_or(0);
                Ok(TagValue::Int16(v))
            }
            TagDataType::UInt32 => {
                let hi = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                let lo = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                Ok(TagValue::UInt32((hi << 16) | lo))
            }
            TagDataType::Int32 => {
                let hi = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                let lo = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                Ok(TagValue::Int32(((hi << 16) | lo) as i32))
            }
            TagDataType::Float => {
                let hi = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                let lo = cur.read_u16::<BigEndian>().unwrap_or(0) as u32;
                let bits = (hi << 16) | lo;
                Ok(TagValue::Float(f32::from_bits(bits)))
            }
            TagDataType::Double => {
                let w0 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w1 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w2 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w3 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let bits = (w0 << 48) | (w1 << 32) | (w2 << 16) | w3;
                Ok(TagValue::Double(f64::from_bits(bits)))
            }
            TagDataType::Int64 => {
                // 8 bytes -> i64 big-endian
                let w0 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w1 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w2 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w3 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let bits = (w0 << 48) | (w1 << 32) | (w2 << 16) | w3;
                Ok(TagValue::Int64(bits as i64))
            }
            TagDataType::UInt64 => {
                let w0 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w1 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w2 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let w3 = cur.read_u16::<BigEndian>().unwrap_or(0) as u64;
                let bits = (w0 << 48) | (w1 << 32) | (w2 << 16) | w3;
                Ok(TagValue::UInt64(bits))
            }
            TagDataType::DateTime => {
                // 8-byte milliseconds since UNIX epoch; otherwise fallback to RFC3339 UTF-8.
                if bytes.len() >= 8 {
                    let mut rdr = Cursor::new(&bytes[0..8]);
                    let ms = ReadBytesExt::read_i64::<BigEndian>(&mut rdr).unwrap_or(0);
                    let dt: chrono::DateTime<Utc> = chrono::DateTime::from_timestamp_millis(ms)
                        .unwrap_or(chrono::DateTime::UNIX_EPOCH);
                    Ok(TagValue::DateTime(dt))
                } else {
                    let s = std::str::from_utf8(bytes).map_err(|_| {
                        DriverError::other("Not enough data for DateTime and not valid UTF-8")
                    })?;
                    let parsed = chrono::DateTime::parse_from_rfc3339(s)
                        .map_err(|_| DriverError::other("Invalid DateTime format"))?;
                    Ok(TagValue::DateTime(parsed.with_timezone(&Utc)))
                }
            }
            TagDataType::ByteString => Ok(TagValue::ByteString(bytes.to_vec())),
            TagDataType::String => {
                // Interpret bytes as UTF-8 string trimmed of trailing zeros
                let s = match std::str::from_utf8(bytes) {
                    Ok(st) => st.trim_end_matches('\0').to_string(),
                    Err(_) => bytes
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<String>(),
                };
                Ok(TagValue::String(s))
            }
        }
    }
}

impl ModbusDriver {
    /// Internal implementation of a single read/write cycle, returning the
    /// driver-specific `DriverError`. The runtime-facing `ProtocolDriver` adapter
    /// calls this and converts errors into the boxed `DynDriverError`.
    #[instrument(skip(self), fields(driver = %self.config.name))]
    pub async fn run_read_cycle_impl(&self) -> Result<(), DriverError> {
        // Handle writes first, using the persistent client context directly.
        // This avoids creating a second connection when reads also need the context.
        {
            let mut rx = self.write_rx.lock().await;
            for _ in 0..64 {
                match rx.try_recv() {
                    Ok(req) => {
                        debug!(tag=%req.tag_id, value=?req.value, "Processing queued write");
                        // Find mapping for this tag (clone to own)
                        let mapping = match self
                            .config
                            .mappings
                            .iter()
                            .find(|m| m.tag_id.as_ref() == req.tag_id)
                        {
                            Some(m) => m.clone(),
                            None => {
                                warn!(tag = %req.tag_id, "Write for unknown tag_id");
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Err("Unknown tag mapping".to_string()));
                                }
                                continue;
                            }
                        };
                        if !mapping.writable {
                            warn!(tag = %req.tag_id, "Attempt to write non-writable tag ignored");
                            if let Some(reply) = req.reply {
                                let _ = reply.send(Err("Tag not writable".to_string()));
                            }
                            continue;
                        }

                        // Convert TagValue to registers (u16 words) using standard ordering
                        let words = match crate::driver::tagvalue_to_registers(&req.value) {
                            Ok(w) => w,
                            Err(e) => {
                                warn!(error = %e, "Failed to convert TagValue to registers for write");
                                let _ = self
                                    .registry
                                    .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Err(format!("Conversion error: {}", e)));
                                }
                                continue;
                            }
                        };

                        // Only implement HoldingRegisters write here (others would require different functions)
                        match mapping.function {
                            ModbusFunction::HoldingRegisters => {
                                // Prepare bytes from standard words, apply byte-ordering per mapping, then re-split to words
                                let mut tx_bytes = Vec::with_capacity(words.len() * 2);
                                for w in &words {
                                    tx_bytes.push((w >> 8) as u8);
                                    tx_bytes.push((w & 0xFF) as u8);
                                }
                                let ordered_bytes =
                                    ModbusDriver::apply_byte_order(tx_bytes, &mapping.byte_order);
                                if !ordered_bytes.len().is_multiple_of(2) {
                                    warn!(
                                        "Ordered write bytes length not even for mapping {}",
                                        mapping.tag_id.as_ref()
                                    );
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::CommLost,
                                    );
                                    if let Some(reply) = req.reply {
                                        let _ = reply
                                            .send(Err("Ordered bytes length not even".to_string()));
                                    }
                                    continue;
                                }
                                let mut ordered_words = Vec::with_capacity(ordered_bytes.len() / 2);
                                let mut cur = Cursor::new(&ordered_bytes);
                                while let Ok(w) = cur.read_u16::<BigEndian>() {
                                    ordered_words.push(w);
                                }

                                // take the persistent context out of the mutex (or reconnect)
                                let mut ctx_owned = {
                                    let mut guard = self.client.lock().await;
                                    guard.take()
                                };
                                if ctx_owned.is_none() {
                                    match self.connect_with_backoff().await {
                                        Ok(c) => ctx_owned = Some(c),
                                        Err(e) => {
                                            error!(error = ?e, "Failed to reconnect for write");
                                            let _ = self.registry.set_tag_quality(
                                                mapping.tag_id.as_ref(),
                                                TagQuality::CommLost,
                                            );
                                            if let Some(reply) = req.reply {
                                                let _ = reply
                                                    .send(Err(format!("Reconnect failed: {}", e)));
                                            }
                                            let mut guard_back = self.client.lock().await;
                                            *guard_back = None;
                                            return Err(DriverError::Io(std::io::Error::other(
                                                "Reconnect failed during write",
                                            )));
                                        }
                                    }
                                }
                                let mut ctx = ctx_owned.unwrap();

                                // Perform the write with timeout
                                let write_res = time::timeout(
                                    Duration::from_millis(self.config.io_timeout_ms),
                                    ctx.write_multiple_registers(mapping.address, &ordered_words),
                                )
                                .await;

                                // put context back into the persistent slot so it can be reused
                                {
                                    let mut guard_back = self.client.lock().await;
                                    *guard_back = Some(ctx);
                                }

                                match write_res {
                                    Ok(Ok(Ok(()))) => {
                                        // On success: update registry with the requested value (authoritative path)
                                        let source_ts = chrono::Utc::now();
                                        let _ = self.registry.update_tag_value(
                                            mapping.tag_id.as_ref(),
                                            req.value.clone(),
                                            TagQuality::Good,
                                            source_ts,
                                        );
                                        if let Some(reply) = req.reply {
                                            let _ = reply.send(Ok(()));
                                        }
                                    }
                                    Ok(Ok(Err(e))) => {
                                        let emsg = format!("{:?}", e);
                                        error!(error = ?e, "Modbus write failed for {}", mapping.tag_id);
                                        if emsg.contains("Illegal")
                                            || emsg.contains("Address")
                                            || emsg.contains("IllegalDataAddress")
                                        {
                                            let _ = self.registry.set_tag_quality(
                                                mapping.tag_id.as_ref(),
                                                TagQuality::ConfigError,
                                            );
                                            if let Some(reply) = req.reply {
                                                let _ = reply.send(Err(format!(
                                                    "Modbus exception: {}",
                                                    emsg
                                                )));
                                            }
                                            continue;
                                        } else {
                                            let _ = self.registry.set_tag_quality(
                                                mapping.tag_id.as_ref(),
                                                TagQuality::CommLost,
                                            );
                                            if let Some(reply) = req.reply {
                                                let _ = reply
                                                    .send(Err(format!("Modbus error: {}", emsg)));
                                            }
                                            let _ = self
                                                .send_health_event(
                                                    "write_error",
                                                    Some(emsg.clone()),
                                                )
                                                .await;
                                            let mut guard = self.client.lock().await;
                                            *guard = None;
                                            return Err(DriverError::Modbus(emsg));
                                        }
                                    }
                                    Ok(Err(transport)) => {
                                        let emsg = format!("Transport: {}", transport);
                                        error!(error = %transport, "Modbus transport error for {}", mapping.tag_id);
                                        let _ = self.registry.set_tag_quality(
                                            mapping.tag_id.as_ref(),
                                            TagQuality::CommLost,
                                        );
                                        if let Some(reply) = req.reply {
                                            let _ = reply.send(Err(emsg.clone()));
                                        }
                                        let mut guard = self.client.lock().await;
                                        *guard = None;
                                        return Err(DriverError::Modbus(emsg));
                                    }
                                    Err(_) => {
                                        warn!("Modbus write timed out for {}", mapping.tag_id);
                                        let _ = self.registry.set_tag_quality(
                                            mapping.tag_id.as_ref(),
                                            TagQuality::CommLost,
                                        );
                                        if let Some(reply) = req.reply {
                                            let _ = reply.send(Err("Timeout".to_string()));
                                        }
                                        let mut guard = self.client.lock().await;
                                        *guard = None;
                                        return Err(DriverError::Timeout);
                                    }
                                }
                            }
                            _ => {
                                warn!(tag = %mapping.tag_id, "Write requested for unsupported Modbus function");
                                let _ = self
                                    .registry
                                    .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                                if let Some(reply) = req.reply {
                                    let _ =
                                        reply.send(Err("Unsupported write function".to_string()));
                                }
                            }
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        warn!("Write queue disconnected");
                        break;
                    }
                }
            }
        }

        // Reuse or establish persistent client context for reads.
        let io_timeout = Duration::from_millis(self.config.io_timeout_ms);
        {
            let mut guard = self.client.lock().await;
            if guard.is_none() {
                match self.connect_with_backoff().await {
                    Ok(ctx_new) => {
                        *guard = Some(ctx_new);
                    }
                    Err(e) => {
                        error!(error = ?e, "Failed to establish persistent Modbus context");
                        return Err(e);
                    }
                }
            }
        }

        // Take the persistent context out of the mutex so we do not hold the mutex across awaits.
        let mut maybe_ctx = {
            let mut guard = self.client.lock().await;
            guard.take()
        };
        if maybe_ctx.is_none() {
            match self.connect_with_backoff().await {
                Ok(ctx_new) => maybe_ctx = Some(ctx_new),
                Err(e) => {
                    error!(error = ?e, "Failed to obtain Modbus context");
                    return Err(e);
                }
            }
        }
        let ctx_ref = maybe_ctx
            .as_mut()
            .ok_or_else(|| DriverError::Other("No Modbus context available".into()))?;

        // Group mappings by function and address to minimize round-trips.
        let mut by_func: HashMap<ModbusFunction, Vec<ModbusMapping>> = HashMap::new();
        for m in &self.config.mappings {
            by_func
                .entry(m.function.clone())
                .or_default()
                .push(m.clone());
        }

        // Protocol maxima
        const MAX_REGISTERS: u16 = 125;
        const MAX_COILS: u16 = 2000;

        for (func, mut maps) in by_func.into_iter() {
            if maps.is_empty() {
                continue;
            }
            maps.sort_by_key(|m| m.address);

            // build contiguous groups
            let mut groups: Vec<(u16, u16, Vec<ModbusMapping>)> = Vec::new(); // (start_addr, quantity, mappings)
            for m in maps.into_iter() {
                if let Some(last) = groups.last_mut() {
                    let (start, qty, vecm) = last;
                    let end = start.wrapping_add(*qty).wrapping_sub(1);
                    if m.address <= end.wrapping_add(1) {
                        let new_end = std::cmp::max(end, m.address + m.quantity - 1);
                        *qty = new_end - *start + 1;
                        vecm.push(m);
                    } else {
                        groups.push((m.address, m.quantity, vec![m]));
                    }
                } else {
                    groups.push((m.address, m.quantity, vec![m]));
                }
            }

            // perform reads per group, splitting into protocol-safe chunks if needed
            for (start_addr, quantity, group_members) in groups {
                // determine max_allowed for this function
                let max_allowed: u16 = match func {
                    ModbusFunction::HoldingRegisters | ModbusFunction::InputRegisters => {
                        MAX_REGISTERS
                    }
                    ModbusFunction::Coils | ModbusFunction::DiscreteInputs => MAX_COILS,
                };

                // We'll read in chunks starting at `chunk_start` for `chunk_qty` words/coils
                let mut chunk_start = start_addr;
                let mut remaining = quantity as u32; // operate in u32 to avoid overflow in arithmetic

                while remaining > 0 {
                    let chunk_qty = std::cmp::min(remaining as u16, max_allowed) as u16;
                    // perform function-specific read using ctx_ref
                    let read_result = match func {
                        ModbusFunction::HoldingRegisters => {
                            match time::timeout(
                                io_timeout,
                                ctx_ref.read_holding_registers(chunk_start, chunk_qty),
                            )
                            .await
                            {
                                Ok(Ok(Ok(regs))) => Ok((
                                    ModbusDriver::registers_to_bytes(&regs),
                                    Some(regs.len() as u16),
                                )),
                                Ok(Ok(Err(e))) => Err(format!("{:?}", e)),
                                Ok(Err(transport)) => Err(format!("Transport: {}", transport)),
                                Err(_) => Err("Timeout".to_string()),
                            }
                        }
                        ModbusFunction::InputRegisters => {
                            match time::timeout(
                                io_timeout,
                                ctx_ref.read_input_registers(chunk_start, chunk_qty),
                            )
                            .await
                            {
                                Ok(Ok(Ok(regs))) => Ok((
                                    ModbusDriver::registers_to_bytes(&regs),
                                    Some(regs.len() as u16),
                                )),
                                Ok(Ok(Err(e))) => Err(format!("{:?}", e)),
                                Ok(Err(transport)) => Err(format!("Transport: {}", transport)),
                                Err(_) => Err("Timeout".to_string()),
                            }
                        }
                        ModbusFunction::Coils => {
                            match time::timeout(
                                io_timeout,
                                ctx_ref.read_coils(chunk_start, chunk_qty),
                            )
                            .await
                            {
                                Ok(Ok(Ok(bits))) => {
                                    // pack bits into words
                                    let mut regs = Vec::with_capacity(bits.len().div_ceil(16));
                                    let mut i = 0usize;
                                    while i < bits.len() {
                                        let mut w: u16 = 0;
                                        for bit in 0..16 {
                                            if i + bit < bits.len() && bits[i + bit] {
                                                w |= 1 << bit;
                                            }
                                        }
                                        regs.push(w);
                                        i += 16;
                                    }
                                    Ok((
                                        ModbusDriver::registers_to_bytes(&regs),
                                        Some(regs.len() as u16),
                                    ))
                                }
                                Ok(Ok(Err(e))) => Err(format!("{:?}", e)),
                                Ok(Err(transport)) => Err(format!("Transport: {}", transport)),
                                Err(_) => Err("Timeout".to_string()),
                            }
                        }
                        ModbusFunction::DiscreteInputs => {
                            match time::timeout(
                                io_timeout,
                                ctx_ref.read_discrete_inputs(chunk_start, chunk_qty),
                            )
                            .await
                            {
                                Ok(Ok(Ok(bits))) => {
                                    let mut regs = Vec::with_capacity(bits.len().div_ceil(16));
                                    let mut i = 0usize;
                                    while i < bits.len() {
                                        let mut w: u16 = 0;
                                        for bit in 0..16 {
                                            if i + bit < bits.len() && bits[i + bit] {
                                                w |= 1 << bit;
                                            }
                                        }
                                        regs.push(w);
                                        i += 16;
                                    }
                                    Ok((
                                        ModbusDriver::registers_to_bytes(&regs),
                                        Some(regs.len() as u16),
                                    ))
                                }
                                Ok(Ok(Err(e))) => Err(format!("{:?}", e)),
                                Ok(Err(transport)) => Err(format!("Transport: {}", transport)),
                                Err(_) => Err("Timeout".to_string()),
                            }
                        }
                    };

                    match read_result {
                        Ok((raw_bytes, _reg_count_opt)) => {
                            // For each mapping in the group, extract the portion that intersects this chunk and decode it.
                            let chunk_start_u32 = chunk_start as u32;
                            let chunk_end_u32 = chunk_start_u32 + (chunk_qty as u32) - 1;
                            for mapping in &group_members {
                                let map_start_u32 = mapping.address as u32;
                                let map_end_u32 = map_start_u32 + (mapping.quantity as u32) - 1;
                                // Check intersection
                                if map_end_u32 < chunk_start_u32 || map_start_u32 > chunk_end_u32 {
                                    // mapping not covered by this chunk
                                    continue;
                                }
                                // overlap range in word addresses
                                let overlap_start = std::cmp::max(map_start_u32, chunk_start_u32);
                                let overlap_end = std::cmp::min(map_end_u32, chunk_end_u32);
                                let offset_words = (overlap_start - chunk_start_u32) as usize;
                                let len_words = (overlap_end - overlap_start + 1) as usize;
                                let off_bytes = offset_words * 2;
                                let len_bytes = len_words * 2;
                                if off_bytes + len_bytes > raw_bytes.len() {
                                    warn!(tag = %mapping.tag_id, "Short payload for mapping in chunk; expected {} bytes, have {}", len_bytes, raw_bytes.len() - off_bytes);
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::ConfigError,
                                    );
                                    continue;
                                }
                                let slice = raw_bytes[off_bytes..off_bytes + len_bytes].to_vec();
                                let ordered =
                                    ModbusDriver::apply_byte_order(slice, &mapping.byte_order);
                                match self.registry.get_tag(mapping.tag_id.as_ref()) {
                                    Ok(_existing) => {
                                        match ModbusDriver::decode_bytes_to_tagvalue(
                                            &mapping.data_type,
                                            &ordered,
                                            &mapping.function,
                                            mapping.bit_offset,
                                        ) {
                                            Ok(tv) => {
                                                let source_ts = chrono::Utc::now();
                                                let _ = self.registry.update_tag_value(
                                                    mapping.tag_id.as_ref(),
                                                    tv,
                                                    TagQuality::Good,
                                                    source_ts,
                                                );
                                            }
                                            Err(e) => {
                                                warn!(tag = %mapping.tag_id, error = %e, "Failed to decode registers");
                                                let _ = self.registry.set_tag_quality(
                                                    mapping.tag_id.as_ref(),
                                                    TagQuality::ConfigError,
                                                );
                                            }
                                        }
                                    }
                                    Err(_) => {
                                        // Not found in runtime: mark as bad and continue (prevents noisy logs).
                                        let _ = self.registry.set_tag_quality(
                                            mapping.tag_id.as_ref(),
                                            TagQuality::CommLost,
                                        );
                                    }
                                }
                            }
                        }
                        Err(err_str) => {
                            let is_illegal = err_str.contains("Illegal")
                                || err_str.contains("Address")
                                || err_str.contains("IllegalDataAddress");
                            let is_device_failure = err_str.contains("Slave")
                                && (err_str.contains("Failure")
                                    || err_str.contains("Device")
                                    || err_str.contains("Unavailable"));
                            if is_illegal {
                                for mapping in &group_members {
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::ConfigError,
                                    );
                                }
                                // advance to next chunk
                            } else if is_device_failure || err_str == "Timeout" {
                                for mapping in &group_members {
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::CommLost,
                                    );
                                }
                                let _ = self
                                    .send_health_event("read_error", Some(err_str.clone()))
                                    .await;
                                let mut guard = self.client.lock().await;
                                *guard = None;
                                return Err(DriverError::Modbus(err_str));
                            }
                        }
                    }

                    // advance to next chunk
                    let advance = chunk_qty as u32;
                    let next = (chunk_start as u32).wrapping_add(advance);
                    if next > u16::MAX as u32 {
                        error!("Modbus chunk_start overflow at address {}", chunk_start);
                        break;
                    }
                    chunk_start = next as u16;
                    remaining = remaining.saturating_sub(advance);
                } // end while remaining > 0
            } // end for groups
        } // end for by_func

        // Put the persistent context back into the mutex so next cycle can reuse it.
        {
            let mut guard = self.client.lock().await;
            *guard = maybe_ctx;
        }
        Ok(())
    }
}

/// Convert TagValue (write request) into `Vec<u16>` registers suitable for Modbus write methods.
pub fn tagvalue_to_registers(value: &TagValue) -> Result<Vec<u16>, DriverError> {
    match value {
        TagValue::Bool(b) => Ok(vec![if *b { 1u16 } else { 0u16 }]),
        TagValue::UInt16(v) => Ok(vec![*v]),
        TagValue::Int16(v) => Ok(vec![*v as u16]),
        TagValue::UInt32(v) => Ok(vec![((*v >> 16) & 0xFFFF) as u16, (*v & 0xFFFF) as u16]),
        TagValue::Int32(v) => {
            let ux = *v as u32;
            Ok(vec![((ux >> 16) & 0xFFFF) as u16, (ux & 0xFFFF) as u16])
        }
        TagValue::Float(f) => {
            let bits = f.to_bits();
            Ok(vec![((bits >> 16) & 0xFFFF) as u16, (bits & 0xFFFF) as u16])
        }
        TagValue::Double(d) => {
            let bits = d.to_bits();
            Ok(vec![
                ((bits >> 48) & 0xFFFF) as u16,
                ((bits >> 32) & 0xFFFF) as u16,
                ((bits >> 16) & 0xFFFF) as u16,
                (bits & 0xFFFF) as u16,
            ])
        }
        TagValue::UInt64(v) => {
            let bits = *v;
            Ok(vec![
                ((bits >> 48) & 0xFFFF) as u16,
                ((bits >> 32) & 0xFFFF) as u16,
                ((bits >> 16) & 0xFFFF) as u16,
                (bits & 0xFFFF) as u16,
            ])
        }
        TagValue::Int64(v) => {
            let bits = *v as u64;
            Ok(vec![
                ((bits >> 48) & 0xFFFF) as u16,
                ((bits >> 32) & 0xFFFF) as u16,
                ((bits >> 16) & 0xFFFF) as u16,
                (bits & 0xFFFF) as u16,
            ])
        }
        TagValue::DateTime(dt) => {
            // Convert to milliseconds since UNIX epoch and store as four words (big-endian)
            let ms = dt.timestamp_millis() as u64;
            Ok(vec![
                ((ms >> 48) & 0xFFFF) as u16,
                ((ms >> 32) & 0xFFFF) as u16,
                ((ms >> 16) & 0xFFFF) as u16,
                (ms & 0xFFFF) as u16,
            ])
        }
        TagValue::ByteString(b) => {
            let mut bytes = b.clone();
            if bytes.len() % 2 != 0 {
                bytes.push(0);
            }
            let mut regs = Vec::with_capacity(bytes.len() / 2);
            for chunk in bytes.chunks(2) {
                regs.push(((chunk[0] as u16) << 8) | (chunk[1] as u16));
            }
            Ok(regs)
        }
        TagValue::String(s) => {
            let mut bytes = s.as_bytes().to_vec();
            if bytes.len() % 2 != 0 {
                bytes.push(0);
            }
            let mut regs = Vec::with_capacity(bytes.len() / 2);
            for chunk in bytes.chunks(2) {
                regs.push(((chunk[0] as u16) << 8) | (chunk[1] as u16));
            }
            Ok(regs)
        }
    }
}

#[async_trait]
impl ProtocolDriver for ModbusDriver {
    fn validate(&self) -> Result<(), DynDriverError> {
        use std::collections::HashMap;

        for m in &self.config.mappings {
            if m.quantity == 0 {
                return Err(Box::new(DriverError::mapping(format!(
                    "Mapping '{}' has zero quantity",
                    m.tag_id
                ))));
            }
            if m.bit_offset >= 16 {
                return Err(Box::new(DriverError::mapping(format!(
                    "Mapping '{}' has invalid bit_offset {} (must be 0..15)",
                    m.tag_id, m.bit_offset
                ))));
            }

            match m.data_type {
                TagDataType::Float | TagDataType::Int32 | TagDataType::UInt32 if m.quantity < 2 => {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Mapping '{}' data_type requires quantity >= 2",
                        m.tag_id
                    ))));
                }
                TagDataType::Double if m.quantity < 4 => {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Mapping '{}' data_type 'Double' requires quantity >= 4",
                        m.tag_id
                    ))));
                }
                _ => {}
            }

            match m.function {
                ModbusFunction::InputRegisters | ModbusFunction::DiscreteInputs if m.writable => {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Mapping '{}' marked writable but uses read-only function {:?}",
                        m.tag_id, m.function
                    ))));
                }
                _ => {}
            }

            if (m.byte_order == WordOrder::CDAB || m.byte_order == WordOrder::DCBA)
                && m.quantity < 2
            {
                return Err(Box::new(DriverError::mapping(format!(
                    "Mapping '{}' uses byte_order {:?} which requires quantity >= 2",
                    m.tag_id, m.byte_order
                ))));
            }
        }

        let mut by_func: HashMap<ModbusFunction, Vec<&ModbusMapping>> = HashMap::new();
        for m in &self.config.mappings {
            by_func.entry(m.function.clone()).or_default().push(m);
        }
        for (func, mut vecm) in by_func.into_iter() {
            vecm.sort_by_key(|m| m.address);
            for w in 0..vecm.len().saturating_sub(1) {
                let cur = vecm[w];
                let next = vecm[w + 1];
                let cur_end = cur.address.saturating_add(cur.quantity).saturating_sub(1);
                if next.address <= cur_end {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Overlapping Modbus ranges for function {:?}: '{}'[{}..{}] overlaps '{}'[{}..{}]",
                        func,
                        cur.tag_id,
                        cur.address,
                        cur_end,
                        next.tag_id,
                        next.address,
                        next.address.saturating_add(next.quantity).saturating_sub(1)
                    ))));
                }
            }
        }
        Ok(())
    }

    async fn read_cycle(&self) -> Result<(), DynDriverError> {
        // Delegate to the internal implementation and convert the concrete DriverError
        // into the boxed DynDriverError used by the runtime abstraction.
        self.run_read_cycle_impl()
            .await
            .map_err(|e| Box::new(e) as DynDriverError)
    }

    async fn submit_write(&self, tag_id: &str, value: TagValue) -> Result<(), DynDriverError> {
        use tokio::sync::oneshot;
        use tokio::time::{timeout, Duration};

        // Create oneshot reply channel for confirmation
        let (tx, rx) = oneshot::channel::<Result<(), String>>();
        let req = WriteRequest {
            tag_id: tag_id.to_string(),
            value,
            reply: Some(tx),
        };

        // Enqueue the request
        self.write_tx
            .send(req)
            .await
            .map_err(|e| Box::new(e) as DynDriverError)?;

        // Wait for confirmation (bounded)
        match timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(msg))) => Err(Box::new(std::io::Error::other(msg))),
            Ok(Err(_)) => Err(Box::new(std::io::Error::other("reply channel closed"))),
            Err(_) => Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "write confirmation timed out",
            ))),
        }
    }

    async fn health(&self) -> Result<Option<serde_json::Value>, DynDriverError> {
        // No synchronous health snapshot available; health events are emitted via the optional sender.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::{TagValue as CMTagValue, WordOrder};
    use std::net::SocketAddr;
    use std::str::FromStr;

    /// #feature DRV-MODBUS
    #[tokio::test]
    async fn instantiate_driver_and_queue_write() {
        // Build a TagRegistry containing a single definition for "tag1".
        // The registry constructor will populate the runtime TagStore with the
        // zero-equivalent initial value for the configured type.
        let defs = vec![core_model::TagDefinition::new(
            "tag1",
            "tag1",
            "ADDR1",
            TagDataType::UInt16,
            "test-modbus",
        )];
        let registry =
            Arc::new(core_model::TagRegistry::from_definitions(&defs).expect("build registry"));

        let cfg = ModbusConfig {
            name: "test-modbus".into(),
            endpoint: SocketAddr::from_str("127.0.0.1:502").unwrap(),
            unit_id: 1,
            cycle_ms: 1000,
            mappings: vec![ModbusMapping::new(
                "tag1",
                0,
                1,
                ModbusFunction::HoldingRegisters,
                TagDataType::UInt16,
                0,
                true,
                WordOrder::ABCD,
            )],
            keepalive_secs: 0,
            max_backoff_secs: 1,
            io_timeout_ms: 2000,
        };
        let drv = ModbusDriver::new(cfg, registry.clone());
        let sender = drv.write_sender();
        sender
            .try_send(WriteRequest::new("tag1", CMTagValue::UInt16(1234)))
            .expect("should queue write");
    }

    /// #feature DRV-MODBUS, UA-TYPES
    #[test]
    fn register_bytes_endianness() {
        let regs = vec![0x1234u16, 0xABCDu16];
        let bytes = ModbusDriver::registers_to_bytes(&regs);
        assert_eq!(bytes, vec![0x12, 0x34, 0xAB, 0xCD]);
    }
}
