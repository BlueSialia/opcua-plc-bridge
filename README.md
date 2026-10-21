# OPC UA PLC Bridge

[![Hippocratic License HL3-BOD-CL-ECO-FFD-LAW-MEDIA-MIL-MY-SV-TAL-USTA-XUAR](https://img.shields.io/static/v1?label=Hippocratic%20License&message=HL3-BOD-CL-ECO-FFD-LAW-MEDIA-MIL-MY-SV-TAL-USTA-XUAR&labelColor=5e2751&color=bc8c3d)](https://firstdonoharm.dev/version/3/0/bod-cl-eco-ffd-law-media-mil-my-sv-tal-usta-xuar.html)

---

A Rust repository for an OPC UA bridge for PLCs. It provides a protocol-agnostic internal tag model and protocol drivers. The repository is organized to enforce strong separation between:

- OPC UA server (exposes nodes, subscriptions, read/write from clients and health status)
- Internal core model (unified, thread-safe representation of PLC variables/tags; has zero OPC UA or protocol knowledge)
- Protocol drivers (translate PLC-specific addressing into the core model)
- Runtime glue (ties everything together, loads config, spawns drivers, starts OPC UA server, handles shutdown)

For more details on the project structure and design, see the [ADR](ADR.md) document.

## Feature Coverage

<details>
<summary>Feature checklist</summary>

Test functions are tagged with `#feature <ID>`.

> `[x]` tested &nbsp; `[ ]` supported, no test yet &nbsp; `[-]` not supported

### Protocol & Transport

- [x] `UA-TCP` — UA-TCP Binary Protocol (`opc.tcp`)
- [x] `UA-SECURE-CONV` — UA-SecureConversation (encrypted channels)
- [-] `UA-HTTPS` — HTTPS/SOAP Web Services transport
- [-] `UA-REV-CONN` — Reverse Connect
- [-] `UA-PUBSUB` — PubSub (MQTT / AMQP / UDP)

### Address Space

- [x] `UA-OBJ` — Object Nodes
- [x] `UA-VAR` — Variable Nodes
- [x] `UA-REF` — Reference Types (Organizes)
- [x] `UA-BROWSE` — Hierarchical Browse structure
- [x] `UA-NS` — Custom Namespaces
- [x] `UA-NODEID` — String NodeIds
- [-] `UA-NODEID-NUM` — Numeric / Opaque / GUID NodeIds
- [-] `UA-VIEW` — View Nodes

### Data Access

- [x] `UA-READ` — Read service
- [x] `UA-WRITE` — Write service
- [x] `UA-TYPES` — Built-in Data Types (Bool, Int16, UInt16, Int32, UInt32, Int64, UInt64, Float, Double, String, DateTime, ByteString)
- [-] `UA-CUSTOM-TYPES` — Custom / Structured Data Types
- [x] `UA-ACCESS` — Access Levels (CurrentRead, CurrentWrite)
- [x] `UA-QUALITY` — Value Quality / StatusCode mapping
- [x] `UA-TS` — Source / Server Timestamps

### Services

- [x] `UA-SESSION` — Session Service
- [-] `UA-METHOD` — Method Service
- [-] `UA-QUERY` — Query Service
- [-] `UA-HISTORY` — History Read / Update Service
- [-] `UA-DISCOVERY` — Discovery Service (FindServers, GetEndpoints, RegisterServer)
- [-] `UA-NODEMGMT` — Node Management Service (Add / Delete Nodes at runtime)

### Security

- [x] `UA-SEC-NONE` — None Security Mode
- [x] `UA-SEC-SIGN` — Sign Security Mode
- [x] `UA-SEC-ENCRYPT` — SignAndEncrypt Security Mode
- [x] `UA-SEC-POLICIES` — Security Policies (Basic128Rsa15, Basic256, Basic256Sha256)
- [x] `UA-AUTH-ANON` — Anonymous Authentication
- [ ] `UA-AUTH-PASS` — Username / Password Authentication
- [-] `UA-AUTH-CERT` — X.509 Certificate Authentication
- [ ] `UA-SEC-TRUST` — Trust Store
- [ ] `UA-SEC-REJECT` — Rejected Certificate Store

### Subscriptions & Events

- [x] `UA-SUBS` — Subscriptions
- [x] `UA-MONITOR` — Monitored Items
- [x] `UA-PUBLISH` — Publish / Republish
- [-] `UA-EVENT-FILTER` — Event Filters
- [-] `UA-ALARMS` — Alarms & Conditions (A&C)
- [-] `UA-AUDIT` — Audit Events

### Advanced Features

- [-] `UA-HDA` — Historical Data Access (HDA)
- [-] `UA-AGGREGATES` — Aggregates (Min, Max, Avg, etc.)
- [-] `UA-PROGRAMS` — Methods / Programs
- [-] `UA-FILE` — File Transfer
- [-] `UA-COMPANION` — Companion Specifications
- [-] `UA-SEMANTIC` — Semantic Annotations
- [-] `UA-REDUNDANCY` — Redundancy (Server / Client failover)
- [-] `UA-DIAG` — Server Diagnostics & Status Variables

### Protocol Drivers

- [x] `DRV-MODBUS` — Modbus TCP
- [x] `DRV-FINS` — Omron FINS
- [-] `DRV-S7` — Siemens S7 (S7comm)
- [-] `DRV-EIP` — EtherNet/IP (CIP)
- [-] `DRV-PROFINET` — PROFINET
- [-] `DRV-BACNET` — BACnet
- [-] `DRV-DNP3` — DNP3
- [-] `DRV-IEC104` — IEC 60870-5-104
- [-] `DRV-MELSEC` — MELSEC (Mitsubishi)
- [-] `DRV-SPARKPLUG` — MQTT Sparkplug B
- [-] `DRV-DF1` — DF1 / PCCC (Allen-Bradley)
- [-] `DRV-HOSTLINK` — HostLink (Omron)

</details>

## Getting Started

These instructions will get you a copy of the project up and running on your local machine for development and testing purposes. See [Install](#install) for notes on how to deploy the project on a live system.

### Prerequisites

- Rust 1.99.1 ([rustup](https://rustup.rs) or [mise](https://mise.jdx.dev))
- Toolchain: clang, cmake, gcc, libclang-dev, pkg-config and python3
- Docker (required for end-to-end tests and container deployment)
- Nix with flakes enabled (optional, for the reproducible development shell)

### Setup

Clone the repository, enable the local Git hooks, and build:

```sh
git clone git@github.com:BlueSialia/opcua-plc-bridge.git
cd opcua-plc-bridge
./setup-hooks.sh
cargo build
```

In the repository settings, enable **Settings → Actions → General → Workflow permissions → “Allow GitHub Actions to create and approve pull requests”** so the release workflow can open its automated pull request and dispatch its checks.

With Nix and direnv:

```sh
direnv allow
```

Otherwise, enter the development shell with:

```sh
nix develop
```

### Running

Run the bridge with an example configuration:

```sh
cargo run -p runtime -- examples/config.toml
```

Or run the built binary directly:

```sh
./target/release/opcua-plc-bridge path/to/config.toml
```

## Running the Tests

### Unit Tests

You can run the unit tests with:

```sh
cargo test --workspace --lib --bins
```

Tests are located in each source file (inline `#[cfg(test)]` modules).

### Integration Tests

You can run the integration tests with:

```sh
cargo test --workspace --test integration
```

Integration tests live in their `tests/` directories.

### E2E Tests

You can run the E2E tests with:

```sh
docker compose -f e2e-tests/docker-compose.yml --profile [node/python] up --abort-on-container-exit --attach opcua-client-[node/python]
```

You can only run the E2E tests for the node or python profile, not both because each starts a different OPC UA client that runs tests and they will interfere with each other.

### Linting

On top of it you should also periodically run the linter with:

```sh
cargo clippy --all-targets --all-features -- -D warnings
```

And verify that the documentation builds without warnings:

```sh
RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps
```

### Coding Style

In order to maintain a similar style over the codebase we use:

```sh
cargo fmt --all -- --check
```

## Install

### Binary

Download the latest binary from the [GitHub releases page](https://github.com/BlueSialia/opcua-plc-bridge/releases). Place it on the target machine alongside a TOML or YAML configuration file (see `examples/config.toml` for an annotated template), then run:

```sh
chmod +x opcua-plc-bridge
./opcua-plc-bridge path/to/config.toml
```

For production deployments behind a reverse proxy or load balancer, ensure the OPC UA TCP port (default 4840) is exposed.

To update, download the new binary, replace the old one, and restart the process.

### Container

Pull the image from GitHub Container Registry and run it with your config file mounted:

```sh
docker pull ghcr.io/bluesialia/opcua-plc-bridge:latest

docker run -d \
  --name opcua-plc-bridge \
  -v /path/to/config.toml:/config.toml:ro \
  -p 4840:4840 \
  ghcr.io/bluesialia/opcua-plc-bridge:latest \
  /config.toml
```

To update, pull the new image and recreate the container:

```sh
docker pull ghcr.io/bluesialia/opcua-plc-bridge:latest
docker stop opcua-plc-bridge
docker rm opcua-plc-bridge
docker run -d \
  --name opcua-plc-bridge \
  -v /path/to/config.toml:/config.toml:ro \
  -p 4840:4840 \
  ghcr.io/bluesialia/opcua-plc-bridge:latest \
  /config.toml
```

Alternatively, pin to a specific version (`v1.2.3`) instead of `latest` for reproducible deployments.

## Built With

| Dependency | Purpose |
|---|---|
| [Rust](https://rust-lang.org/) | Application language |
| [Cargo](https://doc.rust-lang.org/cargo/) | The Rust package manager |
| [open62541](https://github.com/HMIProject/open62541) | OPC UA implementation used through Rust bindings |

## Automations

Local Git hooks enforce Conventional Commits. CI runs formatting, Clippy, tests, documentation, unused-dependency, and security checks. Binary size reporting and release publishing are also automated.

See [AUTOMATIONS.md](AUTOMATIONS.md) for more information and details.

## Contributing

Please read [CONTRIBUTING.md](https://gist.github.com/PurpleBooth/b24679402957c63ec426) for details on our code of conduct, and the process for submitting pull requests to us.

## Versioning

We use [SemVer](http://semver.org/) for versioning. For the versions available, see the [tags on this repository](https://github.com/BlueSialia/opcua-plc-bridge/tags).

## Authors

* **Jorge Domínguez** - *Initial work* - [BlueSialia](https://github.com/BlueSialia)

See also the list of [contributors](https://github.com/BlueSialia/opcua-plc-bridge/contributors) who participated in this project.

## License

This project is licensed under the Hippocratic License 3.0 - see the [LICENSE.md](LICENSE.md) file for details.

## Acknowledgments

None yet.
