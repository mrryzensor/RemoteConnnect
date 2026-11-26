import { useEffect, useRef, useState } from "react";
import {
  Clipboard,
  Download,
  FolderOpen,
  Lock,
  Maximize,
  Monitor,
  Moon,
  PhoneOff,
  Sliders,
  Sun,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import {
  DEFAULT_PERMISSIONS,
  formatBytes,
  type ClipboardFileInfo,
  type DisplayInfo,
  type FileEntry,
  type SessionPermissions,
} from "../types";
import { RemoteViewerClient } from "../lib/viewerClient";
import { FileTransferModal } from "./FileTransferModal";

interface Props {
  serverUrl: string;
  targetDeviceId: string;
  viewerName: string;
  pin?: string;
  authToken?: string | null;
  theme: "dark" | "light";
  onToggleTheme: () => void;
  onExit: (message?: string) => void;
}

export function RemoteViewer({
  serverUrl,
  targetDeviceId,
  viewerName,
  pin,
  authToken,
  theme,
  onToggleTheme,
  onExit,
}: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const clientRef = useRef<RemoteViewerClient | null>(null);
  const lastMoveSentRef = useRef(0);

  const [status, setStatus] = useState<
    "connecting" | "waiting_approval" | "connected" | "disconnected"
  >("connecting");
  const [statusDetail, setStatusDetail] = useState("Iniciando conexión...");
  const [hostName, setHostName] = useState(targetDeviceId);
  const [displays, setDisplays] = useState<DisplayInfo[]>([]);
  const [permissions, setPermissions] = useState<SessionPermissions>({
    ...DEFAULT_PERMISSIONS,
  });
  const [selectedDisplay, setSelectedDisplay] = useState(0);
  const [fps, setFps] = useState(20);
  const [quality, setQuality] = useState(65);
  const [scale, setScale] = useState(0.85);
  const [liveFps, setLiveFps] = useState(0);
  const [audioMuted, setAudioMuted] = useState(false);
  const [showSettings, setShowSettings] = useState(false);

  // Remote Ctrl+C copied files notification
  const [remoteCopiedFiles, setRemoteCopiedFiles] = useState<ClipboardFileInfo[]>([]);
  const [viewerToast, setViewerToast] = useState<string | null>(null);

  // File manager state
  const [showFiles, setShowFiles] = useState(false);
  const [currentPath, setCurrentPath] = useState("");
  const [parentPath, setParentPath] = useState<string | null>(null);
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [fileError, setFileError] = useState<string | null>(null);
  const [fileStatus, setFileStatus] = useState<string | null>(null);

  const flashToast = (msg: string) => {
    setViewerToast(msg);
    window.setTimeout(() => setViewerToast(null), 4000);
  };

  useEffect(() => {
    let framesInSecond = 0;
    const fpsTimer = window.setInterval(() => {
      setLiveFps(framesInSecond);
      framesInSecond = 0;
    }, 1000);

    const client = new RemoteViewerClient({
      onStatus: (st, detail) => {
        setStatus(st);
        if (detail) setStatusDetail(detail);
      },
      onSessionReady: (info) => {
        setHostName(info.hostName);
        setDisplays(info.displays);
        setPermissions(info.permissions);
        stageRef.current?.focus();
      },
      onFrame: (bitmap, width, height) => {
        framesInSecond++;
        const canvas = canvasRef.current;
        if (!canvas) {
          bitmap.close();
          return;
        }
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
        }
        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.drawImage(bitmap, 0, 0);
        }
        bitmap.close();
      },
      onClipboard: (text) => {
        if (navigator.clipboard?.writeText) {
          navigator.clipboard.writeText(text).catch(() => {});
        }
      },
      onClipboardFiles: (files) => {
        setRemoteCopiedFiles(files);
      },
      onFileList: (data) => {
        setCurrentPath(data.currentPath);
        setParentPath(data.parentPath);
        setEntries(data.entries);
        setFileError(data.error);
      },
      onFileDownloadComplete: (fileName, blob) => {
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = fileName;
        a.click();
        URL.revokeObjectURL(url);
        flashToast(`Archivo recibido: ${fileName}`);
      },
      onFileStatus: (_ok, message) => {
        setFileStatus(message);
        flashToast(message);
        window.setTimeout(() => setFileStatus(null), 5000);
      },
    });

    clientRef.current = client;
    void client.connect({
      serverUrl,
      targetDeviceId,
      viewerName,
      pin,
      authToken,
    });

    return () => {
      window.clearInterval(fpsTimer);
      client.disconnect();
      clientRef.current = null;
    };
  }, [serverUrl, targetDeviceId, viewerName, pin, authToken]);

  const applyStreamConfig = (
    nextFps: number,
    nextQuality: number,
    nextDisplay: number,
    nextScale: number
  ) => {
    setFps(nextFps);
    setQuality(nextQuality);
    setSelectedDisplay(nextDisplay);
    setScale(nextScale);
    clientRef.current?.sendStreamConfig(
      nextFps,
      nextQuality,
      nextDisplay,
      nextScale
    );
  };

  const getNormalizedCoords = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const y = Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height));
    return { x, y };
  };

  const mapMouseButton = (btn: number): "left" | "right" | "middle" => {
    if (btn === 2) return "right";
    if (btn === 1) return "middle";
    return "left";
  };

  const handleMouseMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (!permissions.allow_input) return;
    const now = performance.now();
    if (now - lastMoveSentRef.current < 25) return;
    lastMoveSentRef.current = now;
    const { x, y } = getNormalizedCoords(e);
    clientRef.current?.sendMouseMove(x, y);
  };

  const handleMouseDown = (e: React.MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    clientRef.current?.resumeAudio();
    stageRef.current?.focus();
    if (!permissions.allow_input) return;
    const { x, y } = getNormalizedCoords(e);
    clientRef.current?.sendMouseButton(mapMouseButton(e.button), true, x, y);
  };

  const handleMouseUp = (e: React.MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    if (!permissions.allow_input) return;
    const { x, y } = getNormalizedCoords(e);
    clientRef.current?.sendMouseButton(mapMouseButton(e.button), false, x, y);
  };

  const handleWheel = (e: React.WheelEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    if (!permissions.allow_input) return;
    const dx = Math.sign(e.deltaX);
    const dy = Math.sign(e.deltaY) * 2;
    clientRef.current?.sendMouseScroll(dx, dy);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (showFiles) return;
    clientRef.current?.resumeAudio();

    if ((e.ctrlKey || e.metaKey) && e.code === "KeyV" && !e.shiftKey) {
      return;
    }

    if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.code === "KeyV") {
      if (remoteCopiedFiles.length > 0 && permissions.allow_file_transfer) {
        e.preventDefault();
        remoteCopiedFiles.forEach((f) =>
          clientRef.current?.requestFileDownload(f.path)
        );
        flashToast(`Descargando ${remoteCopiedFiles.length} archivo(s) del anfitrión...`);
        return;
      }
    }

    if (!permissions.allow_input) return;
    e.preventDefault();
    clientRef.current?.sendKeyEvent(e.code, e.key, true);
  };

  const handleKeyUp = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (showFiles || !permissions.allow_input) return;
    if ((e.ctrlKey || e.metaKey) && e.code === "KeyV" && !e.shiftKey) {
      return;
    }
    e.preventDefault();
    clientRef.current?.sendKeyEvent(e.code, e.key, false);
  };

  const handlePaste = async (e: React.ClipboardEvent<HTMLDivElement>) => {
    if (showFiles) return;
    const files = Array.from(e.clipboardData.files || []);
    if (files.length > 0 && permissions.allow_file_transfer && permissions.allow_clipboard) {
      e.preventDefault();
      for (const file of files) {
        flashToast(`Enviando archivo con Ctrl+V: ${file.name}...`);
        await clientRef.current?.pasteFileToHost(file, true);
      }
      return;
    }

    const text = e.clipboardData.getData("text/plain");
    if (text && permissions.allow_clipboard) {
      e.preventDefault();
      clientRef.current?.sendClipboard(text);
    }
    if (permissions.allow_input) {
      clientRef.current?.sendKeyEvent("ControlLeft", "Control", true);
      clientRef.current?.sendKeyEvent("KeyV", "v", true);
      clientRef.current?.sendKeyEvent("KeyV", "v", false);
      clientRef.current?.sendKeyEvent("ControlLeft", "Control", false);
    }
  };

  const handleDrop = async (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    if (!permissions.allow_file_transfer) {
      flashToast("El anfitrión no habilitó la transferencia de archivos");
      return;
    }
    const files = Array.from(e.dataTransfer.files || []);
    for (const file of files) {
      flashToast(`Enviando archivo al anfitrión: ${file.name}...`);
      await clientRef.current?.pasteFileToHost(file, false);
    }
  };

  const sendLocalClipboard = async () => {
    if (!permissions.allow_clipboard) return;
    try {
      const text = await navigator.clipboard.readText();
      if (text) {
        clientRef.current?.sendClipboard(text);
        flashToast("Portapapeles enviado al equipo remoto");
      }
    } catch {
      // ignore
    }
  };

  const toggleMute = () => {
    const next = !audioMuted;
    setAudioMuted(next);
    clientRef.current?.setAudioMuted(next);
    if (!next) clientRef.current?.resumeAudio();
  };

  const toggleFullscreen = () => {
    if (!document.fullscreenElement) {
      document.documentElement.requestFullscreen().catch(() => {});
    } else {
      document.exitFullscreen().catch(() => {});
    }
  };

  return (
    <div className="viewer-shell">
      <div className="viewer-toolbar">
        <div className="viewer-toolbar-group">
          <Monitor size={18} color="var(--accent)" />
          <strong style={{ fontSize: 14 }}>{hostName}</strong>
          <span className="viewer-badge">
            <Lock size={12} />
            E2E AES-256
          </span>
          {!permissions.allow_input && (
            <span
              className="viewer-badge"
              style={{ background: "var(--warning-soft)", color: "var(--warning-text)" }}
            >
              Solo lectura
            </span>
          )}
          <span style={{ fontSize: 12, color: "var(--text-muted)", fontFamily: "JetBrains Mono, monospace" }}>
            {liveFps} FPS
          </span>
        </div>

        <div className="viewer-toolbar-group">
          {displays.length > 1 && (
            <select
              value={selectedDisplay}
              onChange={(e) =>
                applyStreamConfig(fps, quality, Number(e.target.value), scale)
              }
              style={{ padding: "5px 9px", fontSize: 12 }}
            >
              {displays.map((d, idx) => (
                <option key={d.id} value={idx}>
                  {d.name} ({d.width}x{d.height})
                </option>
              ))}
            </select>
          )}

          {permissions.allow_audio && (
            <button
              className="btn btn-secondary btn-sm"
              onClick={toggleMute}
              title={audioMuted ? "Activar audio remoto" : "Silenciar audio remoto"}
            >
              {audioMuted ? (
                <VolumeX size={14} />
              ) : (
                <Volume2 size={14} color="var(--success)" />
              )}
              {audioMuted ? "Silenciado" : "Audio"}
            </button>
          )}

          <button
            className="btn btn-secondary btn-sm"
            onClick={() => setShowSettings((s) => !s)}
            title="Calidad y FPS"
          >
            <Sliders size={14} />
            Calidad
          </button>

          {permissions.allow_clipboard && (
            <button
              className="btn btn-secondary btn-sm"
              onClick={sendLocalClipboard}
              title="Enviar portapapeles local al equipo remoto"
            >
              <Clipboard size={14} />
              Portapapeles
            </button>
          )}

          {permissions.allow_file_transfer && (
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => {
                setShowFiles(true);
                clientRef.current?.requestFileList(currentPath || undefined);
              }}
            >
              <FolderOpen size={14} />
              Archivos
            </button>
          )}

          <button
            className="icon-btn"
            onClick={onToggleTheme}
            title={theme === "dark" ? "Cambiar a tema claro" : "Cambiar a tema oscuro"}
          >
            {theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
          </button>

          <button
            className="icon-btn"
            onClick={toggleFullscreen}
            title="Pantalla completa"
          >
            <Maximize size={16} />
          </button>

          <button
            className="btn btn-danger btn-sm"
            onClick={() => {
              clientRef.current?.disconnect();
              onExit();
            }}
          >
            <PhoneOff size={14} />
            Desconectar
          </button>
        </div>
      </div>

      {showSettings && (
        <div className="viewer-settings-bar">
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            FPS ({fps}):
            <input
              type="range"
              min={5}
              max={30}
              value={fps}
              onChange={(e) =>
                applyStreamConfig(Number(e.target.value), quality, selectedDisplay, scale)
              }
            />
          </label>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            Calidad JPEG ({quality}%):
            <input
              type="range"
              min={25}
              max={92}
              value={quality}
              onChange={(e) =>
                applyStreamConfig(fps, Number(e.target.value), selectedDisplay, scale)
              }
            />
          </label>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            Escala ({Math.round(scale * 100)}%):
            <input
              type="range"
              min={40}
              max={100}
              step={5}
              value={Math.round(scale * 100)}
              onChange={(e) =>
                applyStreamConfig(
                  fps,
                  quality,
                  selectedDisplay,
                  Number(e.target.value) / 100
                )
              }
            />
          </label>
        </div>
      )}

      <div
        ref={stageRef}
        className="viewer-stage"
        tabIndex={0}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onPaste={handlePaste}
        onDragOver={(e) => e.preventDefault()}
        onDrop={handleDrop}
      >
        <canvas
          ref={canvasRef}
          className="viewer-canvas"
          onMouseMove={handleMouseMove}
          onMouseDown={handleMouseDown}
          onMouseUp={handleMouseUp}
          onWheel={handleWheel}
          onContextMenu={(e) => e.preventDefault()}
        />

        {remoteCopiedFiles.length > 0 && permissions.allow_file_transfer && (
          <div className="clipboard-float-bar">
            <span>
              Copiado en el equipo remoto:{" "}
              <strong>
                {remoteCopiedFiles.length === 1
                  ? `${remoteCopiedFiles[0].name} (${formatBytes(remoteCopiedFiles[0].size)})`
                  : `${remoteCopiedFiles.length} archivos`}
              </strong>
            </span>
            <button
              className="btn btn-primary btn-sm"
              onClick={() => {
                remoteCopiedFiles.forEach((f) =>
                  clientRef.current?.requestFileDownload(f.path)
                );
                setRemoteCopiedFiles([]);
              }}
            >
              <Download size={13} />
              Descargar aquí (Ctrl+Shift+V)
            </button>
            <button
              className="icon-btn"
              onClick={() => setRemoteCopiedFiles([])}
              title="Cerrar"
            >
              <X size={14} />
            </button>
          </div>
        )}

        {status !== "connected" && (
          <div className="viewer-overlay-msg">
            <div style={{ fontSize: 17, fontWeight: 700, color: "#f8fafc" }}>
              {status === "connecting" && "Conectando al servidor Rust..."}
              {status === "waiting_approval" && "Esperando aprobación del equipo remoto..."}
              {status === "disconnected" && "Desconectado"}
            </div>
            <div>{statusDetail}</div>
            {status === "disconnected" && (
              <button className="btn btn-primary" onClick={() => onExit(statusDetail)}>
                Volver al panel
              </button>
            )}
          </div>
        )}
      </div>

      {showFiles && (
        <FileTransferModal
          currentPath={currentPath}
          parentPath={parentPath}
          entries={entries}
          error={fileError}
          statusMsg={fileStatus}
          onNavigate={(path) => clientRef.current?.requestFileList(path)}
          onDownload={(path) => clientRef.current?.requestFileDownload(path)}
          onUpload={async (file, targetDir) => {
            setFileStatus(`Subiendo ${file.name}...`);
            await clientRef.current?.uploadFile(file, targetDir);
            clientRef.current?.requestFileList(targetDir);
          }}
          onClose={() => setShowFiles(false)}
        />
      )}

      {viewerToast && <div className="toast">{viewerToast}</div>}
    </div>
  );
}
