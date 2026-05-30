# Architectural Decision Records

All Architectural Decisions and their rationale will be documented in this file.

The format is based on the [Y-Statement](https://adr.github.io/adr-templates/#y-statement).

## Tech Stack

### Rust Language

In the context of building a reliable, high-performance OPC UA server,
facing choices like C/C++ or Node.js,
we decided to implement the system in Rust
to achieve strong memory safety and modern async concurrency,
accepting Rust's steeper learning curve and smaller ecosystem.

### Non-Rust OPC UA Library

In the context of building a production-grade OPC UA address space and services,
facing the immaturity of pure-Rust OPC UA libraries,
we decided to use the `open62541` C library via a safe Rust wrapper
and neglected pure-Rust implementations such as locka99/opcua,
to achieve high performance and full protocol compliance,
accepting a dependency on a native C toolchain and the risks associated with FFI boundaries,
because `open62541` is a well-tested, certified OPC UA implementation covering most of the specification (encryption, events, custom data types, etc.).

### Centralized Error Handling

In the context of debugging complex failures across multiple layers (Drivers -> Core -> Server),
facing obscure error messages and inconsistent error reporting,
we decided to use a structured error hierarchy (via `thiserror`) rooted in `CoreError`,
to achieve consistent logging and clear diagnostics for the end-user,
accepting the overhead of manual error conversion between layers.

## OPC UA

### Decoupled Registry Architecture

In the context of supporting multiple industrial protocols (Modbus, FINS, etc.) while providing a unified OPC UA interface,
facing tight coupling and complexity in the OPC UA server implementation,
we decided to use a central, protocol-agnostic `TagRegistry` and `TagStore` in a dedicated `core-model` crate,
to achieve separation of concerns and high testability,
accepting the overhead of maintaining a separate internal representation of the data.

### Lock-Free Per-Tag Reads with ArcSwap

In the context of high-frequency data access by multiple OPC UA clients and internal diagnostics,
facing contention and latency when using traditional `RwLock` or `Mutex` for frequently updated tag values,
we decided to use `ArcSwap` for individual tag snapshots within the `TagStore`,
to achieve lock-free reads and high throughput for the OPC UA server,
accepting slightly increased complexity in the storage layer and the requirement for atomic pointers.

### Data Acquisition via Polling

In the context of maintaining up-to-date PLC data,
facing protocols with no push model,
we decided to poll each PLC at configurable intervals,
to achieve continuous data updates and low-latency reads,
accepting constant network traffic and occasional redundant queries.

### Asynchronous Write Pipeline via Channels

In the context of forwarding write requests from OPC UA clients to slow PLC devices,
facing blocking the OPC UA server's execution threads or providing poor responsive times for other clients,
we decided to use an asynchronous write pipeline where the OPC UA server enqueues requests into driver-specific `mpsc` channels,
to achieve high responsiveness and decoupling of network latencies,
accepting the need for managing write confirmation via oneshot reply channels.

### Write Confirmation Modes

In the context of acknowledging OPC UA client writes back through protocol drivers,
facing the tradeoff between low-latency responses and end-to-end reliability,
we decided to support two write acknowledgement modes — `QueuedAck` (accept as soon as the request
is enqueued to the driver) and `ConfirmedAck` (wait for the driver to confirm the write reached the PLC),
to achieve deployment flexibility: `ConfirmedAck` for production safety and `QueuedAck` for
scenarios where write latency is more critical than confirmation,
accepting the need for oneshot reply channels and configurable timeouts in the `ConfirmedAck` path.

### Sync-to-Async Write Bridge via Dedicated Thread

In the context of integrating with the `open62541` C library,
facing its synchronous `DataSource::write` callback that cannot directly await async Rust futures,
we decided to spawn a dedicated `std::thread` ("opcua-write-bridge") that receives writes via
a blocking `mpsc` channel, blocks on the Tokio runtime handle to await the async `WriteHandler`,
and sends the result back through the same channel,
to achieve a clean separation between the synchronous FFI boundary and the fully async write path,
accepting the overhead of a long-lived OS thread and the need to catch panics at the thread boundary.

### Rich Quality Semantics

In the context of diagnosing communication issues in industrial networks,
facing simple boolean "good/bad" status being insufficient for troubleshooting,
we decided to adopt a rich `TagQuality` enum (Good, Stale, CommLost, Initializing, etc.) throughout the runtime,
to achieve detailed observability and better integration with OPC UA status codes,
accepting the need to map these richer states to legacy stack-specific qualities where necessary.

### Explicit PLC Identity in TagDefinitions

In the context of building the OPC UA browse hierarchy,
facing brittle PLC grouping derived from parsing tag IDs with `.` or `:` delimiters,
we decided to add an explicit `plc_name` field to `TagDefinition` so the OPC UA server can group tags by PLC without string-parsing conventions,
to achieve a deterministic, configuration-driven browse structure,
accepting that every TagDefinition must now carry a `plc_name`,
because tag IDs are identifiers, not hierarchical paths, and should not encode topology.

### Arc-Based Write Routing Table

In the context of handling OPC UA write requests at scale,
facing unnecessary cloning of the entire routing HashMap on every write,
we decided to wrap the `RuntimeWriteHandler` routing table in `Arc<HashMap<...>>` so the `handle_write` future captures a cheap reference clone,
to achieve O(1) overhead per write instead of O(n) where n is the number of routes,
accepting the write-path mutation now uses `Arc::make_mut` for clone-on-write semantics,
because the routing table is populated once at startup and rarely modified afterward.

## PLCs

### PLC Protocol Drivers

In the context of interfacing with diverse PLCs,
facing distinct protocols (Omron FINS vs Modbus/TCP),
we decided to implement dedicated Rust driver modules for each protocol
and neglected a single generic driver,
to achieve modularity and full control over protocol specifics,
accepting some code duplication and complexity,
because each protocol has unique addressing and timing requirements.

### Protocol Driver Abstraction

In the context of extending the project to support new PLC protocols,
facing code duplication and inconsistent behavior across drivers,
we decided to define a `ProtocolDriver` trait that abstracts polling, writing, and health reporting,
to achieve high extensibility and consistent runtime management,
accepting the abstraction cost and the need for trait objects in some parts of the runtime.

### Unified Byte-Order Handling

In the context of mapping multi-byte data types (Float, Int32, Double) across various PLC architectures,
facing inconsistent endianness and word-swapping conventions (e.g., Big-Endian vs. "Mid-Endian"),
we decided to centralize byte-reordering logic in the `core-model` crate using a `WordOrder` abstraction,
to achieve consistent data decoding across all protocol drivers and reduce implementation errors,
accepting the need for drivers to perform an extra transformation step before decoding.

### Test-Driven Driver Implementation

In the context of ensuring reliability for critical industrial equipment,
facing regressions and edge cases in protocol parsing and byte-ordering,
we decided to enforce unit testing for all drivers using mocks or local loopback interfaces,
to achieve high reliability and rapid feedback during development,
accepting the additional effort required to maintain comprehensive test suites.

### Defensive Driver Validation

In the context of initializing protocol drivers with complex memory mappings,
facing the risk of silent data corruption or PLC exceptions due to overlapping addresses or type mismatches,
we decided to implement mandatory pre-flight validation in every driver's `validate()` method,
to achieve "fail-fast" behavior and ensure the runtime only starts with a logically sound configuration,
accepting slightly longer startup times and more rigorous configuration requirements.

### Explicit Per-PLC Modbus Unit ID

In the context of deploying to mixed Modbus production environments with multiple slaves behind gateways,
facing the risk of addressing the wrong device when unit_id is hardcoded to 1,
we decided to make the Modbus unit/slave id an explicit `Option<u8>` field in `PlcConfig` defaulting to `None`,
to achieve deterministic device addressing in multi-slave environments while keeping FINS configurations clean,
accepting that Modbus integrators must explicitly set `unit_id` per PLC and that the runtime falls back to 1 when omitted,
because `Slave::tcp_device()` is always 255 and the correct unit id depends on the physical device addressing scheme.

### Explicit FINS Memory-Area Rules

In the context of mapping tags to Omron FINS memory areas,
facing address-string conventions that obscure the actual memory-area code,
we decided to support an explicit `area` field in `TagConfig` alongside the existing address-string inference,
to achieve full control over FINS memory-area selection (D, CIO, W, H, etc.),
accepting that users must learn the memory-area codes when using explicit configuration,
because address-string conventions vary across PLC programs and do not cover all FINS memory areas.

## Deployment and Runtime

### Unified Configuration Schema

In the context of managing multiple PLC drivers and server settings in a single deployment,
facing fragmented and inconsistent configuration across different modules,
we decided to use a unified `RuntimeConfig` schema (serialized as TOML or YAML),
to achieve ease of deployment and centralized validation,
accepting that all drivers must adhere to a shared configuration structure even if they have unique parameters.

### Static Configuration over Dynamic Discovery

In the context of defining the OPC UA address space and PLC tag lists,
facing the choice between scanning PLCs for available tags vs. using a predefined list,
we decided to favor a static, configuration-driven approach,
to achieve maximum predictability and stability for industrial SCADA/HMI clients,
accepting that changes to the PLC program require manual updates to the bridge's configuration.

### Deterministic Mapping of NodeIds to PLC Addresses

In the context of bridging the OPC UA address space with physical PLC memory,
facing complex and error-prone address translation logic,
we decided to use a declarative mapping in the configuration that directly links OPC UA `String` NodeIds to driver-specific registers and bit-offsets,
to achieve predictability and simplicity in configuration,
accepting that structural changes in the PLC program may require manual configuration updates.

### Graceful Shutdown Coordination

In the context of terminating a multi-threaded runtime with active network connections,
facing the concern of "zombie" TCP sessions on PLCs or abrupt disconnection of OPC UA clients,
we decided to use a hierarchical shutdown mechanism via `tokio::sync::watch` and `ServerHandle`,
to achieve clean resource release and predictable termination sequences,
accepting the complexity of propagating shutdown signals across all background tasks.

## Security

### OPC UA Security (TLS/Certificates)

In the context of deploying on an industrial network,
facing potential eavesdropping or tampering,
we decided to enable OPC UA secure channels with encryption and certificate-based authentication,
to achieve confidentiality and integrity of OPC UA communications,
accepting the complexity of certificate management and user provisioning.

### PLC Protocol Security

In the context of the inherently insecure PLC protocols,
facing their lack of encryption,
we decided to rely on network isolation and perimeter security,
to achieve reasonable protection,
accepting that base protocols remain in plaintext.

## Observability

### Structured Logging with Contextual Metadata

In the context of troubleshooting issues in deployments with dozens of PLCs and thousands of tags,
facing the difficulty of filtering "needle-in-a-haystack" errors in flat log files,
we decided to use the `tracing` crate with structured fields (e.g., `plc_name`, `tag_id`),
to achieve high observability and rapid fault isolation,
accepting the overhead of maintaining consistent field naming conventions across the codebase.

## Testing

### Layered Testing with In-Process Protocol Servers

In the context of validating protocol driver correctness for industrial deployments,
facing the choice between mocking at the trait boundary versus exercising real TCP connections,
we decided to use real protocol servers spawned in-process like `tokio-modbus` in server mode for driver integration tests
and neglected purely mocked/dummy `ProtocolDriver` implementations for protocol-level validation,
to achieve genuine bytes-on-the-wire confidence while keeping tests fast and CI-friendly,
accepting that protocol servers must be available as Rust libraries and that some protocols lack suitable in-process implementations.

### Containerized End-to-End Tests as Pre-Release Gate

In the context of validating the full project pipeline (config → runtime → drivers → OPC UA server → client),
facing the prohibitive cost and complexity of buying physical PLCs and SCADA licenses,
we decided to use Docker Compose with containerized protocol emulators and a scriptable OPC UA client
and neglected running these end-to-end tests in CI,
to achieve deployment-like integration confidence while keeping CI responsive,
accepting that these tests must be executed manually before each release and that container-based emulators are approximations of real hardware.

### Network Fault Injection for Chaos Testing

In the context of verifying the project's resilience to real-world network conditions,
facing the risk of undetected reconnect bugs, timeout misconfigurations, and silent data staleness,
we decided to use `tc netem` for kernel-level traffic shaping within Docker networks
and neglected application-level fault injection libraries such as `toxiproxy`,
to achieve fine-grained control over packet loss, latency, and connection drops without additional service dependencies,
accepting that `tc netem` requires privileged containers and is Linux-specific.

### Deferred FINS Protocol Integration Testing

In the context of testing the Omron FINS driver with real protocol frames,
facing the absence of any trusted, open-source, container-friendly FINS emulator,
we decided to defer true protocol-level FINS integration testing
and neglected writing a custom FINS memory server or using proprietary Windows-only simulators,
to avoid maintaining a protocol reference implementation and introducing non-containerizable test dependencies,
accepting that FINS driver confidence relies solely on unit tests for frame construction, write queuing, and read-group splitting until a suitable emulator emerges.

### Feature-Tagged Test Coverage

In the context of maintaining a high confidence in which features are supported by the library,
facing the inability to verify which features are genuinely supported versus wishful thinking,
we decided to assign a short feature ID to each feature and tag every test function with a `/// #feature <ID>` doc comment,
to achieve a direct, bidirectional, grep-able link between features and their tests,
accepting that a feature with zero matching greps is a gap to be closed.

### Consistent Test Location Convention

In the context of maintaining a multi-crate workspace with unit, integration, and end-to-end tests,
facing tests scattered across separate test modules and integration test directories with no clear rule,
we decided to enforce a uniform convention across all crates:
unit tests live inline as `#[cfg(test)] mod tests { ... }` blocks in the same `src/*.rs` file as the code they test;
integration tests live in `tests/*.rs` at the crate root and access only the public API;
end-to-end tests live in the workspace `e2e-tests/` directory using Docker Compose,
to achieve location predictability,
accepting that integration tests requiring `pub(crate)` internals must either have those internals made public or be moved to inline unit tests alongside the types they exercise.
