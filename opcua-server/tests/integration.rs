//! Integration tests for the `opcua-server` crate.
//!
//! These tests exercise the public API — server builder construction, security
//! policy wiring, and encrypted client↔server communication — without access to
//! `pub(crate)` internals.
//!
//! Unit tests that depend on internal types (e.g. `BridgeWrite`, `TagDataSource`)
//! live in `src/native.rs` as inline `#[cfg(test)]` modules.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use opcua_server::SecurityPolicy;
use open62541::ua;

/// Build an OPC UA server with security policies enabled using a
/// programmatically-generated self-signed certificate. This verifies
/// the end-to-end pipeline: certificate generation (`create_certificate`),
/// builder construction with `default_with_security_policies`, and a
/// functional `Server` object.
/// #feature UA-SEC-POLICIES, UA-SEC-ENCRYPT, UA-SEC-SIGN, UA-SECURE-CONV
#[test]
fn server_builder_accepts_security_policies() {
    let subject = ua::Array::from_slice(&[
        ua::String::new("C=DE").unwrap(),
        ua::String::new("O=TestOrg").unwrap(),
        ua::String::new("CN=TestServer@localhost").unwrap(),
    ]);
    let subject_alt_name = ua::Array::from_slice(&[
        ua::String::new("DNS:localhost").unwrap(),
        ua::String::new("URI:urn:test:opcua:server").unwrap(),
    ]);

    let (certificate, private_key) = open62541::create_certificate(
        &subject,
        &subject_alt_name,
        &ua::CertificateFormat::PEM,
        None,
    )
    .expect("create_certificate should succeed");

    let builder =
        open62541::ServerBuilder::default_with_security_policies(4841, &certificate, &private_key)
            .expect("default_with_security_policies should succeed");

    let builder = builder
        .server_urls(&["opc.tcp://127.0.0.1:4841"])
        .accept_all();

    let (server, _runner) = builder.build();

    // The server should be functional: we can add a namespace.
    let ns = server.add_namespace("urn:test:security:ns");
    assert!(ns >= 2, "namespace index {ns} should be at least 2");
}

/// End-to-end encrypted channel test: starts a server with SignAndEncrypt
/// security using a self-signed certificate, connects an OPC UA client
/// over the encrypted channel, reads a variable node, and verifies the
/// returned value matches what the server published.
/// #feature UA-SECURE-CONV, UA-SEC-ENCRYPT, UA-SEC-SIGN, UA-SEC-POLICIES
#[tokio::test(flavor = "multi_thread")]
async fn encrypted_client_reads_from_server() {
    // OPC UA NS0 node ID constants (from the OPC UA specification).
    const NS0_OBJECTSFOLDER: u32 = 85;
    const NS0_ORGANIZES: u32 = 35;
    const NS0_BASEDATAVARIABLETYPE: u32 = 63;
    const NS0_UINT16: u32 = 5;

    // Generate a self-signed certificate (DER format to exercise that path).
    let subject = ua::Array::from_slice(&[
        ua::String::new("C=DE").unwrap(),
        ua::String::new("O=E2ETest").unwrap(),
        ua::String::new("CN=E2EServer@localhost").unwrap(),
    ]);
    let subject_alt_name = ua::Array::from_slice(&[
        ua::String::new("DNS:localhost").unwrap(),
        ua::String::new("URI:urn:e2e:secure:server").unwrap(),
    ]);
    let (certificate, private_key) = open62541::create_certificate(
        &subject,
        &subject_alt_name,
        &ua::CertificateFormat::DER,
        None,
    )
    .expect("create_certificate should succeed");

    // Use a non-default port to avoid conflicts with other tests.
    let port = 4846u16;
    let endpoint = format!("opc.tcp://127.0.0.1:{}", port);

    // Build server with security policies enabled.
    let (server, runner) =
        open62541::ServerBuilder::default_with_security_policies(port, &certificate, &private_key)
            .expect("default_with_security_policies should succeed")
            .server_urls(&[endpoint.as_str()])
            .accept_all()
            .build();

    // Add a namespace and a variable node holding a known value.
    let ns = server.add_namespace("urn:e2e:secure");
    let node_id = server
        .add_variable_node(open62541::VariableNode {
            requested_new_node_id: Some(ua::NodeId::string(ns, "test.var")),
            parent_node_id: ua::NodeId::ns0(NS0_OBJECTSFOLDER),
            reference_type_id: ua::NodeId::ns0(NS0_ORGANIZES),
            browse_name: ua::QualifiedName::new(ns, "E2ETestVar"),
            type_definition: ua::NodeId::ns0(NS0_BASEDATAVARIABLETYPE),
            attributes: ua::VariableAttributes::default()
                .with_data_type(&ua::NodeId::ns0(NS0_UINT16))
                .with_access_level(&ua::AccessLevelType::NONE.with_current_read(true)),
        })
        .expect("add variable node");
    server
        .write_value(&node_id, &ua::Variant::scalar(ua::UInt16::new(4660)))
        .expect("write initial value");

    // Start the server runner in a background thread.
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let sf = shutdown_flag.clone();
    let server_thread = std::thread::spawn(move || {
        let _ = runner.run_until_cancelled(move || sf.load(Ordering::Relaxed));
    });

    // Give the server a moment to bind and start listening.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Connect an encrypted client and read the node.
    let client = open62541::ClientBuilder::default_encryption(&certificate, &private_key)
        .expect("client default_encryption should succeed")
        .accept_all()
        .security_mode(ua::MessageSecurityMode::SIGNANDENCRYPT)
        .security_policy_uri(ua::String::new(SecurityPolicy::Basic256Sha256.uri()).unwrap())
        .connect(&endpoint)
        .expect("encrypted client connect should succeed");

    let async_client = client.into_async();

    let data_value = async_client
        .read_value(&node_id)
        .await
        .expect("read over encrypted channel should succeed");

    let scalar: ua::UInt16 = data_value
        .value()
        .expect("DataValue should contain a variant")
        .to_scalar()
        .expect("variant should be UInt16");
    assert_eq!(
        scalar.value(),
        4660,
        "encrypted read should return the correct value (0x1234)"
    );

    // Clean shutdown: disconnect the client first to join its background
    // thread gracefully, avoiding a block_in_place panic during Drop.
    async_client.disconnect().await;

    shutdown_flag.store(true, Ordering::Relaxed);
    server_thread.join().ok();
}
