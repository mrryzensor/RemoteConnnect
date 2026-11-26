import {
  DEFAULT_PERMISSIONS,
  type ClipboardFileInfo,
  type DisplayInfo,
  type FileEntry,
  type SessionPermissions,
} from "../types";
import { base64ToBytes, bytesToBase64, E2eKeyPair, E2eSession } from "./crypto";

const PACKET_VIDEO_JPEG = 0x01;
const PACKET_AUDIO_PCM16 = 0x02;

export interface ViewerCallbacks {
  onStatus: (
    status: "connecting" | "waiting_approval" | "connected" | "disconnected",
    detail?: string
  ) => void;
  onSessionReady: (info: {
    sessionId: string;
    hostName: string;
    displays: DisplayInfo[];
    permissions: SessionPermissions;
  }) => void;
  onFrame: (bitmap: ImageBitmap, width: number, height: number) => void;
  onClipboard: (text: string) => void;
  onClipboardFiles: (files: ClipboardFileInfo[]) => void;
  onFileList: (data: {
    currentPath: string;
    parentPath: string | null;
    entries: FileEntry[];
    error: string | null;
  }) => void;
  onFileDownloadComplete: (fileName: string, blob: Blob) => void;
  onFileStatus: (ok: boolean, message: string) => void;
}

interface PendingDownload {
  fileName: string;
  totalSize: number;
  chunks: Uint8Array[];
  received: number;
}

export class RemoteViewerClient {
  private ws: WebSocket | null = null;
  private e2e: E2eSession | null = null;
  private sessionId: string | null = null;
  private keyPair: E2eKeyPair | null = null;
  private downloads = new Map<string, PendingDownload>();
  private decodingVideo = false;

  // WebAudio playback engine
  private audioCtx: AudioContext | null = null;
  private audioRecordDest: MediaStreamAudioDestinationNode | null = null;
  private nextAudioTime = 0;
  private audioMuted = false;

  constructor(private readonly cb: ViewerCallbacks) {}

  getAudioStream(): MediaStream | null {
    return this.audioRecordDest?.stream ?? null;
  }

  setAudioMuted(muted: boolean) {
    this.audioMuted = muted;
  }

  resumeAudio() {
    if (this.audioCtx && this.audioCtx.state === "suspended") {
      this.audioCtx.resume().catch(() => {});
    }
  }

  async connect(params: {
    serverUrl: string;
    targetDeviceId: string;
    viewerName: string;
    pin?: string;
    authToken?: string | null;
  }): Promise<void> {
    this.disconnect();
    this.cb.onStatus("connecting", "Estableciendo túnel E2E...");

    this.keyPair = await E2eKeyPair.generate();

    const ws = new WebSocket(params.serverUrl);
    ws.binaryType = "arraybuffer";
    this.ws = ws;

    ws.onopen = () => {
      this.cb.onStatus("waiting_approval", "Esperando respuesta del equipo remoto...");
      ws.send(
        JSON.stringify({
          type: "connect_request",
          target_device_id: params.targetDeviceId,
          viewer_name: params.viewerName,
          pin: params.pin?.trim() ? params.pin.trim() : null,
          public_key: this.keyPair!.publicKeyB64,
          auth_token: params.authToken || null,
        })
      );
    };

    ws.onmessage = async (event) => {
      if (typeof event.data === "string") {
        await this.handleTextMessage(event.data);
      } else if (event.data instanceof ArrayBuffer) {
        await this.handleBinaryPacket(event.data);
      }
    };

    ws.onerror = () => {
      this.cb.onStatus("disconnected", "Error de conexión WebSocket con el servidor");
    };

    ws.onclose = () => {
      if (this.ws === ws) {
        this.ws = null;
        this.e2e = null;
        this.cb.onStatus("disconnected", "Sesión finalizada");
      }
    };
  }

  disconnect(reason?: string) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN && this.sessionId) {
      this.ws.send(
        JSON.stringify({
          type: "disconnect",
          session_id: this.sessionId,
          reason: reason ?? "Viewer closed session",
        })
      );
    }
    if (this.ws) {
      const old = this.ws;
      this.ws = null;
      old.close();
    }
    if (this.audioCtx) {
      this.audioCtx.close().catch(() => {});
      this.audioCtx = null;
      this.audioRecordDest = null;
    }
    this.nextAudioTime = 0;
    this.e2e = null;
    this.sessionId = null;
    this.downloads.clear();
  }

  private async handleTextMessage(raw: string) {
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }

    switch (msg.type) {
      case "session_ready": {
        const accepted = Boolean(msg.accepted);
        if (!accepted) {
          this.cb.onStatus(
            "disconnected",
            (msg.reason as string) || "Conexión rechazada por el equipo remoto"
          );
          this.disconnect();
          return;
        }
        const sessionId = msg.session_id as string;
        const hostPub = msg.host_public_key as string;
        if (!this.keyPair || !hostPub) {
          this.cb.onStatus("disconnected", "Error en intercambio de claves E2E");
          this.disconnect();
          return;
        }
        this.sessionId = sessionId;
        this.e2e = await this.keyPair.deriveSession(hostPub, sessionId);
        const permissions: SessionPermissions =
          (msg.permissions as SessionPermissions) || { ...DEFAULT_PERMISSIONS };
        this.cb.onStatus("connected", "Cifrado E2E AES-256-GCM activo");
        this.cb.onSessionReady({
          sessionId,
          hostName: (msg.host_name as string) || "Equipo Remoto",
          displays: (msg.displays as DisplayInfo[]) || [],
          permissions,
        });
        break;
      }

      case "encrypted_relay": {
        if (!this.e2e || !msg.payload) return;
        try {
          const inner = await this.e2e.decryptJson<Record<string, unknown>>(
            msg.payload as string
          );
          this.handleSessionMessage(inner);
        } catch (err) {
          console.error("Failed to decrypt E2E message:", err);
        }
        break;
      }

      case "disconnect": {
        this.cb.onStatus(
          "disconnected",
          (msg.reason as string) || "El equipo remoto cerró la sesión"
        );
        this.disconnect();
        break;
      }

      case "error": {
        this.cb.onStatus(
          "disconnected",
          (msg.message as string) || "Error del servidor"
        );
        this.disconnect();
        break;
      }
    }
  }

  private async handleBinaryPacket(buffer: ArrayBuffer) {
    if (!this.e2e) return;
    const bytes = new Uint8Array(buffer);
    if (bytes.length <= 36 + 28) return;

    const encrypted = bytes.subarray(36);
    let plain: Uint8Array;
    try {
      plain = await this.e2e.decryptBytes(encrypted);
    } catch {
      return;
    }
    if (plain.length < 2) return;

    const packetType = plain[0];

    if (packetType === PACKET_VIDEO_JPEG) {
      if (this.decodingVideo || plain.length <= 9) return;
      this.decodingVideo = true;
      try {
        const view = new DataView(plain.buffer, plain.byteOffset + 1, plain.byteLength - 1);
        const width = view.getUint32(0, false);
        const height = view.getUint32(4, false);
        const jpegBytes = plain.subarray(9);
        const blob = new Blob(
          [
            jpegBytes.buffer.slice(
              jpegBytes.byteOffset,
              jpegBytes.byteOffset + jpegBytes.byteLength
            ) as ArrayBuffer,
          ],
          { type: "image/jpeg" }
        );
        const bitmap = await createImageBitmap(blob);
        this.cb.onFrame(bitmap, width, height);
      } catch {
        // drop corrupt frame
      } finally {
        this.decodingVideo = false;
      }
    } else if (packetType === PACKET_AUDIO_PCM16) {
      if (this.audioMuted || plain.length <= 7) return;
      this.playAudioPacket(plain.subarray(1));
    }
  }

  private playAudioPacket(payload: Uint8Array) {
    const view = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const sampleRate = view.getUint32(0, false);
    const channels = Math.max(1, Math.min(2, view.getUint16(4, false)));
    const pcmBytes = payload.subarray(6);
    const totalSamples = Math.floor(pcmBytes.byteLength / 2);
    const frameCount = Math.floor(totalSamples / channels);
    if (frameCount <= 0 || sampleRate < 8000 || sampleRate > 192000) return;

    if (!this.audioCtx) {
      this.audioCtx = new AudioContext({ sampleRate });
      this.audioRecordDest = this.audioCtx.createMediaStreamDestination();
      this.nextAudioTime = this.audioCtx.currentTime + 0.05;
    }
    if (this.audioCtx.state === "suspended") {
      this.audioCtx.resume().catch(() => {});
    }

    const audioBuffer = this.audioCtx.createBuffer(channels, frameCount, sampleRate);
    const pcmView = new DataView(pcmBytes.buffer, pcmBytes.byteOffset, pcmBytes.byteLength);

    for (let ch = 0; ch < channels; ch++) {
      const channelData = audioBuffer.getChannelData(ch);
      for (let i = 0; i < frameCount; i++) {
        const sampleIdx = (i * channels + ch) * 2;
        const sample = pcmView.getInt16(sampleIdx, true);
        channelData[i] = sample / 32768.0;
      }
    }

    const source = this.audioCtx.createBufferSource();
    source.buffer = audioBuffer;
    source.connect(this.audioCtx.destination);
    if (this.audioRecordDest) {
      source.connect(this.audioRecordDest);
    }

    const now = this.audioCtx.currentTime;
    // Prevent runaway latency drift if packets stalled
    if (this.nextAudioTime < now || this.nextAudioTime > now + 0.35) {
      this.nextAudioTime = now + 0.04;
    }
    source.start(this.nextAudioTime);
    this.nextAudioTime += audioBuffer.duration;
  }

  private handleSessionMessage(msg: Record<string, unknown>) {
    switch (msg.kind) {
      case "clipboard_sync": {
        if (typeof msg.text === "string") {
          this.cb.onClipboard(msg.text);
        }
        break;
      }

      case "clipboard_files_available": {
        const files = (msg.files as ClipboardFileInfo[]) || [];
        if (files.length > 0) {
          this.cb.onClipboardFiles(files);
        }
        break;
      }

      case "file_list_response": {
        this.cb.onFileList({
          currentPath: (msg.current_path as string) || "",
          parentPath: (msg.parent_path as string | null) ?? null,
          entries: (msg.entries as FileEntry[]) || [],
          error: (msg.error as string | null) ?? null,
        });
        break;
      }

      case "file_chunk": {
        const tid = msg.transfer_id as string;
        const fileName = msg.file_name as string;
        const totalSize = Number(msg.total_size || 0);
        const isLast = Boolean(msg.is_last);
        const dataB64 = (msg.data as string) || "";
        const chunkBytes = dataB64 ? base64ToBytes(dataB64) : new Uint8Array(0);

        let dl = this.downloads.get(tid);
        if (!dl) {
          dl = { fileName, totalSize, chunks: [], received: 0 };
          this.downloads.set(tid, dl);
        }
        dl.chunks.push(chunkBytes);
        dl.received += chunkBytes.length;

        if (isLast) {
          this.downloads.delete(tid);
          const blob = new Blob(dl.chunks as BlobPart[]);
          this.cb.onFileDownloadComplete(dl.fileName, blob);
          this.cb.onFileStatus(true, `Descargado: ${dl.fileName}`);
        }
        break;
      }

      case "file_transfer_status": {
        this.cb.onFileStatus(Boolean(msg.ok), String(msg.message || ""));
        break;
      }
    }
  }

  private async sendEncrypted(inner: Record<string, unknown>) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN || !this.e2e || !this.sessionId) {
      return;
    }
    const payload = await this.e2e.encryptJson(inner);
    this.ws.send(
      JSON.stringify({
        type: "encrypted_relay",
        session_id: this.sessionId,
        payload,
      })
    );
  }

  sendMouseMove(x: number, y: number) {
    void this.sendEncrypted({ kind: "mouse_move", x, y });
  }

  sendMouseButton(
    button: "left" | "right" | "middle",
    pressed: boolean,
    x: number,
    y: number
  ) {
    void this.sendEncrypted({ kind: "mouse_button", button, pressed, x, y });
  }

  sendMouseScroll(deltaX: number, deltaY: number) {
    void this.sendEncrypted({ kind: "mouse_scroll", delta_x: deltaX, delta_y: deltaY });
  }

  sendKeyEvent(code: string, key: string, pressed: boolean) {
    void this.sendEncrypted({ kind: "key_event", code, key, pressed });
  }

  sendStreamConfig(fps: number, quality: number, displayIndex: number, scale: number) {
    void this.sendEncrypted({
      kind: "stream_config",
      fps,
      quality,
      display_index: displayIndex,
      scale,
    });
  }

  sendClipboard(text: string) {
    void this.sendEncrypted({ kind: "clipboard_sync", text });
  }

  requestFileList(path?: string) {
    void this.sendEncrypted({ kind: "file_list_request", path: path ?? null });
  }

  requestFileDownload(path: string) {
    void this.sendEncrypted({ kind: "file_download_request", path });
  }

  async uploadFile(file: File, targetDir?: string) {
    const transferId = crypto.randomUUID();
    const chunkSize = 128 * 1024;
    const totalSize = file.size;
    let offset = 0;

    if (totalSize === 0) {
      await this.sendEncrypted({
        kind: "file_chunk",
        transfer_id: transferId,
        file_name: file.name,
        target_dir: targetDir ?? null,
        offset: 0,
        total_size: 0,
        data: "",
        is_last: true,
      });
      return;
    }

    while (offset < totalSize) {
      const slice = file.slice(offset, offset + chunkSize);
      const buf = new Uint8Array(await slice.arrayBuffer());
      const isLast = offset + buf.length >= totalSize;
      await this.sendEncrypted({
        kind: "file_chunk",
        transfer_id: transferId,
        file_name: file.name,
        target_dir: targetDir ?? null,
        offset,
        total_size: totalSize,
        data: bytesToBase64(buf),
        is_last: isLast,
      });
      offset += buf.length;
    }
  }

  /**
   * Upload a file pasted with Ctrl+V (or dragged onto viewer) and place it on the Host clipboard (CF_HDROP).
   */
  async pasteFileToHost(file: File, pasteAfterReceive: boolean) {
    const transferId = crypto.randomUUID();
    const chunkSize = 128 * 1024;
    const totalSize = file.size;
    let offset = 0;

    if (totalSize === 0) {
      await this.sendEncrypted({
        kind: "clipboard_file_paste",
        transfer_id: transferId,
        file_name: file.name,
        offset: 0,
        total_size: 0,
        data: "",
        is_last: true,
        paste_after_receive: pasteAfterReceive,
      });
      return;
    }

    while (offset < totalSize) {
      const slice = file.slice(offset, offset + chunkSize);
      const buf = new Uint8Array(await slice.arrayBuffer());
      const isLast = offset + buf.length >= totalSize;
      await this.sendEncrypted({
        kind: "clipboard_file_paste",
        transfer_id: transferId,
        file_name: file.name,
        offset,
        total_size: totalSize,
        data: bytesToBase64(buf),
        is_last: isLast,
        paste_after_receive: pasteAfterReceive,
      });
      offset += buf.length;
    }
  }
}
