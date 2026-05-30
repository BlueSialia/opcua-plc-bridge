"""
E2E test runner using asyncua (Python OPC UA library).

Connects to the bridge, browses the address space, reads known tags,
writes a test value, and verifies OPC UA subscription notifications.

This is a second independent OPC UA client implementation (alongside
node-opcua) to catch bugs that either library might have.

#feature DRV-MODBUS, UA-TCP, UA-SESSION, UA-SEC-NONE, UA-AUTH-ANON
#feature UA-BROWSE, UA-OBJ, UA-VAR, UA-REF, UA-NS, UA-NODEID
#feature UA-READ, UA-WRITE, UA-ACCESS, UA-TYPES, UA-QUALITY, UA-TS
#feature UA-SUBS, UA-MONITOR, UA-PUBLISH

Flags:
  --expect-bad [tag ...]   Expect reads to return Bad status.
                           With no args: all tags must be Bad.
                           With args:   only listed tags must be Bad.
"""

import asyncio
import math
import sys

from asyncua import Client, ua

ENDPOINT = "opc.tcp://bridge:4840"

EXPECTED = {
    # Tags from the pymodbus-backed PLC
    "ns=2;s=holding_u16": {"type": "UInt16", "value": 0xABCD},
    "ns=2;s=holding_u32": {"type": "UInt32", "value": 0x12345678},
    "ns=2;s=holding_float": {"type": "Float", "value": math.pi, "tolerance": 0.001},
    # Tags from the stdlib (Python stdlib) PLC
    "ns=2;s=stdlib_u16": {"type": "UInt16", "value": 0xCAFE},
    "ns=2;s=stdlib_u32": {"type": "UInt32", "value": 0xDEADBEEF},
    "ns=2;s=stdlib_float": {
        "type": "Float",
        "value": math.sqrt(2),
        "tolerance": 0.001,
    },
}

WRITE_TAG = "ns=2;s=holding_u16"
WRITE_VALUE = 0xBEEF

# Parse --expect-bad with optional tag filter args
expect_bad_idx = None
try:
    expect_bad_idx = sys.argv.index("--expect-bad")
except ValueError:
    pass
EXPECT_BAD = expect_bad_idx is not None
EXPECT_BAD_TAGS = []
if EXPECT_BAD:
    EXPECT_BAD_TAGS = [
        a for a in sys.argv[expect_bad_idx + 1 :] if not a.startswith("--")
    ]

failures = 0


def fail(msg):
    global failures
    print(f"FAIL {msg}")
    failures += 1


async def browse_recursive(node, depth=0):
    """Browse children of a node recursively up to depth levels."""
    indent = "  " * depth
    try:
        for child in await node.get_children():
            name = (await child.read_browse_name()).Name
            nid = child.nodeid.to_string() if child.nodeid else "?"
            print(f"{indent}{name}: {nid}")
            if depth < 2:
                await browse_recursive(child, depth + 1)
    except Exception as e:
        print(f"{indent}Browse error: {e}")


async def main():
    global failures

    # ── Connect and create session ──────────────────────────────────
    # #feature UA-TCP, UA-SESSION, UA-SEC-NONE, UA-AUTH-ANON
    print(f"Connecting to {ENDPOINT} ...")
    client = Client(url=ENDPOINT)
    try:
        await client.connect()
        print("Session created.")
    except Exception as e:
        fail(f"connect: {e}")
        sys.exit(1)

    try:
        # ── Browse ──────────────────────────────────────────────────
        # #feature UA-BROWSE, UA-OBJ, UA-VAR, UA-REF, UA-NS, UA-NODEID
        print("\nBrowsing server address space...")
        root = client.get_root_node()
        await browse_recursive(root)

        # ── Subscription test ───────────────────────────────────────
        # #feature UA-SUBS, UA-MONITOR, UA-PUBLISH
        print("\n--- Subscription test ---")
        sub_node = client.get_node("ns=2;s=holding_u16")

        latest_value = None
        notification_count = 0

        class SubHandler:
            def datachange_notification(self, node, val, data):
                nonlocal latest_value, notification_count
                notification_count += 1
                latest_value = val
                print(f"  Subscription #{notification_count}: {node} = {val}")

        handler = SubHandler()
        sub = await client.create_subscription(100, handler)
        await sub.subscribe_data_change(sub_node)

        # Let the subscription deliver the initial value.
        await asyncio.sleep(0.5)

        # Read current value as baseline.
        before = await sub_node.read_value()
        print(f"  Baseline value: {before}")

        # Write a new value and wait for the subscription to fire.
        write_target = 0xCAFE if before == 0xBEEF else 0xBEEF
        print(f"  Writing {write_target} to {WRITE_TAG} ...")
        dv = ua.DataValue(ua.Variant(write_target, ua.VariantType.UInt16))
        await sub_node.write_value(dv)
        print("  Write succeeded")

        await asyncio.sleep(1.5)

        if notification_count == 0:
            fail("Subscription: no notifications received")
        elif latest_value != write_target:
            fail(
                f"Subscription: expected {write_target}, "
                f"got {latest_value} ({notification_count} notifications)"
            )
        else:
            print(
                f"  OK   subscription delivered {write_target} "
                f"in {notification_count} notification(s)"
            )

        await sub.delete()

        # ── Write phase ─────────────────────────────────────────────
        # #feature UA-WRITE, UA-ACCESS
        print("\n--- Write/Read tests ---")

        if not EXPECT_BAD:
            try:
                node = client.get_node(WRITE_TAG)
                dv = ua.DataValue(ua.Variant(WRITE_VALUE, ua.VariantType.UInt16))
                await node.write_value(dv)
                print(f"OK   write {WRITE_TAG} = {WRITE_VALUE}")
                EXPECTED[WRITE_TAG] = {"type": "UInt16", "value": WRITE_VALUE}
            except Exception as e:
                fail(f"write {WRITE_TAG}: {e}")

        # ── Read phase ──────────────────────────────────────────────
        # #feature UA-READ, UA-VAR, UA-TYPES, UA-QUALITY, UA-TS
        for node_id_str, exp in EXPECTED.items():
            try:
                node = client.get_node(node_id_str)
                val = await node.read_value()

                if EXPECT_BAD:
                    only_specific = len(EXPECT_BAD_TAGS) > 0
                    this_expected_bad = (
                        not only_specific or node_id_str in EXPECT_BAD_TAGS
                    )
                    if this_expected_bad:
                        fail(f"{node_id_str}: expected Bad, got {val}")
                    continue

                if exp["type"] == "Float":
                    if abs(val - exp["value"]) > exp["tolerance"]:
                        fail(f"{node_id_str}: expected ~{exp['value']}, got {val}")
                        continue
                elif val != exp["value"]:
                    fail(f"{node_id_str}: expected {exp['value']}, got {val}")
                    continue
                print(f"OK   {node_id_str} = {val}")
            except Exception as e:
                if EXPECT_BAD:
                    only_specific = len(EXPECT_BAD_TAGS) > 0
                    this_expected_bad = (
                        not only_specific or node_id_str in EXPECT_BAD_TAGS
                    )
                    if this_expected_bad:
                        print(f"OK   {node_id_str} = Bad ({e})")
                else:
                    fail(f"{node_id_str}: {e}")

    finally:
        await client.disconnect()

    if failures > 0:
        print(f"\n{failures} failure(s).")
        sys.exit(1)
    print("\nAll checks passed.")
    sys.exit(0)


asyncio.run(main())
