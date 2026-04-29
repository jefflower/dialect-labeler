import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { MouseEvent as ReactMouseEvent } from "react";
import { ipc, clamp, formatClock } from "../lib";

const BASE_PIXELS_PER_SECOND = 100;
const MIN_CHAR_WIDTH = 22;
const ROW_HEIGHT_WAVEFORM = 64;

/**
 * Figure out the px-per-second used for the whole timeline. We bump it up so
 * even the densest stretch of characters has at least MIN_CHAR_WIDTH between
 * neighbors — this preserves perfect time-axis alignment without overlap.
 */
function pickPixelsPerSecond(cellCount: number, durationMs: number): number {
  if (durationMs <= 0 || cellCount <= 0) return BASE_PIXELS_PER_SECOND;
  const charsPerSec = cellCount / (durationMs / 1000);
  const required = charsPerSec * MIN_CHAR_WIDTH;
  return Math.max(BASE_PIXELS_PER_SECOND, Math.ceil(required));
}

export type SelectionRange = {
  startFlat: number;
  endFlat: number;
  /** Inclusive byte-position in the raw (with-tags) text. */
  startRaw: number;
  /** Exclusive byte-position in the raw text — pass directly to slice(end). */
  endRaw: number;
  /**
   * Tags that wrap *every* selected character (intersection of activeTags).
   * If non-empty, the toolbar can offer "remove tag" instead of "add tag".
   */
  commonTags: string[];
};

type AlignedTimelineProps = {
  audioPath: string;
  durationMs: number;
  currentMs: number;
  text: string;
  onSeek: (ratio: number) => void;
  onSelectionChange?: (range: SelectionRange | null) => void;
};

type CharCell = {
  /** Display character (already stripped of inline tags). */
  char: string;
  /** Length of this token in the raw text (1 for chars, full token len for self-closing tags). */
  rawLength: number;
  /** Index into the *raw* (with-tags) text — used for editing operations. */
  rawIndex: number;
  /** 0-based index into the stripped text (for selection ranges). */
  flatIndex: number;
  /** Active inline tags wrapping this char. */
  activeTags: string[];
  /** Ratio (0..1) of this char's centre across the audio timeline. */
  timeRatio: number;
};

type Row = {
  startMs: number;
  endMs: number;
  width: number;
  chars: CharCell[];
};

/**
 * Glyph shown for `[xxx]` bracket markers and legacy `<xxx/>` self-closing
 * markers. Keep them short (1 char) so the visual row stays compact.
 */
const BRACKET_GLYPH: Record<string, string> = {
  laugh: "笑",
  breath: "气",
  cough: "咳",
  clucking: "咯",
  hissing: "嘘",
  sigh: "叹",
  lipsmack: "唇",
  swallowing: "吞",
  pause: "顿",
};

/**
 * Walk the raw text once, emitting a CharCell per visible token and tracking
 * which inline tags wrap each character. The lexer recognises three token
 * shapes:
 *   • `<name>` opening tag       — push name to stack
 *   • `</name>` closing tag      — pop name from stack
 *   • `<name/>` self-closing     — emit a discrete cell tagged with name
 *                                   (legacy, equivalent to `[name]`)
 *   • `[name]` bracket marker    — emit a discrete cell tagged with name
 */
function parseAlignedText(text: string): CharCell[] {
  const cells: CharCell[] = [];
  const tagRe =
    /<\/?([a-zA-Z][a-zA-Z0-9-]*)\s*\/?>|\[([a-zA-Z][a-zA-Z0-9-]*)\]/g;
  const stack: string[] = [];
  let raw = 0;
  let flat = 0;
  let match: RegExpExecArray | null;

  const emitDiscrete = (tagName: string, tokenLen: number) => {
    const glyph =
      BRACKET_GLYPH[tagName.toLowerCase()] ??
      tagName.charAt(0).toUpperCase();
    cells.push({
      char: glyph,
      rawLength: tokenLen,
      rawIndex: raw,
      flatIndex: flat++,
      activeTags: [...stack, tagName.toLowerCase()],
      timeRatio: 0,
    });
  };

  while ((match = tagRe.exec(text)) !== null) {
    const before = text.slice(raw, match.index);
    for (const ch of Array.from(before)) {
      cells.push({
        char: ch,
        rawLength: ch.length,
        rawIndex: raw,
        flatIndex: flat++,
        activeTags: [...stack],
        timeRatio: 0,
      });
      raw += ch.length;
    }
    const tagToken = match[0];
    const tagName = (match[1] ?? match[2] ?? "").toLowerCase();
    if (match[2] !== undefined) {
      // [name] bracket form — discrete event marker
      emitDiscrete(tagName, tagToken.length);
    } else if (tagToken.startsWith("</")) {
      const idx = stack.lastIndexOf(tagName);
      if (idx >= 0) stack.splice(idx, 1);
    } else if (tagToken.endsWith("/>")) {
      // Legacy self-closing — render same as bracket form.
      emitDiscrete(tagName, tagToken.length);
    } else {
      stack.push(tagName);
    }
    raw += tagToken.length;
  }
  for (const ch of Array.from(text.slice(raw))) {
    cells.push({
      char: ch,
      rawLength: ch.length,
      rawIndex: raw,
      flatIndex: flat++,
      activeTags: [...stack],
      timeRatio: 0,
    });
    raw += ch.length;
  }

  const total = cells.length;
  if (total > 0) {
    for (let i = 0; i < total; i += 1) {
      cells[i].timeRatio = (i + 0.5) / total;
    }
  }
  return cells;
}

/** Build wrapped rows given total duration, container width, and px/sec. */
function buildRows(
  durationMs: number,
  containerWidth: number,
  pixelsPerSecond: number,
  cells: CharCell[],
): Row[] {
  if (durationMs <= 0 || containerWidth <= 0) return [];
  const rowDurationMs = Math.max(
    2000,
    Math.floor((containerWidth / pixelsPerSecond) * 1000),
  );
  const rowCount = Math.max(1, Math.ceil(durationMs / rowDurationMs));
  const rows: Row[] = [];
  for (let i = 0; i < rowCount; i += 1) {
    const startMs = i * rowDurationMs;
    const endMs = Math.min(durationMs, startMs + rowDurationMs);
    const span = Math.max(1, endMs - startMs);
    const width = Math.max(160, Math.round((span / 1000) * pixelsPerSecond));
    rows.push({ startMs, endMs, width, chars: [] });
  }
  // Place each cell into its row by timestamp ratio (scaled to durationMs).
  for (const cell of cells) {
    const tMs = cell.timeRatio * durationMs;
    const rowIdx = Math.min(rows.length - 1, Math.floor(tMs / rowDurationMs));
    rows[rowIdx].chars.push(cell);
  }
  return rows;
}

export function AlignedTimeline({
  audioPath,
  durationMs,
  currentMs,
  text,
  onSeek,
  onSelectionChange,
}: AlignedTimelineProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerWidth, setContainerWidth] = useState(0);
  const [peaks, setPeaks] = useState<number[]>([]);
  const [peaksLoading, setPeaksLoading] = useState(false);
  const [selection, setSelection] = useState<{ start: number; end: number } | null>(null);
  const dragRef = useRef<{ startFlat: number } | null>(null);

  // Track container width so wrapping recalculates on resize.
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => setContainerWidth(el.clientWidth);
    update();
    const obs = new ResizeObserver(update);
    obs.observe(el);
    return () => obs.disconnect();
  }, []);

  // Fetch peaks once per audio path.
  useEffect(() => {
    if (!audioPath) {
      setPeaks([]);
      return;
    }
    let cancelled = false;
    setPeaksLoading(true);
    ipc
      .prepareWaveformPeaks({ inputPath: audioPath, bucketCount: 1024 })
      .then((data) => !cancelled && setPeaks(data))
      .catch(() => !cancelled && setPeaks([]))
      .finally(() => !cancelled && setPeaksLoading(false));
    return () => {
      cancelled = true;
    };
  }, [audioPath]);

  const cells = useMemo(() => parseAlignedText(text), [text]);

  const pixelsPerSecond = useMemo(
    () => pickPixelsPerSecond(cells.length, durationMs),
    [cells.length, durationMs],
  );

  const rows = useMemo(
    () => buildRows(durationMs, containerWidth, pixelsPerSecond, cells),
    [durationMs, containerWidth, pixelsPerSecond, cells],
  );

  // Reset selection when the underlying text changes (avoid pointing at
  // stale char indices after edits).
  useEffect(() => {
    setSelection(null);
    onSelectionChange?.(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  const reportSelection = useCallback(
    (next: { start: number; end: number } | null) => {
      setSelection(next);
      if (!next) {
        onSelectionChange?.(null);
        return;
      }
      const startCell = cells[next.start];
      const endCell = cells[next.end];
      if (!startCell || !endCell) {
        onSelectionChange?.(null);
        return;
      }
      // Compute the intersection of activeTags across every selected cell —
      // this is what `commonTags` exposes so the toolbar / shortcut layer
      // can offer a toggle (wrap vs. unwrap) instead of always wrapping.
      const slice = cells.slice(next.start, next.end + 1);
      let common: string[] = slice.length > 0 ? [...slice[0].activeTags] : [];
      for (let i = 1; i < slice.length && common.length > 0; i += 1) {
        common = common.filter((tag) => slice[i].activeTags.includes(tag));
      }
      onSelectionChange?.({
        startFlat: next.start,
        endFlat: next.end,
        startRaw: startCell.rawIndex,
        endRaw: endCell.rawIndex + endCell.rawLength,
        commonTags: common,
      });
    },
    [cells, onSelectionChange],
  );

  function handleCharMouseDown(flatIndex: number, event: ReactMouseEvent) {
    event.preventDefault();
    dragRef.current = { startFlat: flatIndex };
    reportSelection({ start: flatIndex, end: flatIndex });
  }

  function handleCharMouseEnter(flatIndex: number) {
    if (!dragRef.current) return;
    const start = Math.min(dragRef.current.startFlat, flatIndex);
    const end = Math.max(dragRef.current.startFlat, flatIndex);
    reportSelection({ start, end });
  }

  useEffect(() => {
    function up() {
      dragRef.current = null;
    }
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);

  function handleCharClick(cell: CharCell) {
    if (durationMs <= 0) return;
    onSeek(clamp(cell.timeRatio, 0, 1));
  }

  function handleWaveformClick(event: ReactMouseEvent<HTMLDivElement>, row: Row) {
    const rect = event.currentTarget.getBoundingClientRect();
    const localRatio = clamp((event.clientX - rect.left) / rect.width, 0, 1);
    const targetMs = row.startMs + localRatio * (row.endMs - row.startMs);
    onSeek(clamp(targetMs / Math.max(durationMs, 1), 0, 1));
  }

  return (
    <div className="aligned-timeline" ref={containerRef}>
      {peaksLoading && <div className="aligned-empty">解析音频波形…</div>}
      {!peaksLoading && rows.length === 0 && (
        <div className="aligned-empty">无可视化数据（音频时长为 0）</div>
      )}
      <div className="aligned-rows">
        {rows.map((row) => (
          <AlignedRow
            key={row.startMs}
            row={row}
            durationMs={durationMs}
            currentMs={currentMs}
            peaks={peaks}
            selection={selection}
            onWaveformClick={handleWaveformClick}
            onCharClick={handleCharClick}
            onCharMouseDown={handleCharMouseDown}
            onCharMouseEnter={handleCharMouseEnter}
          />
        ))}
      </div>
    </div>
  );
}

type AlignedRowProps = {
  row: Row;
  durationMs: number;
  currentMs: number;
  peaks: number[];
  selection: { start: number; end: number } | null;
  onWaveformClick: (event: ReactMouseEvent<HTMLDivElement>, row: Row) => void;
  onCharClick: (cell: CharCell) => void;
  onCharMouseDown: (flatIndex: number, event: ReactMouseEvent) => void;
  onCharMouseEnter: (flatIndex: number) => void;
};

function AlignedRow({
  row,
  durationMs,
  currentMs,
  peaks,
  selection,
  onWaveformClick,
  onCharClick,
  onCharMouseDown,
  onCharMouseEnter,
}: AlignedRowProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useLayoutEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || row.width <= 0) return;
    const dpr = window.devicePixelRatio || 1;
    const w = row.width;
    const h = ROW_HEIGHT_WAVEFORM;
    canvas.width = Math.floor(w * dpr);
    canvas.height = Math.floor(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);

    const styles = getComputedStyle(document.documentElement);
    const colorBg = styles.getPropertyValue("--surface-2") || "#eee";
    const colorFg = styles.getPropertyValue("--accent") || "#0d9488";
    const colorMuted = styles.getPropertyValue("--text-tertiary") || "#999";

    ctx.fillStyle = colorBg.trim();
    ctx.fillRect(0, 0, w, h);

    if (peaks.length === 0) {
      ctx.fillStyle = colorMuted.trim();
      ctx.font = "11px var(--font-sans)";
      ctx.textAlign = "center";
      ctx.fillText("加载中…", w / 2, h / 2 + 4);
      return;
    }

    // Slice peaks by row time range.
    const startRatio = row.startMs / Math.max(durationMs, 1);
    const endRatio = row.endMs / Math.max(durationMs, 1);
    const startIdx = Math.floor(startRatio * peaks.length);
    const endIdx = Math.max(startIdx + 1, Math.ceil(endRatio * peaks.length));
    const slice = peaks.slice(startIdx, endIdx);

    const middle = h / 2;
    const barCount = slice.length;
    const barW = w / Math.max(barCount, 1);
    const playedX = clamp(
      ((currentMs - row.startMs) / Math.max(row.endMs - row.startMs, 1)) * w,
      0,
      w,
    );

    for (let i = 0; i < barCount; i += 1) {
      const peak = slice[i];
      const barH = Math.max(1, peak * h * 0.92);
      const x = i * barW;
      const isPlayed = x + barW <= playedX;
      ctx.fillStyle = colorFg.trim();
      ctx.globalAlpha = isPlayed ? 1 : 0.35;
      ctx.fillRect(x, middle - barH / 2, Math.max(1, barW - 0.5), barH);
    }
    ctx.globalAlpha = 1;
  }, [row, peaks, currentMs, durationMs]);

  const cursorVisible = currentMs >= row.startMs && currentMs <= row.endMs;
  const cursorLeft = cursorVisible
    ? clamp(
        ((currentMs - row.startMs) / Math.max(row.endMs - row.startMs, 1)) *
          row.width,
        0,
        row.width,
      )
    : -10;

  // Distribute chars across the row width by their timeRatio. Note: we DO
  // NOT do min-pitch repositioning anymore — pixelsPerSecond is sized so the
  // densest stretch of cells already has enough room. Keeping x = timeRatio *
  // width ensures a perfect 1:1 between waveform x-coordinate and char
  // x-coordinate, which the user explicitly asked for.
  const rowSpanMs = Math.max(row.endMs - row.startMs, 1);
  const charPositions = row.chars.map((cell) => {
    const tMs = cell.timeRatio * durationMs;
    const localRatio = clamp((tMs - row.startMs) / rowSpanMs, 0, 1);
    return { cell, x: localRatio * row.width };
  });

  return (
    <div className="aligned-row" style={{ width: row.width }}>
      <div
        className="aligned-waveform"
        onClick={(event) => onWaveformClick(event, row)}
      >
        <canvas ref={canvasRef} className="aligned-waveform-canvas" />
        {cursorVisible && (
          <div
            className="aligned-waveform-cursor"
            style={{ left: `${cursorLeft}px` }}
          />
        )}
        <div className="aligned-time-axis">
          <span>{formatClock(row.startMs)}</span>
          <span>{formatClock(row.endMs)}</span>
        </div>
      </div>
      <div className="aligned-chars" style={{ height: 28 }}>
        {charPositions.map(({ cell, x }) => {
          const isSelected =
            !!selection &&
            cell.flatIndex >= selection.start &&
            cell.flatIndex <= selection.end;
          const tagClasses = cell.activeTags
            .map((tag) => `tag-${tag}`)
            .join(" ");
          const isCursorChar =
            currentMs >= row.startMs &&
            currentMs <= row.endMs &&
            Math.abs(cell.timeRatio * durationMs - currentMs) <
              Math.max(50, durationMs / 60);
          return (
            <span
              key={cell.flatIndex}
              className={`aligned-char ${tagClasses} ${
                isSelected ? "selected" : ""
              } ${isCursorChar ? "playing" : ""}`}
              style={{ left: `${x}px` }}
              onMouseDown={(event) => onCharMouseDown(cell.flatIndex, event)}
              onMouseEnter={() => onCharMouseEnter(cell.flatIndex)}
              onClick={() => onCharClick(cell)}
              title={`${cell.char} · ${formatClock(cell.timeRatio * durationMs)}${
                cell.activeTags.length ? ` · ${cell.activeTags.join("/")}` : ""
              }`}
            >
              {cell.char}
            </span>
          );
        })}
      </div>
    </div>
  );
}
