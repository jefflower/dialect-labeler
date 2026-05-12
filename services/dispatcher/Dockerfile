FROM python:3.11-slim

WORKDIR /srv

# Install deps first so they're cached across code changes.
COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY app ./app

# Where the SQLite DB lives. The bind mount in docker-compose maps this
# to a host volume so data survives container rebuilds.
ENV DB_URL=sqlite:////data/dispatcher.db
ENV STORAGE_DIR=/data/storage

EXPOSE 8080
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8080"]
