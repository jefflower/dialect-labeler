"""
faster-whisper HTTP server for the dialect-labeler distributed ASR pool.

POST /transcribe (multipart/form-data):
    audio:          .wav (or anything ffmpeg can read) — the segment file
    initial_prompt: optional Chinese guidance prompt
    language:       defaults to "zh"; "" or "auto" disables forced language
    beam_size:      optional int (default 5)

GET /health → {ok, model, device, compute_type, loaded_at}
GET /        → same as /health (so curl http://host:9090/ works at a glance)

Model + device + compute_type come from env vars so launchd can pin them:
    WHISPER_MODEL=large-v3-turbo
    WHISPER_DEVICE=cpu | auto
    WHISPER_COMPUTE_TYPE=int8 | int8_float16 | float16 | float32
    WHISPER_PORT=9090   (read by setup.sh; FastAPI itself doesn't read it)

faster-whisper auto-downloads the CTranslate2-converted model from
HuggingFace on first call (~1.5GB for large-v3-turbo). Subsequent boots
warm-load the cached version (~5s).
"""

from __future__ import annotations

import logging
import os
import tempfile
import time
from pathlib import Path

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from faster_whisper import WhisperModel

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(message)s",
)
log = logging.getLogger("whisper-server")

MODEL_NAME = os.environ.get("WHISPER_MODEL", "large-v3-turbo")
DEVICE = os.environ.get("WHISPER_DEVICE", "cpu")
COMPUTE_TYPE = os.environ.get("WHISPER_COMPUTE_TYPE", "int8")

app = FastAPI(title="dialect-labeler whisper-server", version="1.0")

log.info(
    "loading model=%s device=%s compute_type=%s ...",
    MODEL_NAME,
    DEVICE,
    COMPUTE_TYPE,
)
_t0 = time.time()
model = WhisperModel(MODEL_NAME, device=DEVICE, compute_type=COMPUTE_TYPE)
LOADED_AT = time.time()
log.info("model loaded in %.1fs", LOADED_AT - _t0)


def _health() -> dict:
    return {
        "ok": True,
        "model": MODEL_NAME,
        "device": DEVICE,
        "compute_type": COMPUTE_TYPE,
        "loaded_at": LOADED_AT,
    }


@app.get("/health")
def health() -> dict:
    return _health()


@app.get("/")
def root() -> dict:
    return _health()


@app.post("/transcribe")
async def transcribe(
    audio: UploadFile = File(...),
    initial_prompt: str = Form(""),
    language: str = Form("zh"),
    beam_size: int = Form(5),
) -> dict:
    suffix = Path(audio.filename or "seg.wav").suffix or ".wav"
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as f:
        f.write(await audio.read())
        path = f.name

    lang = language.strip()
    if lang in ("", "auto"):
        lang = None

    prompt = initial_prompt.strip() or None

    started = time.time()
    try:
        segments, info = model.transcribe(
            path,
            language=lang,
            initial_prompt=prompt,
            beam_size=beam_size,
            # Dialect-labeler segments are short and independent; do NOT carry
            # prior context across them — matches the local CLI flag.
            condition_on_previous_text=False,
        )
        out_segments = [
            {"start": s.start, "end": s.end, "text": s.text} for s in segments
        ]
        text = "".join(s["text"] for s in out_segments).strip()
        elapsed = time.time() - started
        log.info(
            "ok bytes=%d lang=%s text_len=%d elapsed=%.2fs",
            os.path.getsize(path),
            info.language,
            len(text),
            elapsed,
        )
        return {
            "text": text,
            "language": info.language,
            "language_probability": info.language_probability,
            "duration": info.duration,
            "segments": out_segments,
            "elapsed_sec": elapsed,
            "model": MODEL_NAME,
        }
    except Exception as err:  # noqa: BLE001 — surface to client
        log.exception("transcribe failed")
        raise HTTPException(status_code=500, detail=str(err)) from err
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass
