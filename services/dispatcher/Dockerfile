# --- Stage 1: build the React SPA ------------------------------------------
FROM node:20-alpine AS web
WORKDIR /web
# Aliyun npm mirror — registry.npmjs.org is unreliable from CN data centers.
# Override with `--build-arg NPM_REGISTRY=https://registry.npmjs.org/` for
# OSS or offshore builds.
ARG NPM_REGISTRY=https://registry.npmmirror.com/
RUN npm config set registry "$NPM_REGISTRY"
COPY web/package.json ./
RUN npm install --no-fund --no-audit
COPY web/ ./
RUN npm run build

# --- Stage 2: runtime ------------------------------------------------------
FROM python:3.11-slim
WORKDIR /srv

# Aliyun pip mirror — pypi.org is unreliable from CN data centers AND
# the python:3.11-slim image's DNS resolver hits systemd-resolved on
# the host through docker's default bridge, which often fails to
# resolve external names. The mirror IS resolvable via the docker
# daemon's explicit DNS config (see /etc/docker/daemon.json).
ARG PIP_INDEX_URL=https://mirrors.aliyun.com/pypi/simple/
ARG PIP_TRUSTED_HOST=mirrors.aliyun.com
ENV PIP_INDEX_URL=${PIP_INDEX_URL} PIP_TRUSTED_HOST=${PIP_TRUSTED_HOST}

# Python deps first so they're cached across code changes.
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY app ./app
COPY --from=web /web/dist ./web/dist

# Where the SQLite DB lives. The bind mount in docker-compose maps this
# to a host volume so data survives container rebuilds.
ENV DB_URL=sqlite:////data/dispatcher.db
ENV STORAGE_DIR=/data/storage

EXPOSE 8080
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8080"]
