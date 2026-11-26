import { useRef } from "react";
import {
  ArrowUp,
  Download,
  File as FileIcon,
  Folder,
  RefreshCw,
  Upload,
  X,
} from "lucide-react";
import { formatBytes, type FileEntry } from "../types";

interface Props {
  currentPath: string;
  parentPath: string | null;
  entries: FileEntry[];
  error: string | null;
  statusMsg: string | null;
  onNavigate: (path?: string) => void;
  onDownload: (path: string) => void;
  onUpload: (file: File, targetDir: string) => void;
  onClose: () => void;
}

export function FileTransferModal({
  currentPath,
  parentPath,
  entries,
  error,
  statusMsg,
  onNavigate,
  onDownload,
  onUpload,
  onClose,
}: Props) {
  const fileInputRef = useRef<HTMLInputElement>(null);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal-card wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3>
            <Folder size={18} color="var(--accent)" />
            Transferencia de archivos (E2E)
          </h3>
          <button className="icon-btn" onClick={onClose}>
            <X size={18} />
          </button>
        </div>

        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <button
            className="btn btn-secondary btn-sm"
            disabled={!parentPath}
            onClick={() => parentPath && onNavigate(parentPath)}
            title="Subir nivel"
          >
            <ArrowUp size={14} />
          </button>
          <input
            style={{ flex: 1, fontSize: 13 }}
            value={currentPath}
            readOnly
            placeholder="Cargando directorio remoto..."
          />
          <button
            className="btn btn-secondary btn-sm"
            onClick={() => onNavigate(currentPath || undefined)}
            title="Actualizar"
          >
            <RefreshCw size={14} />
          </button>
          <button
            className="btn btn-primary btn-sm"
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload size={14} />
            Subir aquí
          </button>
          <input
            ref={fileInputRef}
            type="file"
            style={{ display: "none" }}
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) {
                onUpload(f, currentPath);
                e.target.value = "";
              }
            }}
          />
        </div>

        {error && (
          <div style={{ color: "#f87171", fontSize: 13 }}>{error}</div>
        )}
        {statusMsg && (
          <div style={{ color: "#4ade80", fontSize: 13 }}>{statusMsg}</div>
        )}

        <div className="file-list">
          {entries.length === 0 ? (
            <div
              style={{
                padding: 32,
                textAlign: "center",
                color: "var(--text-muted)",
                fontSize: 13,
              }}
            >
              Directorio vacío o cargando...
            </div>
          ) : (
            entries.map((entry) => (
              <div key={entry.path} className="file-row">
                <div
                  className={`file-name ${entry.is_dir ? "dir" : ""}`}
                  onClick={() => entry.is_dir && onNavigate(entry.path)}
                  title={entry.name}
                >
                  {entry.is_dir ? (
                    <Folder size={15} color="var(--accent)" />
                  ) : (
                    <FileIcon size={15} color="var(--text-muted)" />
                  )}
                  <span>{entry.name}</span>
                </div>
                <div style={{ color: "var(--text-secondary)", textAlign: "right" }}>
                  {entry.is_dir ? "Carpeta" : formatBytes(entry.size)}
                </div>
                <div style={{ color: "var(--text-muted)", fontSize: 12 }}>
                  {entry.modified ?? ""}
                </div>
                <div>
                  {!entry.is_dir && (
                    <button
                      className="btn btn-secondary btn-sm"
                      onClick={() => onDownload(entry.path)}
                    >
                      <Download size={13} />
                    </button>
                  )}
                </div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
