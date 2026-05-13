#!/usr/bin/env python3
"""
Local Mode-2 semantic cutter.

Given a long mono WAV, this script:
  1. Pre-cuts on silence into ~10-25s pieces
  2. ASRs each piece via the tailnet faster-whisper pool (parallel)
  3. Asks Qwen 32B (huayu) to merge pieces into semantic segments
     ≤ 90s respecting complete utterances
  4. Re-cuts the source WAV at those boundaries into WAV segments
  5. Writes a Mode-1-compatible `project.json` so the Tauri client on
     macOS / Windows can open the output directory and continue
     annotating

Output layout matches Mode 1 exactly:
    <output_dir>/
        project.json
        source/
            <original-name>.wav
        segments/
            <basename>_NNNN_<startMs>-<endMs>.wav
            ...

Usage:
    python3 scripts/mode2_local_cut.py <input.wav> <output_dir>

Env (optional):
    WHISPER_POOL=http://a:9090,http://b:9090     comma-separated
    OLLAMA_URL=http://100.64.0.4:11434
    OLLAMA_MODEL=qwen2.5:32b
    MAX_SEGMENT_S=90
    MIN_SEGMENT_S=6
    NUM_CTX=32768
"""
from __future__ import annotations

import concurrent.futures as cf
import json
import os
import re
import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any

try:
    import requests  # type: ignore
except ImportError:
    print("requests not installed. Install with: pip3 install requests", file=sys.stderr)
    sys.exit(1)

# Bypass any system HTTP proxy. The dev box typically has Clash /
# Surge listening on 127.0.0.1:7897 with HTTP_PROXY env vars exported
# globally for shell convenience. The proxy can't reach the tailnet
# CIDR (100.64.0.0/10) and returns 502 when asked to. Tailnet IPs are
# direct routes on the local box, so just turn `trust_env` off for the
# session — requests then ignores HTTP_PROXY / HTTPS_PROXY entirely.
_session = requests.Session()
_session.trust_env = False

# ---- config -----------------------------------------------------------

DEFAULT_WHISPER_POOL = [
    "http://100.64.0.2:9090",
    "http://100.64.0.4:9090",
    "http://100.64.0.6:9090",
    "http://100.64.0.11:9090",
]
WHISPER_POOL = [
    u.strip() for u in os.environ.get("WHISPER_POOL", ",".join(DEFAULT_WHISPER_POOL)).split(",") if u.strip()
]
OLLAMA_URL = os.environ.get("OLLAMA_URL", "http://100.64.0.4:11434")
OLLAMA_MODEL = os.environ.get("OLLAMA_MODEL", "qwen2.5:32b")

MAX_SEGMENT_S = float(os.environ.get("MAX_SEGMENT_S", "90"))
MIN_SEGMENT_S = float(os.environ.get("MIN_SEGMENT_S", "6"))
NUM_CTX = int(os.environ.get("NUM_CTX", "32768"))

# Pre-cut tuning. We aim for pieces around TARGET_PIECE_S so Qwen has
# fine enough granularity to land cut boundaries on actual silence,
# but not so fine that we blow the prompt context. For 13-min audio:
#   805s / 15s ≈ 54 pieces → 54 lines in prompt → still fits.
TARGET_PIECE_S = 15.0
SILENCE_DB = -30.0
SILENCE_MIN_MS = 300

# ---- helpers ----------------------------------------------------------


def run_ffmpeg(args: list[str], capture_stderr: bool = False) -> str:
    proc = subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error" if not capture_stderr else "info", *args],
        stderr=subprocess.PIPE if capture_stderr else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"ffmpeg failed: {' '.join(args)}\n{proc.stderr}")
    return proc.stderr if capture_stderr else proc.stdout


def duration_ms(wav: str) -> int:
    out = subprocess.check_output(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", wav],
        text=True,
    ).strip()
    return int(float(out) * 1000)


def detect_silences(wav: str, db: float = SILENCE_DB, min_ms: int = SILENCE_MIN_MS) -> list[tuple[int, int]]:
    """Return list of (silence_start_ms, silence_end_ms)."""
    out = run_ffmpeg(
        ["-i", wav, "-af", f"silencedetect=noise={db}dB:d={min_ms / 1000}", "-f", "null", "-"],
        capture_stderr=True,
    )
    sil: list[tuple[int, int]] = []
    pending_start: float | None = None
    for line in out.splitlines():
        m = re.search(r"silence_start:\s*([-+0-9.]+)", line)
        if m:
            pending_start = float(m.group(1))
            continue
        m = re.search(r"silence_end:\s*([-+0-9.]+)", line)
        if m and pending_start is not None:
            end_s = float(m.group(1))
            start_ms = max(0, int(pending_start * 1000))
            end_ms = int(end_s * 1000)
            if end_ms > start_ms:
                sil.append((start_ms, end_ms))
            pending_start = None
    return sil


def fine_pre_cut(wav: str, target_piece_ms: int = int(TARGET_PIECE_S * 1000)) -> list[tuple[int, int]]:
    """Cut into pieces at silence midpoints, aiming for ~target piece length."""
    total = duration_ms(wav)
    silences = detect_silences(wav)
    boundaries = [0]
    last = 0
    for s, e in silences:
        mid = (s + e) // 2
        # Skip silences too close to the last boundary; only commit
        # when we've accumulated roughly a target piece's worth.
        if mid - last >= target_piece_ms * 0.55:
            boundaries.append(mid)
            last = mid
    if boundaries[-1] != total:
        boundaries.append(total)
    pieces = [(boundaries[i], boundaries[i + 1]) for i in range(len(boundaries) - 1)]
    # Merge any tiny tail piece into its predecessor.
    if len(pieces) >= 2 and (pieces[-1][1] - pieces[-1][0]) < 1500:
        last_piece = pieces.pop()
        prev = pieces.pop()
        pieces.append((prev[0], last_piece[1]))
    return pieces


def cut_segment(input_wav: str, start_ms: int, end_ms: int, output_wav: str) -> None:
    """Sample-accurate copy using `-ss` *after* `-i` ensures no codec
    realignment. For mono 16-bit PCM (common after recording), copy is
    fine; for compressed sources, we'd need re-encode."""
    dur_s = (end_ms - start_ms) / 1000.0
    run_ffmpeg(
        [
            "-y",
            "-i",
            input_wav,
            "-ss",
            f"{start_ms / 1000:.3f}",
            "-t",
            f"{dur_s:.3f}",
            "-c",
            "copy",
            output_wav,
        ]
    )


import threading

_pool_lock = threading.Lock()
_pool_counter = {"i": 0}


def pick_whisper() -> str:
    """Thread-safe round-robin pick from the Whisper pool.

    The previous version stored the counter on the function attribute
    without a lock, which raced under ThreadPoolExecutor and routed
    multiple parallel ASR calls onto the same endpoint — that endpoint
    then 502'd under GPU pressure and the retry loop kept hitting it
    because every retry restarted the round-robin at the same place."""
    with _pool_lock:
        idx = _pool_counter["i"]
        _pool_counter["i"] = (idx + 1) % len(WHISPER_POOL)
        return WHISPER_POOL[idx]


def transcribe(piece_wav: str, language: str = "zh", initial_prompt: str = "普通话") -> str:
    """POST a WAV to one of the Whisper endpoints. Retries across the
    pool on transient failures (502 / connect timeout / connection
    reset). Sleeps briefly between retries so a momentarily-saturated
    endpoint has a chance to recover instead of being hammered."""
    last_err: Exception | None = None
    # Up to len(pool)*2 attempts so each endpoint is tried twice if
    # everyone's having a bad moment.
    for attempt in range(len(WHISPER_POOL) * 2):
        url = pick_whisper()
        try:
            with open(piece_wav, "rb") as f:
                resp = _session.post(
                    f"{url}/transcribe",
                    files={"audio": (os.path.basename(piece_wav), f, "audio/wav")},
                    data={"language": language, "initial_prompt": initial_prompt},
                    timeout=180,
                )
            resp.raise_for_status()
            data = resp.json()
            text = data.get("text", "")
            return text.strip()
        except Exception as err:
            last_err = err
            # Brief back-off — proportional to attempt # so we don't
            # hammer a struggling endpoint pool. First few retries
            # short (other endpoints likely free); later retries longer
            # (everyone's busy, wait it out).
            time.sleep(0.5 + attempt * 0.5)
            continue
    raise RuntimeError(f"all whisper endpoints failed after {len(WHISPER_POOL) * 2} tries: {last_err}")


def transcribe_pieces(input_wav: str, pieces: list[tuple[int, int]], scratch: Path) -> list[dict]:
    """Cut each piece to scratch dir, ASR in parallel, return list of
    {start_ms, end_ms, text}."""
    items: list[dict] = []
    piece_files: list[tuple[int, str, int, int]] = []
    for i, (s, e) in enumerate(pieces):
        p = scratch / f"piece_{i:04d}.wav"
        cut_segment(input_wav, s, e, str(p))
        piece_files.append((i, str(p), s, e))

    def _one(item):
        idx, path, s, e = item
        text = transcribe(path)
        return idx, s, e, text

    # Keep parallelism = pool size so each endpoint sees ~1 in-flight
    # request at a time. Going higher tends to trip 502s when the
    # endpoints share GPU memory.
    workers = max(2, len(WHISPER_POOL))
    with cf.ThreadPoolExecutor(max_workers=workers) as ex:
        for fut in cf.as_completed([ex.submit(_one, it) for it in piece_files]):
            idx, s, e, text = fut.result()
            items.append({"idx": idx, "start_ms": s, "end_ms": e, "text": text})
            if (len(items) % 10) == 0:
                print(f"      ASR {len(items)}/{len(piece_files)} done", flush=True)
    items.sort(key=lambda x: x["idx"])
    return items


def semantic_decide_cuts(pieces: list[dict]) -> list[dict]:
    """Ask Qwen to merge pieces into ≤ MAX_SEGMENT_S segments.

    The LLM picks the SEMANTIC boundaries; we restrict it to pick from
    the existing piece boundaries (so we land on real silences) by
    asking it to return a list of "cut after piece N" decisions.
    """
    max_ms = int(MAX_SEGMENT_S * 1000)
    min_ms = int(MIN_SEGMENT_S * 1000)

    piece_lines = "\n".join(
        f'[{i}] {p["start_ms"] / 1000:6.1f}s-{p["end_ms"] / 1000:6.1f}s '
        f'({(p["end_ms"] - p["start_ms"]) / 1000:.1f}s): {p["text"]}'
        for i, p in enumerate(pieces)
    )

    prompt = f"""你是一个语音切割助手。下面给出一段录音预切的若干片段（每段标了序号、起止时间和转写）。请把相邻片段合并成更大的"段（segment）"，规则：

1. 每个 segment 时长 ≤ {MAX_SEGMENT_S:.0f} 秒（强制），≥ {MIN_SEGMENT_S:.0f} 秒尽量满足但不强制
2. 在语义自然停顿处切（一句话或一段相对独立的发言完整保留，不要拆掉中间）
3. 输出 JSON 数组，按时间顺序，每个元素只含两个字段：
   - "first_piece": 该 segment 包含的第一个片段的 [N]（含）
   - "last_piece": 最后一个片段的 [N]（含）
4. 所有片段必须被覆盖一次（连续、不重叠、不遗漏）
5. 不要输出 JSON 以外的任何文字

预切片段：
{piece_lines}

输出 JSON 数组：
"""

    resp = _session.post(
        f"{OLLAMA_URL}/api/generate",
        json={
            "model": OLLAMA_MODEL,
            "prompt": prompt,
            "stream": False,
            "options": {"num_ctx": NUM_CTX, "temperature": 0.1},
        },
        timeout=600,
    )
    resp.raise_for_status()
    raw = resp.json().get("response", "")

    # Extract the JSON array
    m = re.search(r"\[\s*\{.*?\}\s*\]", raw, re.DOTALL)
    if not m:
        raise RuntimeError(f"LLM response did not contain a JSON array:\n{raw[:500]}")
    cuts = json.loads(m.group(0))

    # Validate + convert to {start_ms, end_ms, piece_idxs}
    result: list[dict] = []
    expected_first = 0
    for c in cuts:
        fp = int(c["first_piece"])
        lp = int(c["last_piece"])
        if fp != expected_first:
            # Try to recover: clamp
            fp = expected_first
        if lp < fp:
            lp = fp
        if lp >= len(pieces):
            lp = len(pieces) - 1
        seg_start = pieces[fp]["start_ms"]
        seg_end = pieces[lp]["end_ms"]
        seg_text = "".join(pieces[i]["text"] for i in range(fp, lp + 1)).strip()
        # If the LLM grouped too much (>max), split it on the nearest piece boundary
        if seg_end - seg_start > max_ms:
            cursor = fp
            while cursor <= lp:
                # Greedy: pack pieces until the next would overflow
                start_p = cursor
                running_end = pieces[cursor]["end_ms"]
                while (
                    cursor + 1 <= lp
                    and pieces[cursor + 1]["end_ms"] - pieces[start_p]["start_ms"] <= max_ms
                ):
                    cursor += 1
                    running_end = pieces[cursor]["end_ms"]
                result.append({
                    "first_piece": start_p,
                    "last_piece": cursor,
                    "start_ms": pieces[start_p]["start_ms"],
                    "end_ms": running_end,
                    "text": "".join(pieces[i]["text"] for i in range(start_p, cursor + 1)).strip(),
                })
                cursor += 1
        else:
            result.append({
                "first_piece": fp,
                "last_piece": lp,
                "start_ms": seg_start,
                "end_ms": seg_end,
                "text": seg_text,
            })
        expected_first = result[-1]["last_piece"] + 1

    # If LLM didn't cover everything, pack the remainder
    while expected_first < len(pieces):
        start_p = expected_first
        cursor = start_p
        while (
            cursor + 1 < len(pieces)
            and pieces[cursor + 1]["end_ms"] - pieces[start_p]["start_ms"] <= max_ms
        ):
            cursor += 1
        result.append({
            "first_piece": start_p,
            "last_piece": cursor,
            "start_ms": pieces[start_p]["start_ms"],
            "end_ms": pieces[cursor]["end_ms"],
            "text": "".join(pieces[i]["text"] for i in range(start_p, cursor + 1)).strip(),
        })
        expected_first = cursor + 1

    return result


def write_project_json(
    project_dir: Path,
    source_wav_basename: str,
    segments: list[dict],
    source_duration_ms: int,
    sample_rate: int,
    channels: int,
) -> None:
    """Mode-1-compatible project.json so the Tauri client can open it."""
    audio_file_id = uuid.uuid4().hex
    project = {
        "version": 2,
        "savedAt": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "rootPath": ".",
        "projectDir": ".",
        "segmentsDir": "./segments",
        "config": {
            # CutConfig — fields the Tauri client expects. Marking mode
            # as "semantic" so opening this in the desktop client picks
            # the Mode 2 strategy panel and avoids re-cutting with
            # silence-based defaults.
            "silenceDb": -30.0,
            "minSilenceMs": 400,
            "minSegmentMs": 300,
            "preRollMs": 100,
            "postRollMs": 200,
            "maxSegmentMs": int(MAX_SEGMENT_S * 1000),
            "mode": "semantic",
            "minSegmentS": int(MIN_SEGMENT_S),
            "maxSegmentS": int(MAX_SEGMENT_S),
            "targetLoudnessLufs": -18,
            "headTailSilenceMs": 150,
            "semanticEndpoint": OLLAMA_URL,
            "semanticModel": OLLAMA_MODEL,
            "semanticNumCtx": NUM_CTX,
        },
        "audioFiles": [
            {
                "id": audio_file_id,
                "path": f"./source/{source_wav_basename}",
                "fileName": source_wav_basename,
                "durationMs": source_duration_ms,
                "sampleRate": sample_rate,
                "channels": channels,
                "matchedEmotion": [],
            }
        ],
        "manifestRecords": [],
        "segments": [
            {
                "id": uuid.uuid4().hex,
                "sourcePath": f"./source/{source_wav_basename}",
                "sourceFileName": source_wav_basename,
                "segmentPath": f"./segments/{seg['filename']}",
                "segmentFileName": seg["filename"],
                "role": None,
                "startMs": seg["start_ms"],
                "endMs": seg["end_ms"],
                "durationMs": seg["end_ms"] - seg["start_ms"],
                # Both fields populated with ASR text so the Tauri
                # client renders them immediately (originalText is the
                # "raw" column, phoneticText the "current" column).
                "originalText": seg["text"],
                "phoneticText": seg["text"],
                "emotion": [],
                "tags": [],
                "notes": "",
            }
            for seg in segments
        ],
    }
    out_path = project_dir / "project.json"
    out_path.write_text(json.dumps(project, ensure_ascii=False, indent=2), encoding="utf-8")


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 1

    input_wav = os.path.abspath(sys.argv[1])
    output_dir = Path(os.path.abspath(sys.argv[2]))
    if not os.path.isfile(input_wav):
        print(f"input not found: {input_wav}", file=sys.stderr)
        return 1
    output_dir.mkdir(parents=True, exist_ok=True)
    segments_dir = output_dir / "segments"
    source_dir = output_dir / "source"
    segments_dir.mkdir(exist_ok=True)
    source_dir.mkdir(exist_ok=True)
    scratch = output_dir / ".pieces"
    scratch.mkdir(exist_ok=True)

    total_ms = duration_ms(input_wav)
    print(f"[input] {input_wav}  ({total_ms / 1000:.1f}s)")

    print(f"[1/5] silence pre-cut...")
    pieces = fine_pre_cut(input_wav)
    print(f"      {len(pieces)} pieces")

    print(f"[2/5] ASR pieces via Whisper pool ({len(WHISPER_POOL)} endpoints)...")
    t0 = time.time()
    transcribed = transcribe_pieces(input_wav, pieces, scratch)
    print(f"      done in {time.time() - t0:.1f}s")
    for p in transcribed[:5]:
        print(f"        [{p['idx']:3d}] {p['start_ms'] / 1000:6.1f}s {p['text'][:60]}")
    if len(transcribed) > 5:
        print(f"        ... +{len(transcribed) - 5} more")

    print(f"[3/5] LLM semantic merge ({OLLAMA_MODEL} @ {OLLAMA_URL}, ≤{MAX_SEGMENT_S:.0f}s)...")
    t0 = time.time()
    decisions = semantic_decide_cuts(transcribed)
    print(f"      {len(decisions)} segments in {time.time() - t0:.1f}s")

    print(f"[4/5] cutting WAV segments...")
    base = Path(input_wav).stem
    final_segments: list[dict] = []
    for i, d in enumerate(decisions):
        fname = f"{base}_{i + 1:04d}_{d['start_ms']}-{d['end_ms']}.wav"
        fpath = segments_dir / fname
        cut_segment(input_wav, d["start_ms"], d["end_ms"], str(fpath))
        final_segments.append({**d, "filename": fname})

    print(f"[5/5] copying source + writing project.json...")
    shutil.copy2(input_wav, source_dir / Path(input_wav).name)
    # Probe sample rate + channels for project.json
    probe = subprocess.check_output(
        [
            "ffprobe",
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels",
            "-of",
            "json",
            input_wav,
        ],
        text=True,
    )
    info: dict[str, Any] = json.loads(probe)["streams"][0]
    write_project_json(
        output_dir,
        Path(input_wav).name,
        final_segments,
        total_ms,
        int(info.get("sample_rate", 44100)),
        int(info.get("channels", 1)),
    )

    # Cleanup scratch pieces (only keep final segments)
    shutil.rmtree(scratch, ignore_errors=True)

    print()
    print(f"DONE → {output_dir}")
    print(f"  source/ : {Path(input_wav).name}")
    print(f"  segments/: {len(final_segments)} files")
    print(f"  total source: {total_ms / 1000:.1f}s")
    durations = [d["end_ms"] - d["start_ms"] for d in decisions]
    print(
        f"  segments: min={min(durations) / 1000:.1f}s  max={max(durations) / 1000:.1f}s  "
        f"avg={sum(durations) / len(durations) / 1000:.1f}s"
    )
    print()
    print("打开 Tauri 客户端 → 「打开」 → 选输出目录里的 project.json，即可开始标注。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
