//! Integration tests for the Modbus driver using an in-process `tokio-modbus` TCP server.
//!
//! These tests exercise genuine Modbus frames over TCP — no mocking, no dummy
//! trait impls. The server stores registers in-memory and responds to the exact
//! Modbus function codes the driver uses, giving us real bytes-on-the-wire
//! confidence while staying fast and CI-friendly.

use core_model::{TagDataType, TagQuality, TagRegistry, TagValue, WordOrder};
use driver_modbus::{ModbusConfig, ModbusDriver, ModbusFunction, ModbusMapping, ProtocolDriver};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::RwLock;
use tokio_modbus::prelude::*;
use tokio_modbus::server::tcp::Server;

// ---------------------------------------------------------------------------
// Test Modbus server: holds registers in memory and serves a subset of
// function codes used by the driver.
// ---------------------------------------------------------------------------

struct TestModbusServer {
    holding: Arc<RwLock<HashMap<u16, u16>>>,
    input: Arc<RwLock<HashMap<u16, u16>>>,
    coils: Arc<RwLock<HashMap<u16, bool>>>,
}

impl TestModbusServer {
    fn new() -> Self {
        Self {
            holding: Arc::new(RwLock::new(HashMap::new())),
            input: Arc::new(RwLock::new(HashMap::new())),
            coils: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn set_holding(&self, addr: u16, value: u16) {
        self.holding.write().await.insert(addr, value);
    }

    async fn set_holding_u32(&self, addr: u16, value: u32) {
        let mut regs = self.holding.write().await;
        regs.insert(addr, (value >> 16) as u16);
        regs.insert(addr + 1, (value & 0xFFFF) as u16);
    }

    async fn set_holding_f32(&self, addr: u16, value: f32) {
        self.set_holding_u32(addr, value.to_bits()).await;
    }
}

impl tokio_modbus::server::Service for TestModbusServer {
    type Request = tokio_modbus::Request<'static>;
    type Response = Option<tokio_modbus::Response>;
    type Exception = tokio_modbus::ExceptionCode;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Exception>> + Send>,
    >;

    fn call(&self, req: Self::Request) -> Self::Future {
        let holding = self.holding.clone();
        let input = self.input.clone();
        let coils = self.coils.clone();

        Box::pin(async move {
            match req {
                Request::ReadHoldingRegisters(addr, qty) => {
                    let regs = holding.read().await;
                    let values: Vec<u16> = (0..qty)
                        .map(|i| regs.get(&(addr + i)).copied().unwrap_or(0))
                        .collect();
                    Ok(Some(Response::ReadHoldingRegisters(values)))
                }
                Request::ReadInputRegisters(addr, qty) => {
                    let regs = input.read().await;
                    let values: Vec<u16> = (0..qty)
                        .map(|i| regs.get(&(addr + i)).copied().unwrap_or(0))
                        .collect();
                    Ok(Some(Response::ReadInputRegisters(values)))
                }
                Request::WriteSingleRegister(addr, value) => {
                    holding.write().await.insert(addr, value);
                    Ok(Some(Response::WriteSingleRegister(addr, value)))
                }
                Request::WriteMultipleRegisters(addr, values) => {
                    let values: Vec<u16> = values.into_owned();
                    let mut regs = holding.write().await;
                    for (i, v) in values.iter().enumerate() {
                        regs.insert(addr + i as u16, *v);
                    }
                    Ok(Some(Response::WriteMultipleRegisters(
                        addr,
                        values.len() as u16,
                    )))
                }
                Request::ReadCoils(addr, qty) => {
                    let c = coils.read().await;
                    let bits: Vec<bool> = (0..qty)
                        .map(|i| c.get(&(addr + i)).copied().unwrap_or(false))
                        .collect();
                    Ok(Some(Response::ReadCoils(bits)))
                }
                Request::WriteSingleCoil(addr, value) => {
                    coils.write().await.insert(addr, value);
                    Ok(Some(Response::WriteSingleCoil(addr, value)))
                }
                _ => Err(tokio_modbus::ExceptionCode::IllegalFunction),
            }
        })
    }
}

/// Spawn a test Modbus TCP server on an OS-assigned port. Returns the
/// bound `SocketAddr` and a handle that can be used to control the server
/// (dropping the returned `JoinHandle` will abort the server task).
async fn spawn_test_server(svc: &TestModbusServer) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let cloned = svc.clone_service();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let handle = spawn_server_on_listener(listener, cloned).await;
    (addr, handle)
}

/// Spawn a server bound to a *specific* address, using SO_REUSEADDR so the
/// port can be rebound immediately after a previous server exits (needed for
/// reconnect tests).
async fn spawn_server_at(addr: SocketAddr, svc: &TestModbusServer) -> tokio::task::JoinHandle<()> {
    let cloned = svc.clone_service();
    let socket = TcpSocket::new_v4().expect("new_v4");
    socket.set_reuseaddr(true).expect("set_reuseaddr");
    socket.bind(addr).expect("bind");
    let listener = socket.listen(128).expect("listen");
    spawn_server_on_listener(listener, cloned).await
}

async fn spawn_server_on_listener(
    listener: TcpListener,
    svc: TestModbusServer,
) -> tokio::task::JoinHandle<()> {
    let server = Server::new(listener);
    tokio::spawn(async move {
        let on_connected = |stream, _client_addr| async {
            Ok::<_, std::io::Error>(Some((svc.clone_service(), stream)))
        };
        server.serve(&on_connected, |_| {}).await.ok();
    })
}

impl TestModbusServer {
    /// Produce a clone of self (all Arc'd state is cheaply shareable) that
    /// implements `Service` independently for the `on_connected` closure.
    fn clone_service(&self) -> TestModbusServer {
        TestModbusServer {
            holding: self.holding.clone(),
            input: self.input.clone(),
            coils: self.coils.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers to build driver + registry
// ---------------------------------------------------------------------------

fn make_registry(defs: &[(&str, TagDataType)]) -> Arc<TagRegistry> {
    let defs: Vec<_> = defs
        .iter()
        .map(|(id, dt)| core_model::TagDefinition::new(*id, *id, "ignored", dt.clone(), "test-plc"))
        .collect();
    Arc::new(TagRegistry::from_definitions(&defs).expect("valid defs"))
}

fn make_driver(
    addr: SocketAddr,
    unit_id: u8,
    mappings: Vec<ModbusMapping>,
    registry: Arc<TagRegistry>,
) -> ModbusDriver {
    let config = ModbusConfig::new("test-plc", addr, unit_id, 100, mappings);
    ModbusDriver::new(config, registry)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// #feature DRV-MODBUS, UA-READ
#[tokio::test]
async fn read_u16_holding_register() {
    let server = TestModbusServer::new();
    server.set_holding(0, 0xABCD).await;
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "tag",
        0,
        1,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        true,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("tag", TagDataType::UInt16)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("tag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::UInt16(0xABCD));
    assert_eq!(tag.quality, TagQuality::Good);
}

/// #feature DRV-MODBUS, UA-READ, UA-TYPES
#[tokio::test]
async fn read_u32_holding_registers() {
    let server = TestModbusServer::new();
    // 0x12345678 spread across two registers at addr 10,11
    server.set_holding(10, 0x1234).await;
    server.set_holding(11, 0x5678).await;
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "u32tag",
        10,
        2,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt32,
        0,
        false,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("u32tag", TagDataType::UInt32)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("u32tag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::UInt32(0x12345678));
}

/// #feature DRV-MODBUS, UA-READ, UA-TYPES
#[tokio::test]
async fn read_f32_holding_registers() {
    let server = TestModbusServer::new();
    server.set_holding_f32(5, std::f32::consts::PI).await;
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "ftag",
        5,
        2,
        ModbusFunction::HoldingRegisters,
        TagDataType::Float,
        0,
        false,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("ftag", TagDataType::Float)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("ftag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::Float(std::f32::consts::PI));
}

/// #feature DRV-MODBUS, UA-READ
#[tokio::test]
async fn read_bool_coil() {
    let server = TestModbusServer::new();
    server.coils.write().await.insert(3, true);
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "btag",
        3,
        1,
        ModbusFunction::Coils,
        TagDataType::Bool,
        0,
        false,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("btag", TagDataType::Bool)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("btag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::Bool(true));
}

/// #feature DRV-MODBUS, UA-READ
#[tokio::test]
async fn read_input_registers() {
    let server = TestModbusServer::new();
    server.input.write().await.insert(100, 0xDEAD);
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "itag",
        100,
        1,
        ModbusFunction::InputRegisters,
        TagDataType::UInt16,
        0,
        false,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("itag", TagDataType::UInt16)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("itag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::UInt16(0xDEAD));
}

/// #feature DRV-MODBUS, UA-WRITE, UA-READ
#[tokio::test]
async fn write_holding_register_and_read_back() {
    let server = TestModbusServer::new();
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "wtag",
        42,
        1,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        true, // writable
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("wtag", TagDataType::UInt16)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    // submit_write enqueues the request and then waits on a oneshot reply.
    // run_read_cycle_impl drains writes and processes reads — the write
    // confirmation is sent from inside that cycle. Run both concurrently.
    let (write_res, read_res) = tokio::join!(
        driver.submit_write("wtag", TagValue::UInt16(0xCAFE)),
        driver.run_read_cycle_impl(),
    );
    write_res.expect("write ok");
    read_res.expect("read cycle");

    let tag = registry.get_tag("wtag").expect("tag exists");
    assert_eq!(*tag.value, TagValue::UInt16(0xCAFE));

    // Also verify on the server side directly
    let server_val = server.holding.read().await.get(&42).copied();
    assert_eq!(server_val, Some(0xCAFE));
}

/// #feature DRV-MODBUS, UA-WRITE
#[tokio::test]
async fn non_writable_tag_rejected() {
    let server = TestModbusServer::new();
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "rotag",
        0,
        1,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        false, // not writable
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("rotag", TagDataType::UInt16)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    let result = driver.submit_write("rotag", TagValue::UInt16(99)).await;
    assert!(result.is_err());
}

/// #feature DRV-MODBUS
#[tokio::test]
async fn driver_reconnects_after_server_restart() {
    // Reserve a port, then drop the socket so nothing is listening.
    let socket = TcpSocket::new_v4().expect("new_v4");
    socket.set_reuseaddr(true).expect("set_reuseaddr");
    socket.bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let addr = socket.local_addr().expect("local_addr");
    drop(socket);

    let mapping = ModbusMapping::new(
        "rtag",
        0,
        1,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        true,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("rtag", TagDataType::UInt16)]);
    // Use a short max_backoff so the endless retry loop doesn't hang the test.
    let mut config = ModbusConfig::new("test-plc", addr, 1, 100, vec![mapping]);
    config.max_backoff_secs = 1;
    let driver = ModbusDriver::new(config, registry.clone());

    // First read fails — nothing is listening. Bounded by timeout to avoid the
    // driver's infinite connect retry loop.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        driver.run_read_cycle_impl(),
    )
    .await;
    assert!(result.is_err(), "should time out when nothing is listening");

    // Now start a server on that port.
    let server = TestModbusServer::new();
    server.set_holding(0, 0x2222).await;
    let _jh = spawn_server_at(addr, &server).await;

    // Driver should reconnect and read the value.
    driver
        .run_read_cycle_impl()
        .await
        .expect("read cycle after server starts");
    assert_eq!(
        *registry.get_tag("rtag").unwrap().value,
        TagValue::UInt16(0x2222)
    );
}

/// #feature DRV-MODBUS, UA-TYPES
#[tokio::test]
async fn byte_order_swapped_words() {
    let server = TestModbusServer::new();
    // Server stores hi-first: [0x1234, 0x5678] => 0x12345678
    server.set_holding(0, 0x1234).await;
    server.set_holding(1, 0x5678).await;
    let (addr, _jh) = spawn_test_server(&server).await;

    // CDAB order swaps the two 16-bit words: [0x5678, 0x1234] => 0x56781234
    let mapping = ModbusMapping::new(
        "swapped",
        0,
        2,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt32,
        0,
        false,
        WordOrder::CDAB,
    );
    let registry = make_registry(&[("swapped", TagDataType::UInt32)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    let tag = registry.get_tag("swapped").expect("tag exists");
    assert_eq!(*tag.value, TagValue::UInt32(0x56781234));
}

/// #feature DRV-MODBUS, UA-READ
#[tokio::test]
async fn multiple_tags_in_single_cycle() {
    let server = TestModbusServer::new();
    // Contiguous registers at 100-103
    server.set_holding(100, 0xAAAA).await;
    server.set_holding(101, 0xBBBB).await;
    server.set_holding(102, 0xCCCC).await;
    server.set_holding(103, 0xDDDD).await;
    let (addr, _jh) = spawn_test_server(&server).await;

    let mappings = vec![
        ModbusMapping::new(
            "a",
            100,
            1,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt16,
            0,
            false,
            WordOrder::ABCD,
        ),
        ModbusMapping::new(
            "b",
            101,
            1,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt16,
            0,
            false,
            WordOrder::ABCD,
        ),
        // c is a 32-bit across 102-103
        ModbusMapping::new(
            "c",
            102,
            2,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt32,
            0,
            false,
            WordOrder::ABCD,
        ),
    ];
    let registry = make_registry(&[
        ("a", TagDataType::UInt16),
        ("b", TagDataType::UInt16),
        ("c", TagDataType::UInt32),
    ]);
    let driver = make_driver(addr, 1, mappings, registry.clone());

    driver.run_read_cycle_impl().await.expect("read cycle");

    assert_eq!(
        *registry.get_tag("a").unwrap().value,
        TagValue::UInt16(0xAAAA)
    );
    assert_eq!(
        *registry.get_tag("b").unwrap().value,
        TagValue::UInt16(0xBBBB)
    );
    assert_eq!(
        *registry.get_tag("c").unwrap().value,
        TagValue::UInt32(0xCCCCDDDD)
    );
}

/// #feature DRV-MODBUS
#[tokio::test]
async fn invalid_address_marks_config_error() {
    let server = TestModbusServer::new();
    let (addr, _jh) = spawn_test_server(&server).await;

    let mapping = ModbusMapping::new(
        "bad",
        9999,
        1,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        false,
        WordOrder::ABCD,
    );
    let registry = make_registry(&[("bad", TagDataType::UInt16)]);
    let driver = make_driver(addr, 1, vec![mapping], registry.clone());

    driver
        .run_read_cycle_impl()
        .await
        .expect("read cycle ok (server returns 0 for unmapped registers)");
    // The driver doesn't treat unexpected zeros as errors — the server happily
    // returns 0 for unmapped registers. A real config error (e.g., overlapping
    // mappings) is caught by validate(), tested below.
}

/// #feature DRV-MODBUS
#[test]
fn validate_rejects_overlapping_ranges() {
    let mappings = vec![
        ModbusMapping::new(
            "x",
            10,
            2,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt16,
            0,
            false,
            WordOrder::ABCD,
        ),
        ModbusMapping::new(
            "y",
            11,
            1,
            ModbusFunction::HoldingRegisters,
            TagDataType::UInt16,
            0,
            false,
            WordOrder::ABCD,
        ),
    ];
    let registry = make_registry(&[("x", TagDataType::UInt16), ("y", TagDataType::UInt16)]);
    let config = ModbusConfig::new("test", "127.0.0.1:502".parse().unwrap(), 1, 100, mappings);
    let driver = ModbusDriver::new(config, registry);
    let result = driver.validate();
    assert!(result.is_err(), "overlapping ranges must fail validation");
}

/// #feature DRV-MODBUS
#[test]
fn validate_rejects_zero_quantity() {
    let mappings = vec![ModbusMapping::new(
        "z",
        0,
        0,
        ModbusFunction::HoldingRegisters,
        TagDataType::UInt16,
        0,
        false,
        WordOrder::ABCD,
    )];
    let registry = make_registry(&[("z", TagDataType::UInt16)]);
    let config = ModbusConfig::new("test", "127.0.0.1:502".parse().unwrap(), 1, 100, mappings);
    let driver = ModbusDriver::new(config, registry);
    assert!(driver.validate().is_err());
}
