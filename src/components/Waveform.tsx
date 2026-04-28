import { useEffect, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { ipc, clamp } from "../lib";

type WaveformProps = {
  path: string;
  durationMs: number;
  currentMs: number;
  onSeek: (ratio: number) => void;
  height?: number;
};

export function Waveform({
  path,
  durationMs,
  currentMs,
  onSeek,
  height = 80,
}: WaveformProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [peaks, setPeaks] = useState<number[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!path) {
      setPeaks([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    ipc
      .prepareWaveformPeaks({ inputPath: path, bucketCount: 480 })
      .then((data) => {
        if (cancelled) return;
        setPeaks(data);
      })
      .catch(() => {
        if (cancelled) return;
        setPeaks([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) return;
    const dpr = window.devicePixelRatio || 1;
    const rect = container.getBoundingClientRect();
    canvas.width = Math.max(2, Math.floor(rect.width * dpr));
    canvas.height = Math.max(2, Math.floor(height * dpr));
    canvas.style.width = `${rect.width}px`;
    canvas.style.height = `${height}px`;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);

    const styles = getComputedStyle(document.documentElement);
    const colorBg = styles.getPropertyValue("--surface-2") || "#eee";
    const colorFg = styles.getPropertyValue("--accent") || "#0d9488";
    const colorPlayed = styles.getPropertyValue("--accent-soft") || "#cffaf2";
    const colorMuted = styles.getPropertyValue("--text-tertiary") || "#999";

    ctx.fillStyle = colorBg.trim();
    ctx.fillRect(0, 0, rect.width, height);

    if (!peaks.length) {
      ctx.fillStyle = colorMuted.trim();
      ctx.font = "12px var(--font-sans)";
      ctx.textAlign = "center";
      ctx.fillText(loading ? "加载波形…" : "暂无波形", rect.width / 2, height / 2 + 4);
      return;
    }

    const playedRatio = clamp(currentMs / Math.max(durationMs, 1), 0, 1);
    const playedX = playedRatio * rect.width;
    const middle = height / 2;
    const barCount = peaks.length;
    const barW = rect.width / barCount;
    const gap = barW > 3 ? 1 : 0;

    for (let i = 0; i < barCount; i += 1) {
      const peak = peaks[i] ?? 0;
      const h = Math.max(1, Math.min(height, peak * height * 0.92));
      const x = i * barW;
      const isPlayed = x + barW <= playedX;
      const isCursor = x <= playedX && x + barW > playedX;
      ctx.fillStyle = isPlayed
        ? colorFg.trim()
        : isCursor
          ? colorFg.trim()
          : (colorPlayed.trim() || colorMuted.trim());
      ctx.globalAlpha = isPlayed ? 1 : isCursor ? 1 : 0.55;
      ctx.fillRect(x, middle - h / 2, Math.max(1, barW - gap), h);
    }
    ctx.globalAlpha = 1;
  }, [peaks, currentMs, durationMs, height, loading]);

  function handleClick(event: ReactMouseEvent<HTMLDivElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    const ratio = clamp((event.clientX - rect.left) / rect.width, 0, 1);
    onSeek(ratio);
  }

  const cursorLeft = clamp(currentMs / Math.max(durationMs, 1), 0, 1) * 100;

  return (
    <div
      className="waveform-container"
      style={{ height }}
      ref={containerRef}
      onClick={handleClick}
      role="slider"
      tabIndex={0}
      aria-label="音频波形，点击跳转"
    >
      <canvas ref={canvasRef} className="waveform-canvas" />
      <div
        className="waveform-cursor"
        style={{ left: `${cursorLeft}%` }}
      />
      {loading && peaks.length === 0 && (
        <div className="waveform-loading">解析音频中…</div>
      )}
    </div>
  );
}
