import { useEffect, useRef, useState } from "react";
import {
  Activity,
  ChevronDown,
  Circle,
  Clipboard,
  Download,
  FolderOpen,
  Lock,
  Maximize,
  Minimize2,
  Monitor,
  Moon,
  PhoneOff,
  Sliders,
  Square,
  Sun,
  Volume2,
  VolumeX,
  X,
  Zap,
} from "lucide-react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isTauriEnv } from "../lib/tauriBridge";
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
  onConnected?: (info: { deviceId: string; hostName: string; pin?: string }) => void;
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
  onConnected,
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
  const [fps, setFps] = useState(30);
  const [bitrateKbps, setBitrateKbps] = useState(2500);
  const [scale, setScale] = useState(1.0);
  const [liveFps, setLiveFps] = useState(0);
  const [liveKbps, setLiveKbps] = useState(0);
  const [rttMs, setRttMs] = useState<number | null>(null);
  const [audioMuted, setAudioMuted] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const [toolbarRevealed, setToolbarRevealed] = useState(false);
  const hideToolbarTimerRef = useRef<number | null>(null);

  // Local-only session recording (never sent to or stored on server)
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recordedChunksRef = useRef<Blob[]>([]);
  const [isRecording, setIsRecording] = useState(false);
  const [recordSeconds, setRecordSeconds] = useState(0);

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
    let bytesInSecond = 0;
    const statsTimer = window.setInterval(() => {
      setLiveFps(framesInSecond);
      setLiveKbps(Math.round((bytesInSecond * 8) / 1000));
      framesInSecond = 0;
      bytesInSecond = 0;
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
        onConnected?.({
          deviceId: targetDeviceId,
          hostName: info.hostName,
          pin,
        });
        stageRef.current?.focus();
      },
      onVideoFrame: (frame: VideoFrame) => {
        framesInSecond++;
        const canvas = canvasRef.current;
        if (!canvas) {
          frame.close();
          return;
        }
        const w = frame.displayWidth;
        const h = frame.displayHeight;
        if (canvas.width !== w || canvas.height !== h) {
          canvas.width = w;
          canvas.height = h;
        }
        const ctx = canvas.getContext("2d");
        if (ctx) {
          ctx.drawImage(frame, 0, 0, w, h);
        }
        frame.close();
      },
      onMetrics: (m) => {
        if (m.rttMs !== undefined) setRttMs(m.rttMs);
        if (m.bytesReceived !== undefined) bytesInSecond += m.bytesReceived;
      },
      onClipboard: (text) => {
        if (navigator.clipboard?.writeText) {
          navigator.clipboard.writeText(text).catch(() => { });
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
      window.clearInterval(statsTimer);
      if (hideToolbarTimerRef.current !== null) {
        window.clearTimeout(hideToolbarTimerRef.current);
      }
      if (isTauriEnv()) {
        getCurrentWindow().setFullscreen(false).catch(() => { });
      } else if (document.fullscreenElement) {
        document.exitFullscreen().catch(() => { });
      }
      if (recorderRef.current && recorderRef.current.state !== "inactive") {
        recorderRef.current.stop();
      }
      client.disconnect();
      clientRef.current = null;
    };
  }, [serverUrl, targetDeviceId, viewerName, pin, authToken]);

  useEffect(() => {
    const onFsChange = () => {
      if (!isTauriEnv()) {
        const active = Boolean(document.fullscreenElement);
        setIsFullscreen(active);
        if (!active) setToolbarRevealed(false);
      }
    };
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, []);

  useEffect(() => {
    if (!isRecording) {
      setRecordSeconds(0);
      return;
    }
    const id = window.setInterval(() => setRecordSeconds((s) => s + 1), 1000);
    return () => window.clearInterval(id);
  }, [isRecording]);

  const toggleRecording = () => {
    if (isRecording) {
      if (recorderRef.current && recorderRef.current.state !== "inactive") {
        recorderRef.current.stop();
      }
      setIsRecording(false);
      return;
    }

    const canvas = canvasRef.current;
    if (!canvas || canvas.width === 0) {
      flashToast("Espera a que cargue el video para iniciar la grabación local");
      return;
    }

    try {
      const videoStream = canvas.captureStream(Math.min(30, Math.max(15, fps)));
      const tracks: MediaStreamTrack[] = [...videoStream.getVideoTracks()];
      const audioStream = clientRef.current?.getAudioStream();
      if (audioStream) {
        tracks.push(...audioStream.getAudioTracks());
      }
      const combined = new MediaStream(tracks);
      const mimeType = MediaRecorder.isTypeSupported("video/webm;codecs=vp9,opus")
        ? "video/webm;codecs=vp9,opus"
        : MediaRecorder.isTypeSupported("video/webm;codecs=vp8,opus")
          ? "video/webm;codecs=vp8,opus"
          : "video/webm";

      const recorder = new MediaRecorder(combined, { mimeType });
      recordedChunksRef.current = [];
      recorder.ondataavailable = (ev) => {
        if (ev.data && ev.data.size > 0) {
          recordedChunksRef.current.push(ev.data);
        }
      };
      recorder.onstop = () => {
        const blob = new Blob(recordedChunksRef.current, { type: "video/webm" });
        const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
        const safeHost = hostName.replace(/[^a-zA-Z0-9_-]/g, "_");
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `RemoteConnect-${safeHost}-${stamp}.webm`;
        a.click();
        URL.revokeObjectURL(url);
        flashToast("Grabación guardada localmente en esta PC");
      };
      recorder.start(1000);
      recorderRef.current = recorder;
      setIsRecording(true);
      flashToast("Grabando sesión solo en esta PC (no se guarda en el servidor)");
    } catch {
      flashToast("No se pudo iniciar la grabación local en este dispositivo");
    }
  };

  const formatRecTime = (secs: number) => {
    const m = String(Math.floor(secs / 60)).padStart(2, "0");
    const s = String(secs % 60).padStart(2, "0");
    return `${m}:${s}`;
  };

  const applyStreamConfig = (
    nextFps: number,
    nextBitrateKbps: number,
    nextDisplay: number,
    nextScale: number
  ) => {
    setFps(nextFps);
    setBitrateKbps(nextBitrateKbps);
    setSelectedDisplay(nextDisplay);
    setScale(nextScale);
    clientRef.current?.sendStreamConfig(
      nextFps,
      nextBitrateKbps,
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
    if (now - lastMoveSentRef.current < 16) return; // ~60 Hz ultra-responsive mouse
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

  const toggleFullscreen = async () => {
    const next = !isFullscreen;
    if (isTauriEnv()) {
      try {
        await getCurrentWindow().setFullscreen(next);
        setIsFullscreen(next);
        setToolbarRevealed(false);
        if (!next) setShowSettings(false);
      } catch {
        // fallback to DOM fullscreen
        if (next) {
          await document.documentElement.requestFullscreen().catch(() => { });
        } else {
          await document.exitFullscreen().catch(() => { });
        }
        setIsFullscreen(next);
      }
    } else {
      if (next) {
        await document.documentElement.requestFullscreen().catch(() => { });
      } else {
        await document.exitFullscreen().catch(() => { });
      }
    }
  };

  const revealToolbar = () => {
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
      hideToolbarTimerRef.current = null;
    }
    setToolbarRevealed(true);
  };

  const scheduleHideToolbar = () => {
    if (showSettings) return;
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
    }
    hideToolbarTimerRef.current = window.setTimeout(() => {
      setToolbarRevealed(false);
    }, 450);
  };

  return (
    <div className={`viewer-shell ${isFullscreen ? "fullscreen" : ""}`}>
      {isFullscreen && !toolbarRevealed && (
        <div
          className="fullscreen-hover-zone"
          onMouseEnter={revealToolbar}
          onClick={revealToolbar}
        >
          <div className="fullscreen-handle-pill">
            <ChevronDown size={11} />
            Menú
          </div>
        </div>
      )}

      <div
        className={`viewer-toolbar ${isFullscreen && toolbarRevealed ? "revealed" : ""}`}
        onMouseEnter={() => {
          if (isFullscreen) revealToolbar();
        }}
        onMouseLeave={() => {
          if (isFullscreen) scheduleHideToolbar();
        }}
      >
        <div className="viewer-toolbar-group">
          <Monitor size={18} color="var(--accent)" />
          <strong style={{ fontSize: 14 }}>{hostName}</strong>
          <span className="viewer-badge">
            <Zap size={11} />
            H.264 HW
          </span>
          <span className="viewer-badge">
            <Lock size={11} />
            E2E
          </span>
          {!permissions.allow_input && (
            <span
              className="viewer-badge"
              style={{ background: "var(--warning-soft)", color: "var(--warning-text)" }}
            >
              Solo lectura
            </span>
          )}
          <span
            style={{
              fontSize: 11.5,
              color: "var(--text-muted)",
              fontFamily: "JetBrains Mono, monospace",
              display: "inline-flex",
              alignItems: "center",
              gap: 8,
            }}
          >
            <span>{liveFps} FPS</span>
            <span>{liveKbps} kbps</span>
            {rttMs !== null && (
              <span style={{ display: "inline-flex", alignItems: "center", gap: 3 }}>
                <Activity size={11} />
                {rttMs} ms
              </span>
            )}
          </span>
        </div>

        <div className="viewer-toolbar-group">
          {displays.length > 1 && (
            <select
              value={selectedDisplay}
              onChange={(e) =>
                applyStreamConfig(fps, bitrateKbps, Number(e.target.value), scale)
              }
              style={{ padding: "5px 9px", fontSize: 12 }}
              title="Cambiar monitor remoto"
            >
              {displays.map((d, idx) => (
                <option key={d.id} value={idx}>
                  Pantalla {idx + 1}
                  {d.is_primary ? " ★" : ""} ({d.width}×{d.height})
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
            className={`btn btn-sm ${isRecording ? "btn-danger" : "btn-secondary"}`}
            onClick={toggleRecording}
            title="Grabar sesión localmente en esta PC (nunca se guarda en el servidor)"
          >
            {isRecording ? (
              <>
                <Square size={12} fill="currentColor" />
                REC {formatRecTime(recordSeconds)}
              </>
            ) : (
              <>
                <Circle size={12} color="#ef4444" fill="#ef4444" />
                Grabar
              </>
            )}
          </button>

          <button
            className="btn btn-secondary btn-sm"
            onClick={() => setShowSettings((s) => !s)}
            title="Calidad, Bitrate H.264 y FPS"
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
            title={
              isFullscreen
                ? "Salir de pantalla completa"
                : "Pantalla completa (sin barra superior)"
            }
          >
            {isFullscreen ? <Minimize2 size={16} /> : <Maximize size={16} />}
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
          <div style={{ display: "flex", gap: 6 }}>
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => applyStreamConfig(60, 2000, selectedDisplay, 0.75)}
            >
              Ultra Fluido (60 FPS)
            </button>
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => applyStreamConfig(30, 2500, selectedDisplay, 1.0)}
            >
              Equilibrado (30 FPS)
            </button>
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => applyStreamConfig(30, 5500, selectedDisplay, 1.0)}
            >
              Alta Nitidez (5.5 Mbps)
            </button>
          </div>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            FPS ({fps}):
            <input
              type="range"
              min={10}
              max={60}
              step={5}
              value={fps}
              onChange={(e) =>
                applyStreamConfig(Number(e.target.value), bitrateKbps, selectedDisplay, scale)
              }
            />
          </label>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            Bitrate H.264 ({(bitrateKbps / 1000).toFixed(1)} Mbps):
            <input
              type="range"
              min={500}
              max={8000}
              step={250}
              value={bitrateKbps}
              onChange={(e) =>
                applyStreamConfig(fps, Number(e.target.value), selectedDisplay, scale)
              }
            />
          </label>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            Escala ({Math.round(scale * 100)}%):
            <input
              type="range"
              min={50}
              max={100}
              step={5}
              value={Math.round(scale * 100)}
              onChange={(e) =>
                applyStreamConfig(
                  fps,
                  bitrateKbps,
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
