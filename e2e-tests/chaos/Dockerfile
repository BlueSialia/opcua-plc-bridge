FROM alpine:3.21

RUN apk add --no-cache socat

EXPOSE 502

# Forward TCP connections to the real modbus emulator.
# The bridge connects to this container (hostname "chaos") on port 502,
# and socat transparently forwards to "modbus-real:502".
ENTRYPOINT ["socat", "TCP-LISTEN:502,fork,reuseaddr", "TCP:modbus-real:502"]
