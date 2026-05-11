#!/usr/bin/env bash
#
# Install or refresh the dialect-labeler whisper-server on a Mac.
#
# Idempotent — safe to re-run after pulling a new whisper_server.py. It will
# rebuild the venv if missing, refresh deps, regenerate the launchd plist,
# and bounce the service.
#
# Env knobs (all optional):
#   PORT            HTTP port (default 9090)
#   WHISPER_MODEL   model name passed to faster-whisper (default large-v3-turbo)
#   WHISPER_DEVICE  cpu | auto | cuda  (default cpu — Apple Silicon CT2 has no
#                                       Metal yet; CPU+int8 is still 2-3× faster
#                                       than openai-whisper CPU)
#   WHISPER_COMPUTE_TYPE  int8 | int8_float16 | float16 | float32 (default int8)
#   INSTALL_DIR     where to install (default ~/whisper-server)
#
# Assumes Python ≥3.10 reachable as `python3` (Homebrew puts one at
# /opt/homebrew/bin/python3 on Apple Silicon). The script copies
# whisper_server.py from $(dirname "$0") so it must be run from the
# checked-out repo dir.

set -euo pipefail

PORT="${PORT:-9090}"
MODEL="${WHISPER_MODEL:-large-v3-turbo}"
DEVICE="${WHISPER_DEVICE:-cpu}"
COMPUTE="${WHISPER_COMPUTE_TYPE:-int8}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/whisper-server}"

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"

# Pick python3. Honors `PY` env-var override so a host with a broken brew
# python (e.g. Python 3.14's ensurepip glitch on some macOS releases) can
# fall back to system /usr/bin/python3 without editing the script.
if [ -z "${PY:-}" ]; then
    for candidate in /opt/homebrew/bin/python3 /usr/local/bin/python3 python3; do
        if command -v "$candidate" >/dev/null 2>&1; then
            PY="$candidate"; break
        fi
    done
fi
if [ -z "${PY:-}" ]; then
    echo "ERROR: python3 not found. brew install python@3.11" >&2
    exit 1
fi

echo ">> install dir : $INSTALL_DIR"
echo ">> python      : $PY ($("$PY" --version 2>&1))"
echo ">> port        : $PORT"
echo ">> model       : $MODEL"
echo ">> device      : $DEVICE"
echo ">> compute     : $COMPUTE"

mkdir -p "$INSTALL_DIR/log"
cp "$SRC_DIR/whisper_server.py" "$INSTALL_DIR/whisper_server.py"
cp "$SRC_DIR/requirements.txt"  "$INSTALL_DIR/requirements.txt"

VENV="$INSTALL_DIR/.venv"
if [ ! -x "$VENV/bin/python" ]; then
    echo ">> creating venv ..."
    "$PY" -m venv "$VENV"
fi

echo ">> installing deps ..."
"$VENV/bin/pip" install -q --upgrade pip
"$VENV/bin/pip" install -q -r "$INSTALL_DIR/requirements.txt"

# Render plist from template.
PLIST_SRC="$SRC_DIR/com.dialect-labeler.whisper.plist.template"
PLIST_DST="$HOME/Library/LaunchAgents/com.dialect-labeler.whisper.plist"
mkdir -p "$HOME/Library/LaunchAgents"
sed \
    -e "s|__DIR__|$INSTALL_DIR|g" \
    -e "s|__VENV__|$VENV|g" \
    -e "s|__PORT__|$PORT|g" \
    -e "s|__MODEL__|$MODEL|g" \
    -e "s|__DEVICE__|$DEVICE|g" \
    -e "s|__COMPUTE__|$COMPUTE|g" \
    "$PLIST_SRC" > "$PLIST_DST"

echo ">> reloading launchd ..."
launchctl unload "$PLIST_DST" 2>/dev/null || true
sleep 1
launchctl load "$PLIST_DST"

echo ""
echo ">> done. Tail logs to watch model download (first run ≈1.5GB):"
echo "   tail -f $INSTALL_DIR/log/err.log"
echo ""
echo ">> verify (give it 30s on first run):"
echo "   curl -s http://localhost:$PORT/health | python3 -m json.tool"
