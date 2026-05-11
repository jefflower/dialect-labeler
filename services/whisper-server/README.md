# whisper-server

faster-whisper HTTP service for the dialect-labeler distributed ASR pool.

Each tailnet Mac (.4 / .6 / .2 — same hosts that run Ollama) runs one copy
on port `9090`. The Tauri client POSTs each segment WAV to the pool and a
work-stealing scheduler routes around slow / unreachable nodes (same
pattern as the Ollama polish pool — see `src-tauri/src/lib.rs`).

## API

```
POST /transcribe       multipart: audio=<wav>, initial_prompt=..., language=zh
GET  /health           {ok, model, device, compute_type, loaded_at}
```

## Install / re-deploy

```bash
# From your dev box, push the service folder, then run setup remotely:
scp -r services/whisper-server huayu@100.64.0.4:~/
ssh huayu@100.64.0.4 'bash ~/whisper-server/setup.sh'

# Logs (first run downloads ~1.5GB of model from HuggingFace, ≈3-5min):
ssh huayu@100.64.0.4 'tail -f ~/whisper-server/log/err.log'

# Verify:
curl http://100.64.0.4:9090/health
```

`setup.sh` is idempotent — re-run after pulling code changes to refresh the
service.

## Config knobs (env vars passed to `setup.sh`)

| Var | Default | Notes |
|---|---|---|
| `PORT` | `9090` | HTTP port |
| `WHISPER_MODEL` | `large-v3-turbo` | matches client default; `large-v3` for max quality at 2× cost |
| `WHISPER_DEVICE` | `cpu` | CTranslate2 has no Metal yet on Apple Silicon; CPU+int8 is still 2–3× faster than openai-whisper CPU |
| `WHISPER_COMPUTE_TYPE` | `int8` | drop to `int8_float16` if accuracy regression noticed |

The model is cached under `~/.cache/huggingface/hub/`; subsequent boots load in ≈5 s.

## Resource sharing with Ollama

Both services run on the same Macs. Whisper is CPU-bound, Ollama (qwen2.5:32b)
is GPU/unified-memory bound — they don't compete for the same silicon. Peak
RSS for whisper-server is ≈2 GB (int8 large-v3-turbo + audio buffers).

## Uninstall

```bash
launchctl unload ~/Library/LaunchAgents/com.dialect-labeler.whisper.plist
rm    ~/Library/LaunchAgents/com.dialect-labeler.whisper.plist
rm -r ~/whisper-server
```
