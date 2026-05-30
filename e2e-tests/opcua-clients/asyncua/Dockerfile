FROM python:3.12-alpine

WORKDIR /app
COPY requirements.txt ./
RUN pip install --no-cache-dir -r requirements.txt

COPY test-runner.py ./

ENTRYPOINT ["python", "test-runner.py"]
