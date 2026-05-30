FROM python:3.12-alpine

RUN pip install --no-cache-dir pymodbus==3.8.0

COPY server.py /server.py

EXPOSE 502

CMD ["python", "/server.py"]
