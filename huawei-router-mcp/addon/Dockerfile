FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates jq && rm -rf /var/lib/apt/lists/*
RUN python3 -m venv /opt/venv
ENV PATH="/opt/venv/bin:$PATH" PYTHONUNBUFFERED=1
COPY requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir -r /tmp/requirements.txt mcp-proxy
COPY server.py /app/server.py
COPY addon/run.sh /run.sh
RUN chmod 755 /run.sh
WORKDIR /app
CMD ["/run.sh"]
