FROM python:3.12-alpine

# No pip dependencies — uses only Python stdlib (socket + struct).
# Completely independent codebase from pymodbus.
COPY server.py /server.py
EXPOSE 502
CMD ["python", "/server.py"]
