import { FileJson, FolderOpen, ScanSearch } from "lucide-react";
import { REVIEW_ONLY } from "../env";

type SetupBandProps = {
  folderPath: string;
  outputPath: string;
  exportJsonlPath: string;
  autoCutAfterScan: boolean;
  busy: boolean;
  onFolderPathChange: (value: string) => void;
  onOutputPathChange: (value: string) => void;
  onExportPathChange: (value: string) => void;
  onChooseFolder: () => void;
  onChooseOutput: () => void;
  onChooseExport: () => void;
  onAutoCutChange: (value: boolean) => void;
  onScan: () => void;
};

export function SetupBand(props: SetupBandProps) {
  return (
    <section className="card">
      <div className="setup-grid">
        <label>
          <span>输入文件夹</span>
          <div className="path-row">
            <input
              value={props.folderPath}
              onChange={(event) => props.onFolderPathChange(event.target.value)}
              placeholder="选择包含音频的文件夹"
            />
            <button onClick={props.onChooseFolder} disabled={props.busy}>
              <FolderOpen size={14} />
              选择
            </button>
          </div>
        </label>
        <label>
          <span>输出文件夹</span>
          <div className="path-row">
            <input
              value={props.outputPath}
              onChange={(event) => props.onOutputPathChange(event.target.value)}
              placeholder="默认：输入文件夹/_dialect_labeler"
            />
            <button onClick={props.onChooseOutput} disabled={props.busy}>
              <FolderOpen size={14} />
              选择
            </button>
          </div>
        </label>
        <label>
          <span>导出 JSONL</span>
          <div className="path-row">
            <input
              value={props.exportJsonlPath}
              onChange={(event) => props.onExportPathChange(event.target.value)}
              placeholder="默认：输出文件夹/export.jsonl"
            />
            <button onClick={props.onChooseExport} disabled={props.busy}>
              <FileJson size={14} />
              选择
            </button>
          </div>
        </label>
        {!REVIEW_ONLY && (
          <label className="checkbox-field">
            <input
              type="checkbox"
              checked={props.autoCutAfterScan}
              onChange={(event) => props.onAutoCutChange(event.target.checked)}
            />
            <span>扫描后自动切割</span>
          </label>
        )}
        <button
          className="btn-primary"
          onClick={props.onScan}
          disabled={props.busy || !props.folderPath.trim()}
        >
          <ScanSearch size={14} />
          扫描
        </button>
      </div>
    </section>
  );
}
