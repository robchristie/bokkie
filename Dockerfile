FROM python:3.12-slim

ARG CODEX_VERSION=0.115.0

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    nodejs \
    npm \
    openssh-client \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir uv \
    && npm install -g "@openai/codex@${CODEX_VERSION}"

WORKDIR /workspace

CMD ["sh", "-lc", "uv run bokkie api --host 0.0.0.0 --port 8008"]
