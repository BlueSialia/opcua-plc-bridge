#!/bin/sh
# Layer 4 chaos scenario runner.
# Shares the chaos proxy's network namespace, applies tc netem rules
# to inject faults, runs the OPC UA test client, then cleans up.
#
# Scenarios:
#   baseline           — no faults, verifies proxy + writes work
#   latency            — 300ms added delay
#   packet-loss        — 30% random packet loss
#   disconnect-recover — 100% loss → verify CommLost → restore → verify recovery

set -e

NODE=/usr/local/bin/node
TEST_RUNNER=/scripts/test-runner.js
IFACE=eth0

passed=0
failed=0

run_test() {
    local name="$1"
    shift
    echo "=== Scenario: $name ==="
    if $NODE "$TEST_RUNNER" "$@"; then
        echo "--- $name: PASS ---"
        passed=$((passed + 1))
    else
        echo "--- $name: FAIL ---"
        failed=$((failed + 1))
    fi
}

cleanup_tc() {
    tc qdisc del dev "$IFACE" root 2>/dev/null || true
}

# ── baseline ──────────────────────────────────────────────────────────
echo ""
echo ">>> baseline (no faults)"
cleanup_tc
run_test "baseline"

# ── latency ───────────────────────────────────────────────────────────
echo ""
echo ">>> latency (300ms delay)"
cleanup_tc
tc qdisc add dev "$IFACE" root netem delay 300ms
run_test "latency"
cleanup_tc

# ── packet-loss ───────────────────────────────────────────────────────
echo ""
echo ">>> packet-loss (30% loss)"
cleanup_tc
tc qdisc add dev "$IFACE" root netem loss 30%
run_test "packet-loss"
cleanup_tc

# ── disconnect-recover ────────────────────────────────────────────────
echo ""
echo ">>> disconnect-recover (100% loss → verify Bad → recover → verify Good)"
cleanup_tc

# Drop everything to force the bridge's Modbus connection to break.
# Only pymodbus PLC traffic goes through the chaos proxy; stdlib tags
# remain Good. We scope --expect-bad to only the pymodbus tags.
tc qdisc add dev "$IFACE" root netem loss 100%
echo "    100% packet loss active, waiting for bridge to detect failure ..."
sleep 6

# Verify pymodbus tags report Bad quality (CommLost) while the
# connection is severed. Stdlib tags should still be Good — we don't
# pass them to --expect-bad.
echo "    checking tag quality during outage ..."
if $NODE "$TEST_RUNNER" --expect-bad \
    "ns=2;s=holding_u16" \
    "ns=2;s=holding_u32" \
    "ns=2;s=holding_float"; then
    echo "    quality check: PASS (pymodbus tags correctly report Bad)"
else
    echo "    quality check: FAIL (expected Bad status)"
    failed=$((failed + 1))
fi

# Restore connectivity.
tc qdisc del dev "$IFACE" root
echo "    connectivity restored, waiting for bridge to reconnect ..."
sleep 4

run_test "disconnect-recover"
cleanup_tc

# ── summary ───────────────────────────────────────────────────────────
total=$((passed + failed))
echo ""
echo "========================================="
echo "Chaos test results: $passed/$total passed"
echo "========================================="

if [ "$failed" -gt 0 ]; then
    exit 1
fi
exit 0
