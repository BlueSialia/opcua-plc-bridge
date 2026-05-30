#!/usr/bin/env python3
"""Minimal Modbus TCP server using only Python stdlib (no pymodbus).

Serves holding registers with pre-populated test values so the
bridge can poll it. This exercises a completely independent Modbus
implementation to catch bugs that pymodbus might also have.

Holding register layout (16-bit words):
  0:      0xCAFE           -> UInt16 tag
  2-3:    0xDEAD_BEEF      -> UInt32 tag (big-endian hi/lo)
  4-5:    sqrt(2) as f32   -> Float tag (IEEE 754 big-endian)
  6:      0x0000           -> WriteTest
"""

import logging
import math
import socket
import struct
import threading

FORMAT = "%(asctime)s %(levelname)-5s %(name)s %(message)s"
logging.basicConfig(level=logging.DEBUG, format=FORMAT)
logger = logging.getLogger("modbus-stdlib")

FC_READ_HOLDING = 0x03
FC_WRITE_SINGLE_REG = 0x06
FC_WRITE_MULTI_REG = 0x10
EX_ILLEGAL_FUNC = 0x01
EX_ILLEGAL_ADDR = 0x02
EX_ILLEGAL_DATA = 0x03


class ModbusServer:
    def __init__(self, host="0.0.0.0", port=502):
        self.host = host
        self.port = port
        sqrt2_bits = struct.unpack(">I", struct.pack(">f", math.sqrt(2)))[0]
        self.holding = {
            0: 0xCAFE,
            1: 0x0000,
            2: 0xDEAD,
            3: 0xBEEF,
            4: (sqrt2_bits >> 16) & 0xFFFF,
            5: sqrt2_bits & 0xFFFF,
            6: 0x0000,
        }

    def _read_holding(self, addr, count):
        if count < 1 or count > 125:
            return None, EX_ILLEGAL_DATA
        values = []
        for i in range(count):
            v = self.holding.get(addr + i)
            if v is None:
                return None, EX_ILLEGAL_ADDR
            values.append(v)
        return values, None

    def _build_error(self, func, exc):
        return struct.pack(">BBB", func | 0x80, exc, 0)

    def handle(self, raw):
        if len(raw) < 8:
            return None
        txn_id, proto_id = raw[0:2], raw[2:4]
        unit_id, func = raw[6], raw[7]
        prefix = txn_id + proto_id

        if func == FC_READ_HOLDING and len(raw) >= 12:
            addr = struct.unpack(">H", raw[8:10])[0]
            count = struct.unpack(">H", raw[10:12])[0]
            logger.debug("READ_HOLDING addr=%d count=%d", addr, count)
            values, exc = self._read_holding(addr, count)
            if exc is not None:
                return prefix + self._build_error(func, exc)
            payload = bytes([unit_id, func, count * 2]) + struct.pack(
                ">" + "H" * count, *values
            )
            return prefix + struct.pack(">H", len(payload)) + payload

        elif func == FC_WRITE_SINGLE_REG and len(raw) >= 12:
            addr = struct.unpack(">H", raw[8:10])[0]
            value = struct.unpack(">H", raw[10:12])[0]
            logger.debug("WRITE_SINGLE_REG addr=%d value=0x%04X", addr, value)
            self.holding[addr] = value & 0xFFFF
            return raw

        elif func == FC_WRITE_MULTI_REG and len(raw) >= 13:
            addr = struct.unpack(">H", raw[8:10])[0]
            count = struct.unpack(">H", raw[10:12])[0]
            byte_count = raw[12]
            expected = count * 2
            if byte_count != expected or len(raw) < 13 + expected:
                return prefix + self._build_error(func, EX_ILLEGAL_DATA)
            values = list(struct.unpack(">" + "H" * count, raw[13 : 13 + expected]))
            logger.debug(
                "WRITE_MULTI_REG addr=%d count=%d values=%s", addr, count, values
            )
            for i, v in enumerate(values):
                self.holding[addr + i] = v & 0xFFFF
            payload = struct.pack(">BBHH", unit_id, func, addr, count)
            return prefix + struct.pack(">H", len(payload)) + payload

        else:
            logger.warning("Unsupported function code: 0x%02X", func)
            return prefix + self._build_error(func, EX_ILLEGAL_FUNC)

    def _handle_client(self, sock, addr):
        logger.info("Client connected: %s", addr)
        try:
            while True:
                raw = sock.recv(4096)
                if not raw:
                    break
                resp = self.handle(raw)
                if resp is not None:
                    sock.sendall(resp)
        except Exception:
            logger.exception("Error handling client %s", addr)
        finally:
            sock.close()
            logger.info("Client disconnected: %s", addr)

    def run(self):
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind((self.host, self.port))
        server.listen(5)
        logger.info("Modbus stdlib server listening on %s:%d", self.host, self.port)
        while True:
            sock, addr = server.accept()
            t = threading.Thread(target=self._handle_client, args=(sock, addr))
            t.daemon = True
            t.start()


if __name__ == "__main__":
    ModbusServer().run()
