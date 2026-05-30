//! FINS/TCP driver: connects to an Omron PLC, performs grouped read cycles, handles writes,
//! updates the TagRegistry and emits health events.

use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::{BufMut, BytesMut};
use chrono::Utc;
use core_model::tag_value::TagQuality;
use core_model::{TagDataType, TagRegistry, TagValue, WordOrder};
use serde_json::Value;
use std::io::Cursor;

use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use tracing::{error, info, warn};

use crate::config::FinsConfig;
use crate::errors::DriverError;
use crate::mapping::TagMapping;
use crate::write_request::WriteRequest;

use std::sync::atomic::{AtomicU8, Ordering};

/// ReadGroup precomputed from mappings — groups contiguous addresses within the same
/// FINS area so the driver can perform bulk reads efficiently.
///
/// Each `ReadGroup` represents a single FINS read request chunk (already split
/// by `max_words_per_request`).
#[derive(Debug, Clone)]
struct ReadGroup {
    area: u8,
    start_address: u32,
    word_count: u16,
    mappings: Vec<TagMapping>,
}

/// FinsDriver holds connection state and interacts with the TagRegistry.
pub struct FinsDriver {
    config: FinsConfig,
    registry: Arc<TagRegistry>,
    write_tx: mpsc::Sender<WriteRequest>,
    write_rx: Arc<Mutex<mpsc::Receiver<WriteRequest>>>,
    /// Session id counter (atomic u8 to avoid async mutex contention).
    sid_counter: Arc<AtomicU8>,
    /// Connection guard.
    conn: Mutex<Option<TcpStream>>,
    /// Precomputed read groups derived from `config.mappings`.
    read_groups: Vec<ReadGroup>,
    health_tx: Arc<Mutex<Option<mpsc::Sender<Value>>>>,
}

impl FinsDriver {
    pub fn new(config: FinsConfig, registry: Arc<TagRegistry>) -> Self {
        let (tx, rx) = mpsc::channel(256);

        // Precompute read groups from mappings: group contiguous addresses within each area.
        use std::collections::BTreeMap;
        let mut by_area: BTreeMap<u8, Vec<TagMapping>> = BTreeMap::new();
        for m in config.mappings.iter() {
            by_area.entry(m.area).or_default().push(m.clone());
        }

        let mut read_groups: Vec<ReadGroup> = Vec::new();
        for (area, mut maps) in by_area.into_iter() {
            if maps.is_empty() {
                continue;
            }
            maps.sort_by_key(|m| m.address);

            let mut i = 0usize;
            while i < maps.len() {
                let start_addr = maps[i].address;
                let mut end_addr = maps[i].address + (maps[i].word_count as u32) - 1;
                let mut group_mappings = vec![maps[i].clone()];
                let mut j = i + 1;
                while j < maps.len() {
                    let next = &maps[j];
                    if next.address <= end_addr + 1 {
                        let candidate_end = next.address + (next.word_count as u32) - 1;
                        if candidate_end > end_addr {
                            end_addr = candidate_end;
                        }
                        group_mappings.push(next.clone());
                        j += 1;
                    } else {
                        break;
                    }
                }

                let total_words = end_addr - start_addr + 1;
                // Split groups into chunks that fit within max_words_per_request.
                let max_fins_words = config.max_words_per_request;
                let mut chunk_start = start_addr;
                let mut remaining = total_words;
                while remaining > 0 {
                    let chunk_words = std::cmp::min(remaining, max_fins_words) as u16;
                    read_groups.push(ReadGroup {
                        area,
                        start_address: chunk_start,
                        word_count: chunk_words,
                        mappings: group_mappings.clone(),
                    });
                    let advance = chunk_words as u32;
                    chunk_start += advance;
                    remaining = remaining.saturating_sub(advance);
                }

                i = j;
            }
        }

        Self {
            config,
            registry,
            write_tx: tx,
            write_rx: Arc::new(Mutex::new(rx)),
            sid_counter: Arc::new(AtomicU8::new(1)),
            conn: Mutex::new(None),
            read_groups,
            health_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn write_sender(&self) -> mpsc::Sender<WriteRequest> {
        self.write_tx.clone()
    }

    async fn connect_with_backoff(&self) -> Result<TcpStream, DriverError> {
        let mut backoff = 1u64;
        loop {
            match TcpStream::connect(self.config.endpoint).await {
                Ok(stream) => {
                    info!(endpoint = %self.config.endpoint, "Connected to FINS PLC");
                    return Ok(stream);
                }
                Err(e) => {
                    warn!(error = %e, endpoint = %self.config.endpoint, "Failed to connect, retrying");
                    time::sleep(Duration::from_secs(backoff)).await;
                    backoff =
                        std::cmp::min(backoff.saturating_mul(2), self.config.max_backoff_secs);
                }
            }
        }
    }

    fn wrap_fins_tcp(payload: &[u8]) -> Vec<u8> {
        let mut buf = BytesMut::with_capacity(8 + payload.len());
        buf.put(&b"FINS"[..]);
        buf.put_u32(payload.len() as u32);
        buf.put_slice(payload);
        buf.to_vec()
    }

    fn build_fins_frame(sid: u8, command: [u8; 2], params: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(10 + 2 + params.len());
        v.push(0x80); // ICF
        v.push(0x00); // RSV
        v.push(0x02); // GCT
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // addressing placeholders
        v.push(sid);
        v.push(command[0]);
        v.push(command[1]);
        v.extend_from_slice(params);
        v
    }

    fn build_memory_read(area: u8, address: u32, bit_offset: u8, word_count: u16) -> Vec<u8> {
        let mut p = Vec::with_capacity(7);
        p.push(area);
        p.push(((address >> 16) & 0xFF) as u8);
        p.push(((address >> 8) & 0xFF) as u8);
        p.push((address & 0xFF) as u8);
        p.push(bit_offset);
        WriteBytesExt::write_u16::<BigEndian>(&mut p, word_count).unwrap_or(());
        p
    }

    fn build_memory_write(area: u8, address: u32, bit_offset: u8, words: &[u16]) -> Vec<u8> {
        let mut p = Vec::with_capacity(7 + words.len() * 2);
        p.push(area);
        p.push(((address >> 16) & 0xFF) as u8);
        p.push(((address >> 8) & 0xFF) as u8);
        p.push((address & 0xFF) as u8);
        p.push(bit_offset);
        WriteBytesExt::write_u16::<BigEndian>(&mut p, words.len() as u16).unwrap_or(());
        for w in words {
            WriteBytesExt::write_u16::<BigEndian>(&mut p, *w).unwrap_or(());
        }
        p
    }

    fn parse_fins_response(frame: &[u8]) -> Result<(u8, u16, Vec<u8>), DriverError> {
        if frame.len() < 14 {
            return Err(DriverError::Protocol("Frame too short".into()));
        }
        // FINS response frame layout:
        // [0] ICF, [1] RSV, [2] GCT, [3-5] DNA, [6-8] SNA, [9] SID,
        // [10-11] Command, [12-13] EndCode, [14+] Data
        let sid = frame[9];
        let end_code = u16::from_be_bytes([frame[12], frame[13]]);
        let data = frame[14..].to_vec();
        Ok((sid, end_code, data))
    }

    fn apply_byte_order(mut bytes: Vec<u8>, order: &WordOrder) -> Vec<u8> {
        order.apply_to_bytes(&mut bytes);
        bytes
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_fixed<T>(
        &self,
        mapping: &TagMapping,
        data_bytes: &[u8],
        size: usize,
        type_name: &str,
        source_ts: chrono::DateTime<Utc>,
        read: impl FnOnce(&mut Cursor<&[u8]>) -> Result<T, std::io::Error>,
        into_value: impl FnOnce(T) -> TagValue,
    ) -> Result<(), DriverError> {
        if data_bytes.len() < size {
            return Err(DriverError::Protocol(format!(
                "Not enough data for {type_name}"
            )));
        }
        let mut rdr = Cursor::new(&data_bytes[..size]);
        let v = read(&mut rdr)
            .map_err(|e| DriverError::Protocol(format!("Failed to decode {type_name}: {e}")))?;
        let _ = self.registry.update_tag_value(
            mapping.tag_id.as_ref(),
            into_value(v),
            TagQuality::Good,
            source_ts,
        );
        Ok(())
    }

    async fn decode_and_update_tag(
        &self,
        mapping: &TagMapping,
        mut data_bytes: Vec<u8>,
    ) -> Result<(), DriverError> {
        data_bytes = Self::apply_byte_order(data_bytes, &mapping.byte_order);

        // Ensure the tag definition exists in the registry before decoding.
        if self
            .registry
            .get_definition(mapping.tag_id.as_ref())
            .is_err()
        {
            return Err(DriverError::Mapping(format!(
                "Tag id not found in registry: {}",
                mapping.tag_id.as_ref()
            )));
        }

        let source_ts = chrono::Utc::now();
        use core_model::TagDataType::*;
        match mapping.data_type {
            Bool => {
                if data_bytes.len() < 2 {
                    return Err(DriverError::Protocol("Not enough data for Bool".into()));
                }
                let mut rdr = Cursor::new(&data_bytes[0..2]);
                let word = ReadBytesExt::read_u16::<BigEndian>(&mut rdr).unwrap_or(0);
                let bit = ((word >> mapping.bit_offset) & 0x1) != 0;
                let _ = self.registry.update_tag_value(
                    mapping.tag_id.as_ref(),
                    TagValue::Bool(bit),
                    TagQuality::Good,
                    source_ts,
                );
            }
            UInt16 => self.decode_fixed(
                mapping,
                &data_bytes,
                2,
                "UInt16",
                source_ts,
                |rdr| ReadBytesExt::read_u16::<BigEndian>(rdr),
                TagValue::UInt16,
            )?,
            Int16 => self.decode_fixed(
                mapping,
                &data_bytes,
                2,
                "Int16",
                source_ts,
                |rdr| ReadBytesExt::read_i16::<BigEndian>(rdr),
                TagValue::Int16,
            )?,
            UInt32 => self.decode_fixed(
                mapping,
                &data_bytes,
                4,
                "UInt32",
                source_ts,
                |rdr| ReadBytesExt::read_u32::<BigEndian>(rdr),
                TagValue::UInt32,
            )?,
            Int32 => self.decode_fixed(
                mapping,
                &data_bytes,
                4,
                "Int32",
                source_ts,
                |rdr| ReadBytesExt::read_i32::<BigEndian>(rdr),
                TagValue::Int32,
            )?,
            Float => self.decode_fixed(
                mapping,
                &data_bytes,
                4,
                "Float",
                source_ts,
                |rdr| ReadBytesExt::read_u32::<BigEndian>(rdr).map(f32::from_bits),
                TagValue::Float,
            )?,
            Double => self.decode_fixed(
                mapping,
                &data_bytes,
                8,
                "Double",
                source_ts,
                |rdr| ReadBytesExt::read_u64::<BigEndian>(rdr).map(f64::from_bits),
                TagValue::Double,
            )?,
            UInt64 => self.decode_fixed(
                mapping,
                &data_bytes,
                8,
                "UInt64",
                source_ts,
                |rdr| ReadBytesExt::read_u64::<BigEndian>(rdr),
                TagValue::UInt64,
            )?,
            Int64 => self.decode_fixed(
                mapping,
                &data_bytes,
                8,
                "Int64",
                source_ts,
                |rdr| ReadBytesExt::read_i64::<BigEndian>(rdr),
                TagValue::Int64,
            )?,
            DateTime => {
                if data_bytes.len() >= 8 {
                    let mut rdr = Cursor::new(&data_bytes[0..8]);
                    let ms = ReadBytesExt::read_u64::<BigEndian>(&mut rdr).unwrap_or(0);
                    let st = SystemTime::UNIX_EPOCH + Duration::from_millis(ms);
                    let dt = chrono::DateTime::<Utc>::from(st);
                    let _ = self.registry.update_tag_value(
                        mapping.tag_id.as_ref(),
                        TagValue::DateTime(dt),
                        TagQuality::Good,
                        source_ts,
                    );
                } else if let Ok(st) = std::str::from_utf8(&data_bytes) {
                    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(st) {
                        let dt = parsed.with_timezone(&Utc);
                        let _ = self.registry.update_tag_value(
                            mapping.tag_id.as_ref(),
                            TagValue::DateTime(dt),
                            TagQuality::Good,
                            source_ts,
                        );
                    } else {
                        return Err(DriverError::Protocol("Invalid DateTime format".into()));
                    }
                } else {
                    return Err(DriverError::Protocol("Not enough data for DateTime".into()));
                }
            }
            ByteString => {
                let _ = self.registry.update_tag_value(
                    mapping.tag_id.as_ref(),
                    TagValue::ByteString(data_bytes),
                    TagQuality::Good,
                    source_ts,
                );
            }
            String => {
                let s = match std::str::from_utf8(&data_bytes) {
                    Ok(st) => st.trim_end_matches('\0').to_string(),
                    Err(_) => data_bytes
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(""),
                };
                let _ = self.registry.update_tag_value(
                    mapping.tag_id.as_ref(),
                    TagValue::String(s),
                    TagQuality::Good,
                    source_ts,
                );
            }
        }
        Ok(())
    }

    /// Split a `u64` into four big-endian u16 words (most significant first).
    fn u64_to_words(v: u64) -> Vec<u16> {
        vec![
            ((v >> 48) & 0xFFFF) as u16,
            ((v >> 32) & 0xFFFF) as u16,
            ((v >> 16) & 0xFFFF) as u16,
            (v & 0xFFFF) as u16,
        ]
    }

    /// Split a `u32` into two big-endian u16 words (most significant first).
    fn u32_to_words(v: u32) -> Vec<u16> {
        vec![((v >> 16) & 0xFFFF) as u16, (v & 0xFFFF) as u16]
    }

    /// Pad a byte vector to an even length, then read it as big-endian u16 words.
    fn bytes_to_words(bytes: Vec<u8>) -> Vec<u16> {
        let mut b = bytes;
        if !b.len().is_multiple_of(2) {
            b.push(0);
        }
        let mut words = Vec::with_capacity(b.len() / 2);
        let mut rdr = Cursor::new(b);
        while let Ok(w) = ReadBytesExt::read_u16::<BigEndian>(&mut rdr) {
            words.push(w);
        }
        words
    }

    fn tagvalue_to_words_standard(v: &TagValue) -> Result<Vec<u16>, DriverError> {
        match v {
            TagValue::Bool(b) => Ok(vec![if *b { 1 } else { 0 }]),
            TagValue::UInt16(x) => Ok(vec![*x]),
            TagValue::Int16(x) => Ok(vec![*x as u16]),
            TagValue::UInt32(x) => Ok(Self::u32_to_words(*x)),
            TagValue::Int32(x) => Ok(Self::u32_to_words(*x as u32)),
            TagValue::Float(f) => Ok(Self::u32_to_words(f.to_bits())),
            TagValue::Double(d) => Ok(Self::u64_to_words(d.to_bits())),
            TagValue::UInt64(x) => Ok(Self::u64_to_words(*x)),
            TagValue::Int64(x) => Ok(Self::u64_to_words(*x as u64)),
            TagValue::DateTime(dt) => Ok(Self::u64_to_words(dt.timestamp_millis() as u64)),
            TagValue::ByteString(b) => Ok(Self::bytes_to_words(b.clone())),
            TagValue::String(s) => Ok(Self::bytes_to_words(s.as_bytes().to_vec())),
        }
    }

    async fn next_sid(&self) -> u8 {
        // Atomic increment — fetch_add returns previous value. Using SeqCst for simplicity.
        self.sid_counter.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn set_health_sender(&self, tx: mpsc::Sender<Value>) {
        let mut guard = self.health_tx.lock().await;
        *guard = Some(tx);
    }

    async fn send_health_event(&self, status: &str, detail: Option<String>) {
        let guard = self.health_tx.lock().await;
        if let Some(tx) = &*guard {
            // Build a consistent health JSON object:
            // {
            //   "plc": "<name>",
            //   "status": "ok" | "error",
            //   "detail": { "message": "<text>", "data": <value|null> }
            // }
            let mut map = serde_json::Map::new();
            map.insert("plc".to_string(), Value::String(self.config.name.clone()));
            map.insert("status".to_string(), Value::String(status.to_string()));

            let mut detail_obj = serde_json::Map::new();
            match detail {
                Some(d) => {
                    detail_obj.insert("message".to_string(), Value::String(d));
                    detail_obj.insert("data".to_string(), Value::Null);
                }
                None => {
                    // No extra detail provided; use status as the message and null data.
                    detail_obj.insert("message".to_string(), Value::String(status.to_string()));
                    detail_obj.insert("data".to_string(), Value::Null);
                }
            }
            map.insert("detail".to_string(), Value::Object(detail_obj));

            // Use non-blocking send so health emission doesn't backpressure drivers.
            let _ = tx.try_send(Value::Object(map));
        }
    }

    /// Internal implementation of a single read/write cycle, returning the
    /// driver-specific `DriverError`. The runtime-facing `ProtocolDriver` adapter
    /// calls this and converts errors into the boxed `DynDriverError`.
    async fn run_read_cycle_impl(&self) -> Result<(), DriverError> {
        {
            let maybe_conn = {
                let mut guard = self.conn.lock().await;
                guard.take()
            };
            if maybe_conn.is_none() {
                match self.connect_with_backoff().await {
                    Ok(s) => {
                        let mut guard = self.conn.lock().await;
                        *guard = Some(s);
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            } else {
                let mut guard = self.conn.lock().await;
                *guard = maybe_conn;
            }
        }

        // Drain writes (bounded number per cycle)
        {
            let mut rx = self.write_rx.lock().await;
            for _ in 0..64 {
                match rx.try_recv() {
                    Ok(req) => {
                        // Compare the mapping's Arc<str> with the incoming String
                        let mapping = self
                            .config
                            .mappings
                            .iter()
                            .find(|m| m.tag_id.as_ref() == req.tag_id)
                            .cloned();
                        let mapping = match mapping {
                            Some(m) => m,
                            None => {
                                warn!(tag = %req.tag_id, "Write request for unknown tag_id");
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Err("Unknown tag id".to_string()));
                                }
                                continue;
                            }
                        };
                        if !mapping.writable {
                            warn!(tag = %req.tag_id, "Write requested on non-writable tag; ignoring");
                            if let Some(reply) = req.reply {
                                let _ = reply.send(Err("Not writable".to_string()));
                            }
                            continue;
                        }

                        let words = match Self::tagvalue_to_words_standard(&req.value) {
                            Ok(w) => w,
                            Err(e) => {
                                warn!(error = %e, "Failed to convert TagValue to words for write");
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Err(format!("Conversion error: {}", e)));
                                }
                                continue;
                            }
                        };

                        let mut bytes = Vec::with_capacity(words.len() * 2);
                        for w in &words {
                            bytes.push((w >> 8) as u8);
                            bytes.push((w & 0xFF) as u8);
                        }
                        let ordered = Self::apply_byte_order(bytes, &mapping.byte_order);

                        if ordered.len() % 2 != 0 {
                            warn!("Ordered write bytes length not even");
                            if let Some(reply) = req.reply {
                                let _ =
                                    reply.send(Err("Ordered bytes length not even".to_string()));
                            }
                            continue;
                        }
                        let mut ordered_words = Vec::with_capacity(ordered.len() / 2);
                        let mut c = Cursor::new(&ordered);
                        while let Ok(w) = ReadBytesExt::read_u16::<BigEndian>(&mut c) {
                            ordered_words.push(w);
                        }

                        let sid = self.next_sid().await;
                        let params = Self::build_memory_write(
                            mapping.area,
                            mapping.address,
                            mapping.bit_offset,
                            &ordered_words,
                        );
                        let fins_frame = Self::build_fins_frame(sid, [0x01, 0x02], &params);
                        let wrapped = Self::wrap_fins_tcp(&fins_frame);

                        let mut maybe_stream = {
                            let mut guard = self.conn.lock().await;
                            guard.take()
                        };
                        if maybe_stream.is_none() {
                            match self.connect_with_backoff().await {
                                Ok(s) => maybe_stream = Some(s),
                                Err(e) => {
                                    error!(error = ?e, "Failed to reconnect for write");
                                    // Connection-level failure -> communication lost.
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::CommLost,
                                    );
                                    let _ = self
                                        .send_health_event("connect_failed", Some(format!("{}", e)))
                                        .await;
                                    if let Some(reply) = req.reply {
                                        let _ = reply.send(Err(format!("Reconnect failed: {}", e)));
                                    }
                                    continue;
                                }
                            }
                        }
                        let mut stream = maybe_stream.unwrap();
                        if let Err(e) = stream.write_all(&wrapped).await {
                            error!(error = ?e, "IO error writing FINS write");
                            maybe_stream = None;
                            // IO error on transport -> mark comms lost.
                            let _ = self
                                .registry
                                .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                            if let Some(reply) = req.reply {
                                let _ = reply.send(Err(format!("IO error: {}", e)));
                            }
                            let mut guard = self.conn.lock().await;
                            *guard = maybe_stream;
                            continue;
                        }
                        let mut header = [0u8; 8];
                        if let Err(e) = stream.read_exact(&mut header).await {
                            error!(error = ?e, "IO error reading FINS/TCP header for write");
                            maybe_stream = None;
                            // Read failure -> communication problem.
                            let _ = self
                                .registry
                                .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                            if let Some(reply) = req.reply {
                                let _ = reply.send(Err(format!("IO error: {}", e)));
                            }
                            let mut guard = self.conn.lock().await;
                            *guard = maybe_stream;
                            continue;
                        }
                        let mut cur = Cursor::new(&header[4..8]);
                        let len =
                            ReadBytesExt::read_u32::<BigEndian>(&mut cur).unwrap_or(0) as usize;
                        let mut payload = vec![0u8; len];
                        if let Err(e) = stream.read_exact(&mut payload).await {
                            error!(error = ?e, "IO error reading FINS/TCP payload for write");
                            maybe_stream = None;
                            // Payload read error -> communication problem.
                            let _ = self
                                .registry
                                .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                            if let Some(reply) = req.reply {
                                let _ = reply.send(Err(format!("IO error: {}", e)));
                            }
                            let mut guard = self.conn.lock().await;
                            *guard = maybe_stream;
                            continue;
                        }
                        {
                            let mut guard = self.conn.lock().await;
                            *guard = Some(stream);
                        }
                        match Self::parse_fins_response(&payload) {
                            Ok((resp_sid, end_code, _data)) => {
                                if resp_sid != sid {
                                    warn!(
                                        expected = sid,
                                        got = resp_sid,
                                        "SID mismatch on write response; dropping"
                                    );
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::CommLost,
                                    );
                                    if let Some(reply) = req.reply {
                                        let _ = reply.send(Err("SID mismatch".to_string()));
                                    }
                                    continue;
                                }
                                if end_code != 0 {
                                    warn!(end_code = end_code, "FINS write returned error code");
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::Error(format!(
                                            "FINS end code: 0x{:04X}",
                                            end_code
                                        )),
                                    );
                                    if let Some(reply) = req.reply {
                                        let _ = reply.send(Err(format!(
                                            "FINS end code: 0x{:04X}",
                                            end_code
                                        )));
                                    }
                                    continue;
                                }
                                let source_ts = chrono::Utc::now();
                                let _ = self.registry.update_tag_value(
                                    mapping.tag_id.as_ref(),
                                    req.value,
                                    TagQuality::Good,
                                    source_ts,
                                );
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Ok(()));
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "Failed to parse write response; marking Error");
                                let _ = self.registry.set_tag_quality(
                                    mapping.tag_id.as_ref(),
                                    TagQuality::Error(format!("parse_write_resp: {}", e)),
                                );
                                if let Some(reply) = req.reply {
                                    let _ = reply.send(Err(format!("Parse error: {}", e)));
                                }
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        warn!("Write queue disconnected");
                        break;
                    }
                }
            }
        }

        for group in &self.read_groups {
            let sid = self.next_sid().await;
            let params =
                Self::build_memory_read(group.area, group.start_address, 0, group.word_count);
            let fins_frame = Self::build_fins_frame(sid, [0x01, 0x01], &params);
            let wrapped = Self::wrap_fins_tcp(&fins_frame);

            let mut maybe_stream = {
                let mut guard = self.conn.lock().await;
                guard.take()
            };
            if maybe_stream.is_none() {
                match self.connect_with_backoff().await {
                    Ok(s) => maybe_stream = Some(s),
                    Err(e) => {
                        error!(error = ?e, "Failed to reconnect for read group");
                        for mapping in &group.mappings {
                            let _ = self
                                .registry
                                .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                        }
                        let _ = self
                            .send_health_event("connect_failed", Some(format!("{}", e)))
                            .await;
                        break;
                    }
                }
            }
            let mut stream = maybe_stream.unwrap();
            if let Err(e) = stream.write_all(&wrapped).await {
                error!(error = ?e, "IO error writing FINS read group");
                maybe_stream = None;
                for mapping in &group.mappings {
                    let _ = self
                        .registry
                        .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                }
                let mut guard = self.conn.lock().await;
                *guard = maybe_stream;
                break;
            }

            let mut header = [0u8; 8];
            if let Err(e) = stream.read_exact(&mut header).await {
                error!(error = ?e, "IO error reading FINS/TCP header");
                maybe_stream = None;
                for mapping in &group.mappings {
                    let _ = self
                        .registry
                        .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                }
                let mut guard = self.conn.lock().await;
                *guard = maybe_stream;
                break;
            }
            let mut cur = Cursor::new(&header[4..8]);
            let len = ReadBytesExt::read_u32::<BigEndian>(&mut cur).unwrap_or(0) as usize;
            let mut payload = vec![0u8; len];
            if let Err(e) = stream.read_exact(&mut payload).await {
                error!(error = ?e, "IO error reading FINS/TCP payload");
                maybe_stream = None;
                for mapping in &group.mappings {
                    let _ = self
                        .registry
                        .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                }
                let mut guard = self.conn.lock().await;
                *guard = maybe_stream;
                break;
            }
            {
                let mut guard = self.conn.lock().await;
                *guard = Some(stream);
            }

            match Self::parse_fins_response(&payload) {
                Ok((resp_sid, end_code, payload_bytes)) => {
                    if resp_sid != sid {
                        warn!(
                            expected = sid,
                            got = resp_sid,
                            "SID mismatch on read response; dropping"
                        );
                        for mapping in &group.mappings {
                            let _ = self
                                .registry
                                .set_tag_quality(mapping.tag_id.as_ref(), TagQuality::CommLost);
                        }
                    } else {
                        if end_code != 0 {
                            warn!(
                                end_code = end_code,
                                "FINS read returned non-zero end code; marking tags Error"
                            );
                            for mapping in &group.mappings {
                                let _ = self.registry.set_tag_quality(
                                    mapping.tag_id.as_ref(),
                                    TagQuality::Error(format!("FINS end code: 0x{:04X}", end_code)),
                                );
                            }
                            break;
                        }

                        let expected_len = (group.word_count as usize) * 2;
                        let chunk_start = group.start_address;
                        let chunk_end = group.start_address + (group.word_count as u32) - 1;
                        if payload_bytes.len() < expected_len {
                            for mapping in &group.mappings {
                                let map_start = mapping.address;
                                let map_end = mapping.address + (mapping.word_count as u32) - 1;
                                if !(map_end < chunk_start || map_start > chunk_end) {
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::Error("short_payload".into()),
                                    );
                                }
                            }
                        } else {
                            for mapping in &group.mappings {
                                let map_start = mapping.address;
                                let map_end = mapping.address + (mapping.word_count as u32) - 1;
                                if map_end < chunk_start || map_start > chunk_end {
                                    continue;
                                }
                                let overlap_start = std::cmp::max(map_start, chunk_start);
                                let overlap_end = std::cmp::min(map_end, chunk_end);
                                let offset_words = (overlap_start - chunk_start) as usize;
                                let len_words = (overlap_end - overlap_start + 1) as usize;
                                let off_bytes = offset_words * 2;
                                let len_bytes = len_words * 2;
                                if off_bytes + len_bytes > payload_bytes.len() {
                                    warn!(tag = %mapping.tag_id, "Short payload for mapping; expected {} bytes, have {}", len_bytes, payload_bytes.len() - off_bytes);
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::Error("short_payload".into()),
                                    );
                                    continue;
                                }
                                let slice =
                                    payload_bytes[off_bytes..off_bytes + len_bytes].to_vec();
                                if let Err(e) = self.decode_and_update_tag(mapping, slice).await {
                                    warn!(error = %e, tag = %mapping.tag_id, "Failed to decode or update tag; marking Error");
                                    let _ = self.registry.set_tag_quality(
                                        mapping.tag_id.as_ref(),
                                        TagQuality::Error(format!("decode_failed: {}", e)),
                                    );
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse fins response; marking Error");
                    for mapping in &group.mappings {
                        let _ = self.registry.set_tag_quality(
                            mapping.tag_id.as_ref(),
                            TagQuality::Error(format!("parse_fins_resp: {}", e)),
                        );
                    }
                    break;
                }
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl driver_common::ProtocolDriver for FinsDriver {
    fn validate(&self) -> Result<(), driver_common::DynDriverError> {
        for m in &self.config.mappings {
            if m.word_count == 0 {
                return Err(Box::new(DriverError::mapping(format!(
                    "Mapping '{}' has zero word_count",
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
                TagDataType::Float | TagDataType::Int32 | TagDataType::UInt32
                    if m.word_count < 2 =>
                {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Mapping '{}' data_type requires word_count >= 2",
                        m.tag_id
                    ))));
                }
                TagDataType::Double if m.word_count < 4 => {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Mapping '{}' data_type 'Double' requires word_count >= 4",
                        m.tag_id
                    ))));
                }
                _ => {}
            }
            if (m.byte_order == WordOrder::CDAB || m.byte_order == WordOrder::DCBA)
                && m.word_count < 2
            {
                return Err(Box::new(DriverError::mapping(format!(
                    "Mapping '{}' uses byte_order {:?} which requires word_count >= 2",
                    m.tag_id, m.byte_order
                ))));
            }
        }

        let mut by_area: std::collections::HashMap<u8, Vec<&crate::mapping::TagMapping>> =
            std::collections::HashMap::new();
        for m in &self.config.mappings {
            by_area.entry(m.area).or_default().push(m);
        }
        for (_area, mut vecm) in by_area.into_iter() {
            vecm.sort_by_key(|m| m.address);
            for w in 0..vecm.len().saturating_sub(1) {
                let cur = vecm[w];
                let next = vecm[w + 1];
                let cur_end = cur
                    .address
                    .saturating_add(cur.word_count as u32)
                    .saturating_sub(1);
                if next.address <= cur_end {
                    return Err(Box::new(DriverError::mapping(format!(
                        "Overlapping FINS ranges for area 0x{:02X}: '{}'[{}..{}] overlaps '{}'[{}..{}]",
                        _area,
                        cur.tag_id,
                        cur.address,
                        cur_end,
                        next.tag_id,
                        next.address,
                        next.address.saturating_add(next.word_count as u32).saturating_sub(1)
                    ))));
                }
            }
        }
        Ok(())
    }

    async fn read_cycle(&self) -> Result<(), driver_common::DynDriverError> {
        // Delegate to internal implementation and convert the concrete driver error
        // into the boxed `DynDriverError` used by the runtime abstraction.
        self.run_read_cycle_impl()
            .await
            .map_err(|e| Box::new(e) as driver_common::DynDriverError)
    }

    async fn submit_write(
        &self,
        tag_id: &str,
        value: core_model::TagValue,
    ) -> Result<(), driver_common::DynDriverError> {
        // Create a oneshot reply channel so the caller can wait for confirmation.
        use tokio::sync::oneshot;
        use tokio::time::{timeout, Duration};

        let (tx, rx) = oneshot::channel::<Result<(), String>>();
        let req = WriteRequest {
            tag_id: tag_id.to_string(),
            value,
            reply: Some(tx),
        };

        // Enqueue the write request in the driver's write queue.
        self.write_sender()
            .send(req)
            .await
            .map_err(|e| Box::new(e) as driver_common::DynDriverError)?;

        // Wait for reply (bounded).
        match timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(msg))) => Err(Box::new(std::io::Error::other(msg))),
            Ok(Err(_recv_err)) => Err(Box::new(std::io::Error::other("reply channel closed"))),
            Err(_) => Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "write confirmation timed out",
            ))),
        }
    }

    async fn health(&self) -> Result<Option<serde_json::Value>, driver_common::DynDriverError> {
        // The FINS driver emits health events via a configured sender; there is no
        // synchronous snapshot available at the moment, so return `None`.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::TagValue;
    use std::net::SocketAddr;

    /// #feature DRV-FINS
    #[tokio::test]
    async fn build_and_wrap_frame() {
        let payload = vec![1, 2, 3];
        let wrapped = FinsDriver::wrap_fins_tcp(&payload);
        assert!(wrapped.starts_with(b"FINS"));
    }

    /// #feature DRV-FINS
    #[tokio::test]
    async fn write_request_queue() {
        let cfg = FinsConfig {
            name: "t".into(),
            endpoint: SocketAddr::from(([127, 0, 0, 1], 9600)),
            cycle_ms: 100,
            keepalive_secs: 30,
            max_backoff_secs: 30,
            mappings: vec![],
            max_words_per_request: 960,
        };
        let defs: Vec<core_model::TagDefinition> = Vec::new();
        let registry =
            Arc::new(core_model::TagRegistry::from_definitions(&defs).expect("build registry"));
        let drv = FinsDriver::new(cfg, registry);
        let tx = drv.write_sender();
        // Use an explicit `String` here to avoid inference issues with `Into<String>` in tests.
        let req = WriteRequest::new(String::from("a"), TagValue::UInt16(1));
        let _ = tx.try_send(req);
    }

    /// #feature DRV-FINS
    #[tokio::test]
    async fn validate_and_split_read_groups() {
        // Simple instantiation test to ensure precomputation runs.
        let cfg = FinsConfig {
            name: "t2".into(),
            endpoint: SocketAddr::from(([127, 0, 0, 1], 9600)),
            cycle_ms: 100,
            keepalive_secs: 30,
            max_backoff_secs: 30,
            mappings: vec![TagMapping::new(
                Arc::from("t1"),
                0x82,
                100,
                0,
                2,
                true,
                WordOrder::ABCD,
                TagDataType::UInt16,
            )],
            max_words_per_request: 960,
        };
        let defs: Vec<core_model::TagDefinition> = vec![];
        let registry =
            Arc::new(core_model::TagRegistry::from_definitions(&defs).expect("build registry"));
        let drv = FinsDriver::new(cfg, registry);
        let _ = drv;
    }
}
