import { useMemo, useState } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  CircleDashed,
  Folder,
  Pause,
  Scissors,
  Search,
  Volume2,
} from "lucide-react";
import type {
  AudioFileInfo,
  ProjectScan,
  SegmentRecord,
} from "../types";
import { roleLabels } from "../defaults";
import { formatClock, formatDuration, formatMsRange } from "../lib";
import { Waveform } from "./Waveform";
import { EmptyState } from "./EmptyState";

type MainViewProps = {
  scan: ProjectScan | null;
  segments: SegmentRecord[];
  selectedAudio: AudioFileInfo | null;
  selectedAudioId: string;
  selectedSegmentId: string;
  visibleSegments: SegmentRecord[];
  busy: boolean;
  isPlaying: boolean;
  playbackPath: string;
  playbackStatus: string;
  playbackCurrentMs: number;
  playbackDurationMs: number;
  onSelectAudio: (audio: AudioFileInfo) => void;
  onCutOne: (audio: AudioFileInfo) => void;
  onSelectSegment: (segment: SegmentRecord) => void;
  onTogglePlay: () => void;
  onSeekRatio: (ratio: number) => void;
};

export function MainView(props: MainViewProps) {
  const [audioQuery, setAudioQuery] = useState("");
  const [segmentQuery, setSegmentQuery] = useState("");

  const filteredAudio = useMemo(() => {
    if (!props.scan) return [];
    if (!audioQuery.trim()) return props.scan.audioFiles;
    const q = audioQuery.toLowerCase();
    return props.scan.audioFiles.filter((audio) =>
      audio.fileName.toLowerCase().includes(q),
    );
  }, [props.scan, audioQuery]);

  const filteredSegments = useMemo(() => {
    if (!segmentQuery.trim()) return props.visibleSegments;
    const q = segmentQuery.toLowerCase();
    return props.visibleSegments.filter(
      (s) =>
        s.segmentFileName.toLowerCase().includes(q) ||
        s.phoneticText.toLowerCase().includes(q) ||
        s.originalText.toLowerCase().includes(q),
    );
  }, [props.visibleSegments, segmentQuery]);

  const segmentStats = useMemo(() => {
    const total = props.visibleSegments.length;
    const done = props.visibleSegments.filter((s) =>
      s.phoneticText.trim(),
    ).length;
    return { total, done, percent: total ? Math.round((done / total) * 100) : 0 };
  }, [props.visibleSegments]);

  // Two-stage progress per audio file:
  //   asrDone    — segments with non-empty phoneticText (Whisper landed)
  //   polishDone — segments with a non-empty emotion array (Ollama polished)
  // Shown as two badges on each row so users can tell at a glance whether
  // an audio file is half-done (Whisper yes, polish no) vs fully done.
  const statsByAudio = useMemo(() => {
    const map = new Map<
      string,
      { total: number; asrDone: number; polishDone: number }
    >();
    for (const segment of props.segments) {
      const entry =
        map.get(segment.sourcePath) ?? { total: 0, asrDone: 0, polishDone: 0 };
      entry.total += 1;
      if (segment.phoneticText.trim()) entry.asrDone += 1;
      if (segment.emotion.length > 0) entry.polishDone += 1;
      map.set(segment.sourcePath, entry);
    }
    return map;
  }, [props.segments]);

  if (!props.scan) {
    return (
      <section className="card" style={{ flex: 1, display: "grid" }}>
        <EmptyState
          icon={<Folder size={28} />}
          title="还没有打开项目"
          description={
            <>
              选一个含音频文件的文件夹，或拖拽文件夹到窗口任意位置。
              扫描后系统会按静音切片，调用 Whisper（可选 Ollama 改写）生成识别初稿。
            </>
          }
        />
      </section>
    );
  }

  return (
    <section className="workspace">
      <aside className="pane">
        <div className="card-head">
          <h2>
            原音频
            <span className="badge-count">{props.scan.audioFiles.length}</span>
          </h2>
        </div>
        <div className="pane-search">
          <div className="search-input">
            <Search size={14} />
            <input
              value={audioQuery}
              onChange={(event) => setAudioQuery(event.target.value)}
              placeholder="按文件名搜索…"
            />
          </div>
        </div>
        <div className="pane-body">
          {filteredAudio.length === 0 && (
            <EmptyState
              icon={<Search size={20} />}
              title="未找到匹配的音频"
              description="尝试更换搜索词"
            />
          )}
          {filteredAudio.map((audio) => {
            const stats = statsByAudio.get(audio.path);
            const isCut = !!stats;
            const asrFullyDone =
              !!stats && stats.asrDone === stats.total && stats.total > 0;
            const polishFullyDone =
              !!stats && stats.polishDone === stats.total && stats.total > 0;
            return (
              <button
                key={audio.id}
                className={`audio-row ${audio.id === props.selectedAudioId ? "active" : ""}`}
                onClick={() => props.onSelectAudio(audio)}
                title={audio.path}
              >
                <strong>{audio.fileName}</strong>
                <div className="audio-row-meta">
                  <span className={`badge-role ${audio.role ?? "unknown"}`}>
                    {roleLabels[audio.role ?? "unknown"]}
                  </span>
                  <span className="dot">·</span>
                  <span>{formatDuration(audio.durationMs)}</span>
                  <span className="dot">·</span>
                  <span>{audio.sampleRate ? `${audio.sampleRate}Hz` : "—"}</span>
                  <span className="dot">·</span>
                  <span>{audio.channels ? `${audio.channels}ch` : "—"}</span>
                  {isCut && (
                    <>
                      <span className="dot">·</span>
                      <span
                        className={`segment-tag ${asrFullyDone ? "tag-done" : "tag-cut"}`}
                        title={`Whisper 已识别 ${stats.asrDone}/${stats.total} 段`}
                      >
                        {asrFullyDone ? (
                          <CheckCircle2 size={11} />
                        ) : (
                          <CircleDashed size={11} />
                        )}
                        ASR {stats.asrDone}/{stats.total}
                      </span>
                      <span
                        className={`segment-tag ${polishFullyDone ? "tag-done" : "tag-cut"}`}
                        title={`Ollama 已改写 ${stats.polishDone}/${stats.total} 段`}
                      >
                        {polishFullyDone ? (
                          <CheckCircle2 size={11} />
                        ) : (
                          <CircleDashed size={11} />
                        )}
                        AI {stats.polishDone}/{stats.total}
                      </span>
                    </>
                  )}
                </div>
                <span className="audio-row-action">
                  <button
                    className="btn-ghost"
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onCutOne(audio);
                    }}
                    disabled={props.busy}
                    title={
                      isCut
                        ? "重新切割（覆盖现有片段）"
                        : "按当前静音参数切割此音频"
                    }
                  >
                    <Scissors size={13} />
                    {isCut ? "重切" : "切割"}
                  </button>
                </span>
              </button>
            );
          })}
        </div>
      </aside>

      <section className="pane">
        <div className="card-head">
          <h2>
            切割片段
            <span className="badge-count">
              {segmentStats.done}/{segmentStats.total}
            </span>
          </h2>
        </div>
        <div className="player-bar">
          <div className="player-bar-top">
            <button
              className="transport-button"
              onClick={props.onTogglePlay}
              disabled={!props.playbackPath}
              aria-label={props.isPlaying ? "暂停原音频" : "播放原音频"}
            >
              {props.isPlaying ? <Pause size={16} /> : <Volume2 size={16} />}
            </button>
            <div className="player-bar-info">
              <strong>
                {props.selectedAudio?.fileName ?? "未选择原音频"}
              </strong>
              <span>
                {props.playbackStatus ||
                  formatClock(props.playbackDurationMs || props.selectedAudio?.durationMs)}
              </span>
            </div>
            <span className="player-bar-time">
              {formatClock(props.playbackCurrentMs)} /{" "}
              {formatClock(
                props.playbackDurationMs ||
                  props.selectedAudio?.durationMs ||
                  0,
              )}
            </span>
          </div>
          <Waveform
            path={props.playbackPath}
            durationMs={
              props.playbackDurationMs ||
              props.selectedAudio?.durationMs ||
              0
            }
            currentMs={props.playbackCurrentMs}
            onSeek={props.onSeekRatio}
            height={56}
          />
          <div className="completion-track">
            <div
              className="completion-fill"
              style={{ width: `${segmentStats.percent}%` }}
            />
          </div>
        </div>
        <div className="pane-search">
          <div className="search-input">
            <Search size={14} />
            <input
              value={segmentQuery}
              onChange={(event) => setSegmentQuery(event.target.value)}
              placeholder="搜索片段文本或文件名…"
            />
          </div>
        </div>
        <div className="pane-body">
          {filteredSegments.length === 0 && (
            <EmptyState
              icon={<Scissors size={20} />}
              title={
                props.visibleSegments.length === 0
                  ? "尚未切割"
                  : "未找到匹配的片段"
              }
              description={
                props.visibleSegments.length === 0
                  ? "选择左侧的音频后点 “切割”，或顶部 “全部切割”"
                  : "试试其他搜索词"
              }
            />
          )}
          {filteredSegments.map((segment, index) => {
            const tooLong = segment.durationMs > 30_000;
            return (
              <button
                key={segment.id}
                className={`segment-row ${segment.id === props.selectedSegmentId ? "active" : ""}`}
                onClick={() => props.onSelectSegment(segment)}
                title={
                  tooLong
                    ? `${segment.segmentFileName} · 时长 ${(segment.durationMs / 1000).toFixed(1)}s 超过 30s 上限，建议手动拆分或调小切割阈值`
                    : segment.segmentFileName
                }
              >
                <span className="segment-index">
                  {String(index + 1).padStart(2, "0")}
                </span>
                <span
                  className={`segment-text ${segment.phoneticText ? "" : "empty"}`}
                >
                  {segment.phoneticText ||
                    segment.originalText ||
                    "未标注"}
                </span>
                <span className="segment-time">
                  {tooLong && (
                    <AlertTriangle
                      size={12}
                      style={{
                        color: "var(--warning)",
                        verticalAlign: "-2px",
                        marginRight: 4,
                      }}
                    />
                  )}
                  {formatMsRange(segment.startMs, segment.endMs)}
                </span>
                <div className="segment-meta-row">
                  {tooLong && (
                    <span className="segment-tag tag-overlong">
                      &gt;30s
                    </span>
                  )}
                  {segment.emotion.map((e) => (
                    <span className="segment-tag emotion" key={e}>
                      {e}
                    </span>
                  ))}
                  {segment.tags.map((t) => (
                    <span className={`segment-tag tag-${t}`} key={t}>
                      {t}
                    </span>
                  ))}
                </div>
              </button>
            );
          })}
        </div>
      </section>

      <aside className="pane info-pane">
        <div className="card-head">
          <h2>
            原文 / 元信息
            {props.selectedAudio && (
              <span
                className={`badge-role ${props.selectedAudio.role ?? "unknown"}`}
              >
                {roleLabels[props.selectedAudio.role ?? "unknown"]}
              </span>
            )}
          </h2>
        </div>
        <div className="info-pane-body pane-body">
          {props.selectedAudio?.matchedText ? (
            <p>{props.selectedAudio.matchedText}</p>
          ) : (
            <p className="info-warning">
              当前音频没有 manifest 文本。Whisper 识别初稿后会自动填入。
            </p>
          )}
          {props.selectedAudio?.matchedEmotion?.length ? (
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
              {props.selectedAudio.matchedEmotion.map((e) => (
                <span className="segment-tag emotion" key={e}>
                  {e}
                </span>
              ))}
            </div>
          ) : null}
          <dl className="info-stat-grid">
            <dt>识别状态</dt>
            <dd>
              {segmentStats.done}/{segmentStats.total} ·{" "}
              {segmentStats.percent}%
            </dd>
            <dt>采样率</dt>
            <dd>
              {props.selectedAudio?.sampleRate
                ? `${props.selectedAudio.sampleRate} Hz`
                : "—"}
            </dd>
            <dt>声道</dt>
            <dd>{props.selectedAudio?.channels ?? "—"}</dd>
            <dt>编码</dt>
            <dd>{props.selectedAudio?.codecName ?? "—"}</dd>
            <dt>位深</dt>
            <dd>
              {props.selectedAudio?.bitsPerSample
                ? `${props.selectedAudio.bitsPerSample}-bit`
                : "—"}
            </dd>
            <dt>路径</dt>
            <dd>{props.selectedAudio?.path ?? "—"}</dd>
            <dt>输出目录</dt>
            <dd>{props.scan.projectDir}</dd>
          </dl>
        </div>
      </aside>
    </section>
  );
}
