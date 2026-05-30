/**
 * E2E test runner: connects to the bridge, browses the address space,
 * writes a test value, reads known tags via their string NodeIds, asserts
 * values, and verifies OPC UA subscription (data change) notifications.
 *
 * This is one of two independent OPC UA client implementations (alongside
 * the Python asyncua client) to catch bugs that either library might have.
 *
 * #feature DRV-MODBUS, UA-TCP, UA-SESSION, UA-SEC-NONE, UA-AUTH-ANON
 * #feature UA-BROWSE, UA-OBJ, UA-VAR, UA-REF, UA-NS, UA-NODEID
 * #feature UA-READ, UA-WRITE, UA-ACCESS, UA-TYPES, UA-QUALITY, UA-TS
 * #feature UA-SUBS, UA-MONITOR, UA-PUBLISH
 *
 * The OPC UA server uses tag IDs as string NodeIds in namespace 2:
 *   ns=2;s=holding_u16
 *   ns=2;s=holding_u32
 *   ns=2;s=holding_float
 *   ns=2;s=stdlib_u16
 *   ns=2;s=stdlib_u32
 *   ns=2;s=stdlib_float
 *
 * Flags:
 *   --expect-bad [tag ...]   Expect reads to return Bad status.
 *                            With no args: all tags must be Bad.
 *                            With args:   only listed tags must be Bad.
 *
 * Exits 0 on success, 1 on any failure.
 */

import {
  AttributeIds,
  OPCUAClient,
  resolveNodeId,
  StatusCodes,
  TimestampsToReturn,
} from "node-opcua";

const ENDPOINT = "opc.tcp://bridge:4840";

const EXPECTED = {
  // Tags from the pymodbus-backed PLC
  "ns=2;s=holding_u16": { type: "UInt16", value: 0xabcd },
  "ns=2;s=holding_u32": { type: "UInt32", value: 0x12345678 },
  "ns=2;s=holding_float": { type: "Float", value: Math.PI, tolerance: 0.001 },
  // Tags from the stdlib (Python stdlib) PLC
  "ns=2;s=stdlib_u16": { type: "UInt16", value: 0xcafe },
  "ns=2;s=stdlib_u32": { type: "UInt32", value: 0xdeadbeef },
  "ns=2;s=stdlib_float": {
    type: "Float",
    value: Math.SQRT2,
    tolerance: 0.001,
  },
};

const WRITE_TAG = "ns=2;s=holding_u16";
const WRITE_VALUE = 0xbeef;
const expectBadIdx = process.argv.indexOf("--expect-bad");
const EXPECT_BAD = expectBadIdx !== -1;
const EXPECT_BAD_TAGS = EXPECT_BAD
  ? process.argv.slice(expectBadIdx + 1).filter((a) => !a.startsWith("--"))
  : [];

function fail(msg) {
  console.error(`FAIL ${msg}`);
}

function statusName(code) {
  for (const [k, v] of Object.entries(StatusCodes)) {
    if (v.value === code || v === code) return k;
  }
  return `0x${code.toString(16)}`;
}

async function browseServer(session) {
  // #feature UA-BROWSE, UA-OBJ, UA-VAR, UA-REF, UA-NS, UA-NODEID
  console.log("Browsing server address space...");
  try {
    const browseResult = await session.browse("RootFolder");
    if (browseResult.references) {
      for (const ref of browseResult.references) {
        console.log(
          `  ${ref.browseName?.toString()}: ${ref.nodeId?.toString()}`,
        );
        if (ref.nodeId) {
          try {
            const sub = await session.browse(ref.nodeId);
            if (sub.references) {
              for (const sref of sub.references) {
                console.log(
                  `    ${sref.browseName?.toString()}: ${sref.nodeId?.toString()}`,
                );
                if (sref.nodeId) {
                  try {
                    const sub2 = await session.browse(sref.nodeId);
                    if (sub2.references) {
                      for (const s2ref of sub2.references) {
                        console.log(
                          `      ${s2ref.browseName?.toString()}: ${s2ref.nodeId?.toString()}`,
                        );
                      }
                    }
                  } catch (_) {}
                }
              }
            }
          } catch (_) {}
        }
      }
    }
  } catch (e) {
    console.log(`Browse error: ${e.message}`);
  }
}

async function main() {
  // #feature UA-TCP, UA-SESSION, UA-SEC-NONE, UA-AUTH-ANON
  const client = OPCUAClient.create({
    endpointMustExist: false,
    securityMode: 1, // None
    securityPolicy: "http://opcfoundation.org/UA/SecurityPolicy#None",
    connectionStrategy: { initialDelay: 1000, maxRetry: 10, maxDelay: 2000 },
    requestedSessionTimeout: 30000,
  });

  console.log(`Connecting to ${ENDPOINT} ...`);
  await client.connect(ENDPOINT);
  const session = await client.createSession();
  console.log("Session created.");

  await browseServer(session);

  let failures = 0;

  // ── Subscription test ──────────────────────────────────────────────
  // #feature UA-SUBS, UA-MONITOR, UA-PUBLISH
  //
  // Create a monitored item on holding_u16, write a new value, and
  // verify the subscription fires with the expected value.
  {
    console.log("\n--- Subscription test ---");

    const subNodeId = resolveNodeId("ns=2;s=holding_u16");

    // Create a subscription with a short publishing interval so we get
    // notifications quickly.
    const subscription = await session.createSubscription2({
      requestedPublishingInterval: 100, // ms
      requestedLifetimeCount: 60,
      requestedMaxKeepAliveCount: 10,
      maxNotificationsPerPublish: 5,
      publishingEnabled: true,
      priority: 10,
    });

    // Accumulate the latest monitored value.
    let latestValue = undefined;
    let notificationCount = 0;

    const monitoredItem = await subscription.monitor(
      {
        nodeId: subNodeId,
        attributeId: AttributeIds.Value,
      },
      {
        samplingInterval: 50, // sample every 50ms
        discardOldest: true,
        queueSize: 1,
      },
      TimestampsToReturn.Both,
    );

    monitoredItem.on("changed", (dataValue) => {
      notificationCount++;
      latestValue = dataValue.value.value;
      console.log(
        `  Subscription #${notificationCount}: ${subNodeId.toString()} = ${latestValue}`,
      );
    });

    // Give the subscription time to establish and receive the initial value.
    await new Promise((resolve) => setTimeout(resolve, 500));

    // Read current value as baseline
    const before = await session.read({
      nodeId: subNodeId,
      attributeId: AttributeIds.Value,
    });
    const beforeValue = before.value.value;
    console.log(`  Baseline value: ${beforeValue}`);

    // Write a new value — the subscription should fire with the change.
    const writeTarget = beforeValue === 0xbeef ? 0xcafe : 0xbeef;
    console.log(`  Writing ${writeTarget} to ${WRITE_TAG} ...`);
    const ws = await session.write({
      nodeId: subNodeId,
      attributeId: AttributeIds.Value,
      value: { value: { dataType: "UInt16", value: writeTarget } },
    });
    if (ws.value !== StatusCodes.Good.value) {
      fail(`subscription write: status ${statusName(ws.value)}`);
      failures++;
    } else {
      console.log(`  Write succeeded`);
    }

    // Wait for the subscription to fire with the new value.
    // The driver polls every 200ms and the subscription samples every 50ms,
    // so we should get a notification within ~1 second.
    await new Promise((resolve) => setTimeout(resolve, 1500));

    // Verify we received at least one notification.
    if (notificationCount === 0) {
      fail("Subscription: no notifications received");
      failures++;
    } else if (latestValue !== writeTarget) {
      fail(
        `Subscription: expected ${writeTarget}, got ${latestValue} (${notificationCount} notifications)`,
      );
      failures++;
    } else {
      console.log(
        `  OK   subscription delivered ${writeTarget} in ${notificationCount} notification(s)`,
      );
    }

    await subscription.terminate();
  }

  // ── Write phase ─────────────────────────────────────────────────────
  // #feature UA-WRITE, UA-ACCESS
  console.log("\n--- Write/Read tests ---");

  if (!EXPECT_BAD) {
    try {
      const nodeId = resolveNodeId(WRITE_TAG);
      const s = await session.write({
        nodeId,
        attributeId: AttributeIds.Value,
        value: { value: { dataType: "UInt16", value: WRITE_VALUE } },
      });
      if (s.value !== StatusCodes.Good.value) {
        fail(`write ${WRITE_TAG}: status ${statusName(s.value)} (${s.value})`);
        failures++;
      } else {
        console.log(`OK   write ${WRITE_TAG} = ${WRITE_VALUE}`);
        EXPECTED[WRITE_TAG] = { type: "UInt16", value: WRITE_VALUE };
      }
    } catch (e) {
      fail(`write ${WRITE_TAG}: ${e.message}`);
      failures++;
    }
  }

  // ── Read phase ──────────────────────────────────────────────────────
  // #feature UA-READ, UA-VAR, UA-TYPES, UA-QUALITY, UA-TS
  for (const [nodeIdStr, exp] of Object.entries(EXPECTED)) {
    try {
      const nodeId = resolveNodeId(nodeIdStr);
      const r = await session.read({ nodeId, attributeId: AttributeIds.Value });

      if (EXPECT_BAD) {
        const onlySpecific = EXPECT_BAD_TAGS.length > 0;
        const thisTagExpectedBad =
          !onlySpecific || EXPECT_BAD_TAGS.includes(nodeIdStr);
        if (thisTagExpectedBad) {
          if (r.statusCode.value === StatusCodes.Good.value) {
            fail(`${nodeIdStr}: expected Bad, got Good`);
            failures++;
          } else {
            console.log(
              `OK   ${nodeIdStr} = Bad (${statusName(r.statusCode.value)})`,
            );
          }
        }
        continue;
      }

      if (r.statusCode.value !== StatusCodes.Good.value) {
        fail(
          `${nodeIdStr}: status ${statusName(r.statusCode.value)} (${r.statusCode.value})`,
        );
        failures++;
        continue;
      }

      const v = r.value.value;
      if (exp.type === "Float") {
        if (Math.abs(v - exp.value) > exp.tolerance) {
          fail(`${nodeIdStr}: expected ~${exp.value}, got ${v}`);
          failures++;
          continue;
        }
      } else if (v !== exp.value) {
        fail(`${nodeIdStr}: expected ${exp.value}, got ${v}`);
        failures++;
        continue;
      }
      console.log(`OK   ${nodeIdStr} = ${v}`);
    } catch (e) {
      fail(`${nodeIdStr}: ${e.message}`);
      failures++;
    }
  }

  await session.close();
  await client.disconnect();

  if (failures > 0) {
    console.error(`\n${failures} failure(s).`);
    process.exit(1);
  }
  console.log("\nAll checks passed.");
  process.exit(0);
}

main().catch((e) => {
  console.error("Fatal:", e.message);
  process.exit(1);
});
