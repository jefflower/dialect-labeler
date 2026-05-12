# --- Stage 1: build the React SPA ------------------------------------------
FROM node:20-alpine AS web
WORKDIR /web
COPY web/package.json ./
RUN npm install --no-fund --no-audit
COPY web/ ./
RUN npm run build

# --- Stage 2: runtime ------------------------------------------------------
FROM python:3.11-slim
WORKDIR /srv

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
