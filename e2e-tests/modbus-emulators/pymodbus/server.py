#!/usr/bin/env python3
"""Modbus TCP emulator for E2E testing.

Sets up known holding-register and coil values that the bridge's test
config expects, then runs forever so the bridge can poll it.
"""

import logging
import math
import struct

FORMAT = "%(asctime)s %(levelname)-5s %(name)s %(message)s"
logging.basicConfig(level=logging.DEBUG, format=FORMAT)

from pymodbus.datastore import (
    ModbusSequentialDataBlock,
    ModbusServerContext,
    ModbusSlaveContext,
)
from pymodbus.device import ModbusDeviceIdentification
from pymodbus.server import StartTcpServer


def build_context():
    """Build a Modbus slave context with pre-populated test values.

    Holding register layout (16-bit words):
      0:      0xABCD           → UInt16 tag
      2-3:    0x1234_5678      → UInt32 tag (big-endian hi/lo)
      4-5:    PI as f32        → Float tag (IEEE 754 big-endian)

    Coil layout:
      0:      True             → Bool tag
    """
    pi_bits = struct.unpack(">I", struct.pack(">f", math.pi))[0]

    holding = ModbusSequentialDataBlock(
        1,
        [
            0xABCD,  # register 1 — UInt16 (address 0 in bridge)
            0x0000,  # register 2 — padding
            0x1234,  # register 3 — UInt32 hi word (address 2 in bridge)
            0x5678,  # register 4 — UInt32 lo word
            (pi_bits >> 16)
            & 0xFFFF,  # register 5 — Float hi word (address 4 in bridge)
            pi_bits & 0xFFFF,  # register 6 — Float lo word
            0x0000,  # register 7 — WriteTest (initial value, overwritten by E2E)
        ],
    )

    coils = ModbusSequentialDataBlock(0, [True])

    store = ModbusSlaveContext(
        hr=holding,
        co=coils,
    )
    return ModbusServerContext(slaves={1: store}, single=False)


if __name__ == "__main__":
    context = build_context()
    identity = ModbusDeviceIdentification()
    identity.VendorName = "opcua-plc-bridge e2e"
    identity.ProductCode = "E2E"
    identity.VendorUrl = "https://github.com/BlueSialia/opcua-plc-bridge"
    identity.ProductName = "E2E Modbus Emulator"
    identity.ModelName = "E2E"
    identity.MajorMinorRevision = "1.0"

    print("Starting Modbus E2E emulator on 0.0.0.0:502")
    StartTcpServer(
        context=context,
        identity=identity,
        address=("0.0.0.0", 502),
    )
