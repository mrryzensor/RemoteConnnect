import { useEffect, useRef, useState } from "react";
import {
  Activity,
  ChevronDown,
  ChevronUp,
  Circle,
  Clipboard,
  Download,
  FolderOpen,
  Hand,
  Keyboard,
  Lock,
  Maximize,
  Minimize2,
  Monitor,
  Moon,
  MousePointer2,
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
import { isTauriEnv, saveRecordingFile } from "../lib/tauriBridge";
import { bytesToBase64 } from "../lib/crypto";
import {
  DEFAULT_PERMISSIONS,
  DEFAULT_PREFERENCES,
  formatBytes,
  type ClipboardFileInfo,
  type DisplayInfo,
  type FileEntry,
  type SessionPermissions,
  type UserPreferences,
} from "../types";
import { RemoteViewerClient } from "../lib/viewerClient";
import { FileTransferModal } from "./FileTransferModal";

interface Props {
  serverUrl: string;
  targetDeviceId: string;
  viewerName: string;
  pin?: string;
  authToken?: string | null;
  preferences?: UserPreferences;
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
  preferences,
  theme,
  onToggleTheme,
  onConnected,
  onExit,
}: Props) {
  const effectivePrefs: UserPreferences = {
    ...DEFAULT_PREFERENCES,
    ...(preferences ?? {}),
  };
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const canvasCtxRef = useRef<CanvasRenderingContext2D | null>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const clientRef = useRef<RemoteViewerClient | null>(null);
  const lastMoveSentRef = useRef(0);
  const lastTouchTimeRef = useRef(0);
  const onConnectedRef = useRef(onConnected);
  onConnectedRef.current = onConnected;

  const isTouchDevice =
    typeof window !== "undefined" &&
    (/Android|iPhone|iPad|iPod/i.test(navigator.userAgent) ||
      window.matchMedia?.("(pointer: coarse)").matches);

  const [touchMode, setTouchMode] = useState<"touch" | "cursor">("touch");
  const [zoom, setZoom] = useState<number>(1.0);
  const [pan, setPan] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const zoomRef = useRef<number>(1.0);
  const panRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const [showSpecialKeys, setShowSpecialKeys] = useState<boolean>(isTouchDevice);
  const [stickyMods, setStickyMods] = useState<{
    ctrl: boolean;
    shift: boolean;
    win: boolean;
    alt: boolean;
    altGr: boolean;
  }>({ ctrl: false, shift: false, win: false, alt: false, altGr: false });
  const mobileInputRef = useRef<HTMLInputElement>(null);
  const [virtualCursor, setVirtualCursor] = useState<{ x: number; y: number }>({
    x: 0.5,
    y: 0.5,
  });
  const virtualCursorRef = useRef<{ x: number; y: number }>({ x: 0.5, y: 0.5 });

  const updateZoom = (nextZoom: number) => {
    const clamped = Math.max(1.0, Math.min(4.0, Math.round(nextZoom * 20) / 20));
    zoomRef.current = clamped;
    setZoom(clamped);
    if (clamped <= 1.01) {
      panRef.current = { x: 0, y: 0 };
      setPan({ x: 0, y: 0 });
    }
  };

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
  const [fps, setFps] = useState(effectivePrefs.default_fps);
  const [bitrateKbps, setBitrateKbps] = useState(effectivePrefs.default_bitrate_kbps);
  const [scale, setScale] = useState(effectivePrefs.default_scale);
  const [liveFps, setLiveFps] = useState(0);
  const [liveKbps, setLiveKbps] = useState(0);
  const [rttMs, setRttMs] = useState<number | null>(null);
  const [transportMode, setTransportMode] = useState<"p2p" | "relay">("relay");
  const [audioMuted, setAudioMuted] = useState(effectivePrefs.mute_audio_on_connect);
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
        // Persist device immediately upon approval/connection (never wait until disconnect)
        void onConnectedRef.current?.({
          deviceId: targetDeviceId,
          hostName: info.hostName,
          pin,
        });
        stageRef.current?.focus();
        client.setAudioMuted(effectivePrefs.mute_audio_on_connect);
        if (
          effectivePrefs.default_fps !== 30 ||
          effectivePrefs.default_bitrate_kbps !== 2500 ||
          effectivePrefs.default_scale !== 1.0
        ) {
          client.sendStreamConfig(
            effectivePrefs.default_fps,
            effectivePrefs.default_bitrate_kbps,
            0,
            effectivePrefs.default_scale
          );
        }
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
          canvasCtxRef.current = canvas.getContext("2d", {
            alpha: false,
            desynchronized: true,
          });
        }
        if (!canvasCtxRef.current) {
          canvasCtxRef.current = canvas.getContext("2d", {
            alpha: false,
            desynchronized: true,
          });
        }
        canvasCtxRef.current?.drawImage(frame, 0, 0, w, h);
        frame.close();
      },
      onBitmapFrame: (bitmap, width, height) => {
        framesInSecond++;
        const canvas = canvasRef.current;
        if (!canvas) {
          bitmap.close();
          return;
        }
        if (canvas.width !== width || canvas.height !== height) {
          canvas.width = width;
          canvas.height = height;
          canvasCtxRef.current = canvas.getContext("2d", {
            alpha: false,
            desynchronized: true,
          });
        }
        if (!canvasCtxRef.current) {
          canvasCtxRef.current = canvas.getContext("2d", {
            alpha: false,
            desynchronized: true,
          });
        }
        canvasCtxRef.current?.drawImage(bitmap, 0, 0);
        bitmap.close();
      },
      onDecoderError: (msg) => {
        flashToast(msg);
      },
      onTransportChange: (mode) => {
        setTransportMode(mode);
        if (mode === "p2p") {
          flashToast("Conexión directa P2P activa (latencia mínima)");
        }
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
      preferP2p: effectivePrefs.prefer_p2p,
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
      const active = Boolean(document.fullscreenElement);
      if (!isTauriEnv() || active) {
        setIsFullscreen(active);
      }
      setShowSettings(false);
      setToolbarRevealed(false);
    };
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, []);

  useEffect(() => {
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
      hideToolbarTimerRef.current = null;
    }
    if (isFullscreen) {
      setShowSettings(false);
      setToolbarRevealed(false);
    } else {
      setToolbarRevealed(false);
    }
  }, [isFullscreen]);

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
      recorder.onstop = async () => {
        const blob = new Blob(recordedChunksRef.current, { type: "video/webm" });
        const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
        const safeHost = hostName.replace(/[^a-zA-Z0-9_-]/g, "_");
        const fileName = `RemoteConnect-${safeHost}-${stamp}.webm`;

        if (isTauriEnv()) {
          try {
            const bytes = new Uint8Array(await blob.arrayBuffer());
            const savedPath = await saveRecordingFile(fileName, bytesToBase64(bytes));
            flashToast(`Grabación guardada en: ${savedPath}`);
            return;
          } catch {
            // Fallback to browser download if native save fails
          }
        }

        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = fileName;
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
    nextScale: number,
    nextShowCursor?: boolean
  ) => {
    setFps(nextFps);
    setBitrateKbps(nextBitrateKbps);
    setSelectedDisplay(nextDisplay);
    setScale(nextScale);
    const cursorFlag =
      nextShowCursor !== undefined ? nextShowCursor : isTouchDevice && touchMode === "cursor";
    clientRef.current?.sendStreamConfig(
      nextFps,
      nextBitrateKbps,
      nextDisplay,
      nextScale,
      cursorFlag
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
    if (isTouchDevice && touchMode === "cursor") return;
    if (performance.now() - lastTouchTimeRef.current < 1000) return; // ignore synthetic mouse after touch
    const now = performance.now();
    if (now - lastMoveSentRef.current < 16) return; // ~60 Hz ultra-responsive mouse
    lastMoveSentRef.current = now;
    const { x, y } = getNormalizedCoords(e);
    clientRef.current?.sendMouseMove(x, y);
  };

  const handleMouseDown = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (isTouchDevice && touchMode === "cursor") return;
    if (performance.now() - lastTouchTimeRef.current < 1000) return; // ignore synthetic mouse after touch
    e.preventDefault();
    clientRef.current?.resumeAudio();
    stageRef.current?.focus();
    if (!permissions.allow_input) return;
    const { x, y } = getNormalizedCoords(e);
    clientRef.current?.sendMouseButton(mapMouseButton(e.button), true, x, y);
  };

  const handleMouseUp = (e: React.MouseEvent<HTMLCanvasElement>) => {
    if (isTouchDevice && touchMode === "cursor") return;
    if (performance.now() - lastTouchTimeRef.current < 1000) return; // ignore synthetic mouse after touch
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

  // Touch support for Android / tablets:
  // - "touch" mode: direct tap = left click at finger, drag = move
  // - "cursor" mode: moves the REAL Windows cursor on the host like a laptop trackpad
  // - Right click: configurable via `right_click_gesture` ("long_press" default, or "two_finger_tap")
  // - 2-finger pinch = zoom (1x..4x), 2-finger drag = pan when zoomed or scroll at 1x
  const touchStateRef = useRef<{
    startX: number;
    startY: number;
    lastX: number;
    lastY: number;
    moved: boolean;
    longPressTimer: number | null;
    rightClicked: boolean;
    twoFingerStartDist: number;
    twoFingerStartZoom: number;
    twoFingerMidX: number;
    twoFingerMidY: number;
    twoFingerMoved: boolean;
    wasTwoFinger: boolean;
  }>({
    startX: 0,
    startY: 0,
    lastX: 0,
    lastY: 0,
    moved: false,
    longPressTimer: null,
    rightClicked: false,
    twoFingerStartDist: 0,
    twoFingerStartZoom: 1,
    twoFingerMidX: 0,
    twoFingerMidY: 0,
    twoFingerMoved: false,
    wasTwoFinger: false,
  });

  const getTouchNorm = (touch: React.Touch, el: HTMLCanvasElement) => {
    const rect = el.getBoundingClientRect();
    return {
      x: Math.max(0, Math.min(1, (touch.clientX - rect.left) / rect.width)),
      y: Math.max(0, Math.min(1, (touch.clientY - rect.top) / rect.height)),
    };
  };

  const handleTouchStart = (e: React.TouchEvent<HTMLCanvasElement>) => {
    lastTouchTimeRef.current = performance.now();
    clientRef.current?.resumeAudio();
    if (isFullscreen && toolbarRevealed) {
      setToolbarRevealed(false);
      setShowSettings(false);
    }
    if (!permissions.allow_input || e.touches.length === 0) return;
    const ts = touchStateRef.current;
    if (ts.longPressTimer !== null) {
      window.clearTimeout(ts.longPressTimer);
      ts.longPressTimer = null;
    }

    if (e.touches.length === 2) {
      const t0 = e.touches[0];
      const t1 = e.touches[1];
      ts.wasTwoFinger = true;
      ts.twoFingerMoved = false;
      ts.twoFingerStartDist = Math.hypot(t1.clientX - t0.clientX, t1.clientY - t0.clientY);
      ts.twoFingerStartZoom = zoomRef.current;
      ts.twoFingerMidX = (t0.clientX + t1.clientX) / 2;
      ts.twoFingerMidY = (t0.clientY + t1.clientY) / 2;
      ts.lastY = ts.twoFingerMidY;
      return;
    }

    const t = e.touches[0];
    ts.startX = t.clientX;
    ts.startY = t.clientY;
    ts.lastX = t.clientX;
    ts.lastY = t.clientY;
    ts.moved = false;
    ts.rightClicked = false;
    ts.wasTwoFinger = false;

    const useLongPress = effectivePrefs.right_click_gesture !== "two_finger_tap";
    if (e.touches.length === 1) {
      if (touchMode === "touch") {
        const { x, y } = getTouchNorm(t, e.currentTarget);
        clientRef.current?.sendMouseMove(x, y);
        if (useLongPress) {
          ts.longPressTimer = window.setTimeout(() => {
            if (!ts.moved) {
              ts.rightClicked = true;
              clientRef.current?.sendMouseButton("right", true, x, y);
              clientRef.current?.sendMouseButton("right", false, x, y);
            }
          }, 460);
        }
      } else if (useLongPress) {
        ts.longPressTimer = window.setTimeout(() => {
          if (!ts.moved) {
            ts.rightClicked = true;
            const cur = virtualCursorRef.current;
            clientRef.current?.sendMouseButton("right", true, cur.x, cur.y);
            clientRef.current?.sendMouseButton("right", false, cur.x, cur.y);
          }
        }, 460);
      }
    }
  };

  const handleTouchMove = (e: React.TouchEvent<HTMLCanvasElement>) => {
    lastTouchTimeRef.current = performance.now();
    if (!permissions.allow_input || e.touches.length === 0) return;
    const ts = touchStateRef.current;

    if (e.touches.length === 2) {
      const t0 = e.touches[0];
      const t1 = e.touches[1];
      const dist = Math.hypot(t1.clientX - t0.clientX, t1.clientY - t0.clientY);
      const midX = (t0.clientX + t1.clientX) / 2;
      const midY = (t0.clientY + t1.clientY) / 2;

      if (Math.abs(dist - ts.twoFingerStartDist) > 10) {
        ts.twoFingerMoved = true;
        if (ts.twoFingerStartDist > 10) {
          const ratio = dist / ts.twoFingerStartDist;
          updateZoom(ts.twoFingerStartZoom * ratio);
        }
      }

      const moveDx = midX - ts.twoFingerMidX;
      const moveDy = midY - ts.twoFingerMidY;
      if (Math.hypot(moveDx, moveDy) > 6) {
        ts.twoFingerMoved = true;
        if (zoomRef.current > 1.05) {
          const nextPan = {
            x: Math.max(-600, Math.min(600, panRef.current.x + moveDx)),
            y: Math.max(-600, Math.min(600, panRef.current.y + moveDy)),
          };
          panRef.current = nextPan;
          setPan(nextPan);
          ts.twoFingerMidX = midX;
          ts.twoFingerMidY = midY;
        } else if (Math.abs(ts.lastY - midY) > 12) {
          clientRef.current?.sendMouseScroll(0, Math.sign(ts.lastY - midY) * 2);
          ts.lastY = midY;
        }
      }
      return;
    }

    const t = e.touches[0];
    if (!ts.moved) {
      if (Math.hypot(t.clientX - ts.startX, t.clientY - ts.startY) <= 7) {
        return;
      }
      ts.moved = true;
      ts.lastX = t.clientX;
      ts.lastY = t.clientY;
      if (ts.longPressTimer !== null) {
        window.clearTimeout(ts.longPressTimer);
        ts.longPressTimer = null;
      }
    }

    if (touchMode === "cursor") {
      const rect = e.currentTarget.getBoundingClientRect();
      const baseW = rect.width / Math.max(1, zoomRef.current);
      const baseH = rect.height / Math.max(1, zoomRef.current);
      const dx = ((t.clientX - ts.lastX) / Math.max(1, baseW)) * 1.45;
      const dy = ((t.clientY - ts.lastY) / Math.max(1, baseH)) * 1.45;
      ts.lastX = t.clientX;
      ts.lastY = t.clientY;

      const nx = Math.max(0, Math.min(1, virtualCursorRef.current.x + dx));
      const ny = Math.max(0, Math.min(1, virtualCursorRef.current.y + dy));
      virtualCursorRef.current = { x: nx, y: ny };
      setVirtualCursor({ x: nx, y: ny });

      // RustDesk-style auto-follow: pan the zoomed screen smoothly to follow the cursor
      if (zoomRef.current > 1.02) {
        const z = zoomRef.current;
        const maxPanX = (baseW * (z - 1)) / 2;
        const maxPanY = (baseH * (z - 1)) / 2;
        const nextPan = {
          x: Math.max(-maxPanX, Math.min(maxPanX, (0.5 - nx) * baseW * (z - 1))),
          y: Math.max(-maxPanY, Math.min(maxPanY, (0.5 - ny) * baseH * (z - 1))),
        };
        panRef.current = nextPan;
        setPan(nextPan);
      }

      const now = performance.now();
      if (now - lastMoveSentRef.current < 16) return;
      lastMoveSentRef.current = now;
      clientRef.current?.sendMouseMove(nx, ny);
      return;
    }

    const now = performance.now();
    if (now - lastMoveSentRef.current < 16) return;
    lastMoveSentRef.current = now;
    const { x, y } = getTouchNorm(t, e.currentTarget);
    clientRef.current?.sendMouseMove(x, y);
  };

  const handleTouchEnd = (e: React.TouchEvent<HTMLCanvasElement>) => {
    lastTouchTimeRef.current = performance.now();
    e.preventDefault();
    const ts = touchStateRef.current;
    if (ts.longPressTimer !== null) {
      window.clearTimeout(ts.longPressTimer);
      ts.longPressTimer = null;
    }
    if (!permissions.allow_input) return;

    // Two-finger quick tap -> right click when configured
    if (
      ts.wasTwoFinger &&
      e.touches.length === 0 &&
      !ts.twoFingerMoved &&
      effectivePrefs.right_click_gesture === "two_finger_tap"
    ) {
      ts.wasTwoFinger = false;
      const pos =
        touchMode === "cursor"
          ? virtualCursorRef.current
          : getTouchNorm(e.changedTouches[0], e.currentTarget);
      clientRef.current?.sendMouseButton("right", true, pos.x, pos.y);
      clientRef.current?.sendMouseButton("right", false, pos.x, pos.y);
      return;
    }

    if (ts.wasTwoFinger || ts.rightClicked || ts.moved) return;

    if (touchMode === "cursor") {
      const cur = virtualCursorRef.current;
      clientRef.current?.sendMouseButton("left", true, cur.x, cur.y);
      clientRef.current?.sendMouseButton("left", false, cur.x, cur.y);
      return;
    }

    const t = e.changedTouches[0];
    if (!t) return;
    const { x, y } = getTouchNorm(t, e.currentTarget);
    clientRef.current?.sendMouseButton("left", true, x, y);
    clientRef.current?.sendMouseButton("left", false, x, y);
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

  // Sticky modifier keys + special Windows keys helper for Android / touch devices
  const MOD_MAP: Record<
    keyof typeof stickyMods,
    { code: string; key: string }
  > = {
    ctrl: { code: "ControlLeft", key: "Control" },
    shift: { code: "ShiftLeft", key: "Shift" },
    win: { code: "MetaLeft", key: "Meta" },
    alt: { code: "AltLeft", key: "Alt" },
    altGr: { code: "AltRight", key: "AltGraph" },
  };

  const toggleStickyMod = (mod: keyof typeof stickyMods) => {
    if (!permissions.allow_input) return;
    setStickyMods((prev) => {
      const nextVal = !prev[mod];
      const def = MOD_MAP[mod];
      clientRef.current?.sendKeyEvent(def.code, def.key, nextVal);
      return { ...prev, [mod]: nextVal };
    });
  };

  const releaseAllStickyMods = () => {
    setStickyMods((prev) => {
      let changed = false;
      const next = { ...prev };
      (Object.keys(MOD_MAP) as Array<keyof typeof stickyMods>).forEach((m) => {
        if (prev[m]) {
          changed = true;
          next[m] = false;
          const def = MOD_MAP[m];
          clientRef.current?.sendKeyEvent(def.code, def.key, false);
        }
      });
      return changed ? next : prev;
    });
  };

  const tapSpecialKey = (code: string, key: string) => {
    if (!permissions.allow_input) return;
    clientRef.current?.resumeAudio();
    clientRef.current?.sendKeyEvent(code, key, true);
    clientRef.current?.sendKeyEvent(code, key, false);
    releaseAllStickyMods();
  };

  const handleMobileTextInput = (e: React.ChangeEvent<HTMLInputElement>) => {
    const val = e.target.value;
    if (!val || !permissions.allow_input) return;
    for (const ch of val) {
      clientRef.current?.sendKeyEvent("", ch, true);
      clientRef.current?.sendKeyEvent("", ch, false);
    }
    releaseAllStickyMods();
    e.target.value = "";
  };

  const handleMobileInputKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (!permissions.allow_input) return;
    if (e.key === "Backspace" || e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      tapSpecialKey(e.key, e.key);
    }
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
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
      hideToolbarTimerRef.current = null;
    }
    setShowSettings(false);
    setToolbarRevealed(false);
    if (isTauriEnv()) {
      try {
        await getCurrentWindow().setFullscreen(next);
        setIsFullscreen(next);
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
    stageRef.current?.focus();
  };

  const revealToolbar = () => {
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
      hideToolbarTimerRef.current = null;
    }
    setToolbarRevealed(true);
  };

  const scheduleHideToolbar = () => {
    if (hideToolbarTimerRef.current !== null) {
      window.clearTimeout(hideToolbarTimerRef.current);
    }
    hideToolbarTimerRef.current = window.setTimeout(() => {
      setToolbarRevealed(false);
      setShowSettings(false);
    }, 450);
  };

  return (
    <div
      className={`viewer-shell ${isFullscreen ? "fullscreen" : ""} ${
        isFullscreen && toolbarRevealed ? "toolbar-revealed" : ""
      }`}
    >
      {isFullscreen && !toolbarRevealed && (
        <div
          className="fullscreen-hover-zone"
          onMouseEnter={revealToolbar}
          onClick={revealToolbar}
        >
          <div className="fullscreen-handle-pill">
            <ChevronDown size={12} />
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
            {transportMode === "p2p" ? "P2P Directo" : "Relay"}
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

          {isTouchDevice && (
            <>
              <button
                className="btn btn-secondary btn-sm"
                onClick={() => {
                  const next = touchMode === "touch" ? "cursor" : "touch";
                  setTouchMode(next);
                  // Keep remote host cursor hidden to avoid double cursor; we draw a crisp 0ms SVG pointer locally
                  applyStreamConfig(fps, bitrateKbps, selectedDisplay, scale, false);
                  if (next === "cursor") {
                    clientRef.current?.sendMouseMove(
                      virtualCursorRef.current.x,
                      virtualCursorRef.current.y
                    );
                  }
                  flashToast(
                    next === "cursor"
                      ? "Modo Cursor: arrastra para mover, toca para clic"
                      : "Modo Táctil directo: toca directamente donde quieras hacer clic"
                  );
                }}
                title="Cambiar entre modo Táctil directo y Cursor real de Windows"
              >
                {touchMode === "cursor" ? (
                  <>
                    <MousePointer2 size={14} color="var(--accent)" />
                    Cursor
                  </>
                ) : (
                  <>
                    <Hand size={14} color="var(--accent)" />
                    Táctil
                  </>
                )}
              </button>
              <div style={{ display: "inline-flex", alignItems: "center", gap: 2 }}>
                <button
                  className="btn btn-secondary btn-sm"
                  style={{ padding: "5px 8px" }}
                  onClick={() => updateZoom(zoom - 0.5)}
                  title="Alejar zoom"
                >
                  −
                </button>
                <button
                  className="btn btn-secondary btn-sm"
                  style={{ padding: "5px 7px", fontFamily: "JetBrains Mono, monospace", fontSize: 11 }}
                  onClick={() => updateZoom(1.0)}
                  title="Restablecer zoom a 100%"
                >
                  {Math.round(zoom * 100)}%
                </button>
                <button
                  className="btn btn-secondary btn-sm"
                  style={{ padding: "5px 8px" }}
                  onClick={() => updateZoom(zoom + 0.5)}
                  title="Acercar zoom"
                >
                  +
                </button>
              </div>
              <button
                className={`btn btn-sm ${showSpecialKeys ? "btn-primary" : "btn-secondary"}`}
                onClick={() => setShowSpecialKeys((s) => !s)}
                title="Mostrar / ocultar barra de teclas especiales Windows"
              >
                <Keyboard size={14} />
                Teclas
              </button>
            </>
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
              onExit(`Desconectado de ${hostName}`);
            }}
          >
            <PhoneOff size={14} />
            Desconectar
          </button>

          {isFullscreen && toolbarRevealed && (
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => {
                setToolbarRevealed(false);
                setShowSettings(false);
              }}
              title="Ocultar barra de menú"
            >
              <ChevronUp size={14} />
              Ocultar
            </button>
          )}
        </div>
      </div>

      {showSettings && (!isFullscreen || toolbarRevealed) && (
        <div
          className="viewer-settings-bar"
          onMouseEnter={() => {
            if (isFullscreen) revealToolbar();
          }}
          onMouseLeave={() => {
            if (isFullscreen) scheduleHideToolbar();
          }}
        >
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
        <div
          className="viewer-canvas-wrap"
          style={{
            transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
            transformOrigin: "center center",
          }}
        >
          <div className="viewer-canvas-box">
            <canvas
              ref={canvasRef}
              className="viewer-canvas"
              style={{ touchAction: "none" }}
              onMouseMove={handleMouseMove}
              onMouseDown={handleMouseDown}
              onMouseUp={handleMouseUp}
              onWheel={handleWheel}
              onTouchStart={handleTouchStart}
              onTouchMove={handleTouchMove}
              onTouchEnd={handleTouchEnd}
              onContextMenu={(e) => e.preventDefault()}
            />
            {isTouchDevice && touchMode === "cursor" && (
              <svg
                className="virtual-cursor-arrow"
                viewBox="0 0 24 24"
                style={{
                  left: `${virtualCursor.x * 100}%`,
                  top: `${virtualCursor.y * 100}%`,
                }}
              >
                <path
                  d="M4 2 L4 20 L9 15 L13 22 L15.6 20.6 L11.8 14 L18.5 14 Z"
                  fill="#ffffff"
                  stroke="#0f172a"
                  strokeWidth="1.8"
                  strokeLinejoin="round"
                />
              </svg>
            )}
          </div>
        </div>

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

      {showSpecialKeys && permissions.allow_input && (
        <div className="special-keys-dock">
          <input
            ref={mobileInputRef}
            type="text"
            className="mobile-kbd-input"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            onChange={handleMobileTextInput}
            onKeyDown={handleMobileInputKeyDown}
            aria-label="Teclado virtual"
          />

          <div className="special-keys-row">
            <button
              className="skey-btn accent"
              onClick={() => mobileInputRef.current?.focus()}
            >
              ⌨ Teclado
            </button>
            <button
              className="skey-btn"
              onClick={() => {
                const cur = virtualCursorRef.current;
                clientRef.current?.sendMouseButton("right", true, cur.x, cur.y);
                clientRef.current?.sendMouseButton("right", false, cur.x, cur.y);
              }}
            >
              Clic Der.
            </button>
            <button
              className={`skey-btn ${stickyMods.ctrl ? "active" : ""}`}
              onClick={() => toggleStickyMod("ctrl")}
            >
              Ctrl
            </button>
            <button
              className={`skey-btn ${stickyMods.shift ? "active" : ""}`}
              onClick={() => toggleStickyMod("shift")}
            >
              Shift
            </button>
            <button
              className={`skey-btn ${stickyMods.win ? "active" : ""}`}
              onClick={() => toggleStickyMod("win")}
            >
              Win
            </button>
            <button
              className={`skey-btn ${stickyMods.alt ? "active" : ""}`}
              onClick={() => toggleStickyMod("alt")}
            >
              Alt
            </button>
            <button
              className={`skey-btn ${stickyMods.altGr ? "active" : ""}`}
              onClick={() => toggleStickyMod("altGr")}
            >
              Alt Gr
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Escape", "Escape")}>
              Esc
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Tab", "Tab")}>
              Tab
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Enter", "Enter")}>
              Enter
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Backspace", "Backspace")}>
              ⌫
            </button>
          </div>

          <div className="special-keys-row">
            <button className="skey-btn" onClick={() => tapSpecialKey("ArrowLeft", "ArrowLeft")}>
              ←
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("ArrowUp", "ArrowUp")}>
              ↑
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("ArrowDown", "ArrowDown")}>
              ↓
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("ArrowRight", "ArrowRight")}>
              →
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Delete", "Delete")}>
              Supr
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Insert", "Insert")}>
              Insert
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Home", "Home")}>
              Inicio
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("End", "End")}>
              Fin
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("PageUp", "PageUp")}>
              RePág
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("PageDown", "PageDown")}>
              AvPág
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("PrintScreen", "PrintScreen")}>
              Impr Pant
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("ScrollLock", "ScrollLock")}>
              ScrLck
            </button>
            <button className="skey-btn" onClick={() => tapSpecialKey("Pause", "Pause")}>
              Pausa
            </button>
          </div>

          <div className="special-keys-row">
            {(["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"] as const).map(
              (fk) => (
                <button
                  key={fk}
                  className="skey-btn"
                  onClick={() => tapSpecialKey(fk, fk)}
                >
                  {fk}
                </button>
              )
            )}
          </div>
        </div>
      )}

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
