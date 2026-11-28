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
const PACKET_VIDEO_H264 = 0x04;

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
  onVideoFrame: (frame: VideoFrame) => void;
  onBitmapFrame?: (bitmap: ImageBitmap, width: number, height: number) => void;
  onDecoderError?: (message: string) => void;
  onTransportChange?: (mode: "p2p" | "relay") => void;
  onMetrics: (metrics: { rttMs?: number; bytesReceived?: number }) => void;
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
  private pingTimer: number | null = null;
  private wsKeepaliveTimer: number | null = null;
  private reconnectTimer: number | null = null;
  private manualDisconnect = false;
  private reconnectAttempts = 0;
  private lastConnectParams: {
    serverUrl: string;
    targetDeviceId: string;
    viewerName: string;
    pin?: string;
    authToken?: string | null;
    preferP2p?: boolean;
  } | null = null;

  // WebCodecs hardware-accelerated H.264 decoder
  private videoDecoder: VideoDecoder | null = null;
  private awaitingKeyframe = true;
  private forceSoftwareDecoder = false;
  private lastVideoW = 0;
  private lastVideoH = 0;

  // Ordered crypto/decode queues (prevents H.264 frame & input reordering under async WebCrypto)
  private binaryQueue: Promise<void> = Promise.resolve();
  private sendQueue: Promise<void> = Promise.resolve();

  // Direct WebRTC P2P DataChannels (LAN / STUN UDP, automatic fallback to WS relay)
  private preferP2p = true;
  private rtcPc: RTCPeerConnection | null = null;
  private rtcControlDc: RTCDataChannel | null = null;
  private rtcMediaDc: RTCDataChannel | null = null;
  private p2pControlReady = false;
  private p2pMediaReady = false;
  private rtcRemoteDescSet = false;
  private pendingRemoteIce: RTCIceCandidateInit[] = [];
  private p2pChunks = new Map<
    number,
    { total: number; received: number; parts: Array<Uint8Array | null> }
  >();

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

  private async initVideoDecoder() {
    this.closeVideoDecoder();
    if (typeof VideoDecoder === "undefined") {
      this.cb.onStatus(
        "disconnected",
        "Este navegador no soporta WebCodecs H.264. Usa Edge, Chrome o la app de escritorio."
      );
      return;
    }

    // Probe supported Annex-B H.264 configs (hardware first unless fallback triggered)
    const hwCandidates: Array<VideoDecoderConfig & { avc?: { format: "annexb" | "avc" } }> = [
      {
        codec: "avc1.42E033",
        avc: { format: "annexb" },
        hardwareAcceleration: "prefer-hardware",
        optimizeForLatency: true,
      },
      {
        codec: "avc1.420033",
        avc: { format: "annexb" },
        hardwareAcceleration: "prefer-hardware",
        optimizeForLatency: true,
      },
    ];
    const swCandidates: Array<VideoDecoderConfig & { avc?: { format: "annexb" | "avc" } }> = [
      {
        codec: "avc1.42E02A",
        avc: { format: "annexb" },
        hardwareAcceleration: "prefer-software",
        optimizeForLatency: true,
      },
      {
        codec: "avc1.42001F",
        avc: { format: "annexb" },
        hardwareAcceleration: "prefer-software",
        optimizeForLatency: true,
      },
    ];
    const candidates = this.forceSoftwareDecoder
      ? swCandidates
      : [...hwCandidates, ...swCandidates];

    let chosen: VideoDecoderConfig | null = null;
    for (const cfg of candidates) {
      try {
        const res = await VideoDecoder.isConfigSupported(cfg);
        if (res.supported) {
          chosen = cfg;
          break;
        }
      } catch {
        // try next candidate
      }
    }

    if (!chosen) {
      chosen = {
        codec: "avc1.42001F",
        optimizeForLatency: true,
      };
    }

    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        this.cb.onVideoFrame(frame);
      },
      error: (err) => {
        console.warn("WebCodecs VideoDecoder error, falling back to software:", err);
        this.forceSoftwareDecoder = true;
        this.cb.onDecoderError?.(`VideoDecoder fallback SW: ${String(err)}`);
        this.awaitingKeyframe = true;
        void this.initVideoDecoder().then(() => this.requestKeyframe());
      },
    });

    decoder.configure(chosen);
    this.videoDecoder = decoder;
    this.awaitingKeyframe = true;
  }

  private closeVideoDecoder() {
    if (this.videoDecoder) {
      try {
        if (this.videoDecoder.state !== "closed") {
          this.videoDecoder.close();
        }
      } catch {
        // ignore
      }
      this.videoDecoder = null;
    }
    this.awaitingKeyframe = true;
  }

  async connect(params: {
    serverUrl: string;
    targetDeviceId: string;
    viewerName: string;
    pin?: string;
    authToken?: string | null;
    preferP2p?: boolean;
  }): Promise<void> {
    this.cleanupConnectionOnly();
    this.manualDisconnect = false;
    this.lastConnectParams = params;
    this.preferP2p = params.preferP2p ?? true;
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

      // Measure RTT ping every 2.5 seconds (uses P2P control channel when active)
      this.pingTimer = window.setInterval(() => {
        if (this.p2pControlReady && this.rtcControlDc?.readyState === "open") {
          try {
            this.rtcControlDc.send(
              JSON.stringify({ kind: "p2p_ping", timestamp: Date.now() })
            );
            return;
          } catch {
            // fallback to ws ping
          }
        }
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
          this.ws.send(JSON.stringify({ type: "ping", timestamp: Date.now() }));
        }
      }, 2500);

      // Dedicated WebSocket keepalive every 15s so Cloudflare/proxies NEVER drop the
      // signaling socket for idle timeout while traffic flows over direct WebRTC P2P!
      this.wsKeepaliveTimer = window.setInterval(() => {
        if (this.ws && this.ws.readyState === WebSocket.OPEN) {
          this.ws.send(JSON.stringify({ type: "ping", timestamp: Date.now() }));
        }
      }, 15000);
    };

    ws.onmessage = (event) => {
      if (typeof event.data === "string") {
        void this.handleTextMessage(event.data);
      } else if (event.data instanceof ArrayBuffer) {
        const buf = event.data;
        this.binaryQueue = this.binaryQueue
          .then(() => this.handleBinaryPacket(buf))
          .catch(() => {});
      }
    };

    ws.onerror = () => {
      // onclose handles reconnect or final disconnect
    };

    ws.onclose = () => {
      if (this.ws === ws) {
        this.ws = null;
        this.e2e = null;
        this.closeP2p();
        this.closeVideoDecoder();
        if (this.pingTimer !== null) {
          window.clearInterval(this.pingTimer);
          this.pingTimer = null;
        }
        if (this.wsKeepaliveTimer !== null) {
          window.clearInterval(this.wsKeepaliveTimer);
          this.wsKeepaliveTimer = null;
        }
        if (!this.manualDisconnect && this.lastConnectParams && this.reconnectAttempts < 4) {
          this.reconnectAttempts++;
          const delayMs = this.reconnectAttempts * 1500;
          this.cb.onStatus(
            "connecting",
            `Reconectando automáticamente (intento ${this.reconnectAttempts}/4)...`
          );
          const paramsCopy = this.lastConnectParams;
          this.reconnectTimer = window.setTimeout(() => {
            this.reconnectTimer = null;
            if (!this.manualDisconnect) {
              void this.connect(paramsCopy);
            }
          }, delayMs);
        } else {
          this.cb.onStatus("disconnected", "Sesión finalizada");
        }
      }
    };
  }

  private cleanupConnectionOnly() {
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.pingTimer !== null) {
      window.clearInterval(this.pingTimer);
      this.pingTimer = null;
    }
    if (this.wsKeepaliveTimer !== null) {
      window.clearInterval(this.wsKeepaliveTimer);
      this.wsKeepaliveTimer = null;
    }
    if (this.ws) {
      const old = this.ws;
      this.ws = null;
      old.close();
    }
    this.closeP2p();
    this.closeVideoDecoder();
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

  disconnect(reason?: string) {
    this.manualDisconnect = true;
    this.reconnectAttempts = 0;
    if (this.ws && this.ws.readyState === WebSocket.OPEN && this.sessionId) {
      this.ws.send(
        JSON.stringify({
          type: "disconnect",
          session_id: this.sessionId,
          reason: reason ?? "Viewer closed session",
        })
      );
    }
    this.cleanupConnectionOnly();
  }

  private async handleTextMessage(raw: string) {
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(raw);
    } catch {
      return;
    }

    switch (msg.type) {
      case "pong": {
        // Keep displaying direct P2P RTT when P2P control channel is active
        if (!this.p2pControlReady) {
          const sentTs = Number(msg.timestamp || 0);
          if (sentTs > 0) {
            this.cb.onMetrics({ rttMs: Math.max(1, Date.now() - sentTs) });
          }
        }
        break;
      }

      case "session_ready": {
        const accepted = Boolean(msg.accepted);
        if (!accepted) {
          this.manualDisconnect = true;
          this.cb.onStatus(
            "disconnected",
            (msg.reason as string) || "Conexión rechazada por el equipo remoto"
          );
          this.disconnect();
          return;
        }
        this.reconnectAttempts = 0;
        const sessionId = msg.session_id as string;
        const hostPub = msg.host_public_key as string;
        if (!this.keyPair || !hostPub) {
          this.cb.onStatus("disconnected", "Error en intercambio de claves E2E");
          this.disconnect();
          return;
        }
        this.sessionId = sessionId;
        this.e2e = await this.keyPair.deriveSession(hostPub, sessionId);
        await this.initVideoDecoder();
        // Always negotiate direct WebRTC P2P — server is used strictly for registration & signaling
        void this.startP2pNegotiation();

        const permissions: SessionPermissions =
          (msg.permissions as SessionPermissions) || { ...DEFAULT_PERMISSIONS };
        this.cb.onStatus("connecting", "Estableciendo túnel P2P directo...");
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
        this.manualDisconnect = true;
        this.cb.onStatus(
          "disconnected",
          (msg.reason as string) || "El equipo remoto cerró la sesión"
        );
        this.disconnect();
        break;
      }

      case "error": {
        this.manualDisconnect = true;
        this.cb.onStatus(
          "disconnected",
          (msg.message as string) || "Error del servidor"
        );
        this.disconnect();
        break;
      }
    }
  }

  private closeP2p() {
    const wasP2p = this.p2pMediaReady || this.p2pControlReady;
    this.p2pControlReady = false;
    this.p2pMediaReady = false;
    this.rtcRemoteDescSet = false;
    this.pendingRemoteIce = [];
    this.p2pChunks.clear();
    if (this.rtcControlDc) {
      try {
        this.rtcControlDc.close();
      } catch {
        // ignore
      }
      this.rtcControlDc = null;
    }
    if (this.rtcMediaDc) {
      try {
        this.rtcMediaDc.close();
      } catch {
        // ignore
      }
      this.rtcMediaDc = null;
    }
    if (this.rtcPc) {
      try {
        this.rtcPc.close();
      } catch {
        // ignore
      }
      this.rtcPc = null;
    }
    if (wasP2p) {
      this.cb.onTransportChange?.("relay");
    }
  }

  private async startP2pNegotiation() {
    if (typeof RTCPeerConnection === "undefined") return;
    this.closeP2p();

    try {
      const pc = new RTCPeerConnection({
        iceServers: [
          {
            urls: [
              "stun:stun.cloudflare.com:3478",
              "stun:stun.l.google.com:19302",
            ],
          },
        ],
      });
      this.rtcPc = pc;

      pc.onicecandidate = (ev) => {
        if (ev.candidate && ev.candidate.candidate) {
          void this.sendEncryptedViaWs({
            kind: "rtc_ice_candidate",
            candidate: ev.candidate.candidate,
            sdp_mid: ev.candidate.sdpMid ?? null,
            sdp_mline_index: ev.candidate.sdpMLineIndex ?? null,
          });
        }
      };

      pc.onconnectionstatechange = () => {
        if (
          pc.connectionState === "failed" ||
          pc.connectionState === "disconnected" ||
          pc.connectionState === "closed"
        ) {
          if (this.rtcPc === pc) {
            this.closeP2p();
          }
        }
      };

      const controlDc = pc.createDataChannel("control", { ordered: true });
      this.rtcControlDc = controlDc;
      controlDc.onopen = () => {
        if (this.rtcControlDc !== controlDc) return;
        this.p2pControlReady = true;
        // Immediate P2P RTT probe
        try {
          controlDc.send(JSON.stringify({ kind: "p2p_ping", timestamp: Date.now() }));
        } catch {
          // ignore
        }
      };
      controlDc.onclose = () => {
        if (this.rtcControlDc === controlDc) {
          this.p2pControlReady = false;
        }
      };
      controlDc.onmessage = (ev) => {
        if (typeof ev.data === "string") {
          try {
            const inner = JSON.parse(ev.data) as Record<string, unknown>;
            this.handleSessionMessage(inner);
          } catch {
            // ignore
          }
        }
      };

      const mediaDc = pc.createDataChannel("media", { ordered: true });
      mediaDc.binaryType = "arraybuffer";
      this.rtcMediaDc = mediaDc;
      mediaDc.onopen = () => {
        if (this.rtcMediaDc !== mediaDc) return;
        this.p2pMediaReady = true;
        this.cb.onTransportChange?.("p2p");
        this.cb.onStatus("connected", "P2P Directo (DTLS/UDP) activo");
        this.requestKeyframe();
      };
      mediaDc.onclose = () => {
        if (this.rtcMediaDc === mediaDc) {
          this.p2pMediaReady = false;
          this.cb.onTransportChange?.("relay");
        }
      };
      mediaDc.onmessage = (ev) => {
        if (ev.data instanceof ArrayBuffer) {
          this.handleP2pMediaChunk(ev.data);
        }
      };

      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      if (offer.sdp) {
        await this.sendEncryptedViaWs({ kind: "rtc_offer", sdp: offer.sdp });
      }
    } catch (err) {
      console.warn("WebRTC P2P negotiation failed (using relay):", err);
      this.closeP2p();
    }
  }

  private handleP2pMediaChunk(buffer: ArrayBuffer) {
    this.cb.onMetrics({ bytesReceived: buffer.byteLength });
    const bytes = new Uint8Array(buffer);
    if (bytes.length <= 6) return;

    const view = new DataView(bytes.buffer, bytes.byteOffset, 6);
    const msgId = view.getUint16(0, false);
    const chunkIdx = view.getUint16(2, false);
    const totalChunks = view.getUint16(4, false);
    const payload = bytes.subarray(6);

    if (totalChunks <= 1) {
      void this.handlePlainMediaPacket(payload);
      return;
    }

    if (chunkIdx >= totalChunks) return;

    let entry = this.p2pChunks.get(msgId);
    if (!entry) {
      if (this.p2pChunks.size > 8) {
        const oldestKey = this.p2pChunks.keys().next().value;
        if (oldestKey !== undefined) this.p2pChunks.delete(oldestKey);
      }
      entry = {
        total: totalChunks,
        received: 0,
        parts: new Array<Uint8Array | null>(totalChunks).fill(null),
      };
      this.p2pChunks.set(msgId, entry);
    }

    if (entry.parts[chunkIdx] === null) {
      // Copy chunk slice because underlying buffer may be reused
      entry.parts[chunkIdx] = new Uint8Array(payload);
      entry.received++;
    }

    if (entry.received === entry.total) {
      this.p2pChunks.delete(msgId);
      let totalLen = 0;
      for (const p of entry.parts) {
        if (p) totalLen += p.length;
      }
      const full = new Uint8Array(totalLen);
      let offset = 0;
      for (const p of entry.parts) {
        if (p) {
          full.set(p, offset);
          offset += p.length;
        }
      }
      void this.handlePlainMediaPacket(full);
    }
  }

  private async handleBinaryPacket(buffer: ArrayBuffer) {
    if (!this.e2e) return;
    this.cb.onMetrics({ bytesReceived: buffer.byteLength });

    const bytes = new Uint8Array(buffer);
    if (bytes.length <= 36 + 28) return;

    const encrypted = bytes.subarray(36);
    let plain: Uint8Array;
    try {
      plain = await this.e2e.decryptBytes(encrypted);
    } catch {
      return;
    }
    await this.handlePlainMediaPacket(plain);
  }

  private async handlePlainMediaPacket(plain: Uint8Array) {
    if (plain.length < 2) return;

    const packetType = plain[0];

    if (packetType === PACKET_VIDEO_JPEG) {
      // Backward compatibility if the remote host is running an older JPEG build
      if (plain.length <= 9 || !this.cb.onBitmapFrame) return;
      try {
        const view = new DataView(plain.buffer, plain.byteOffset + 1, 8);
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
        this.cb.onBitmapFrame(bitmap, width, height);
      } catch {
        // ignore
      }
      return;
    }

    if (packetType === PACKET_VIDEO_H264) {
      if (plain.length <= 14 || !this.videoDecoder || this.videoDecoder.state !== "configured") {
        return;
      }
      const isKeyframe = plain[1] === 1;
      const view = new DataView(plain.buffer, plain.byteOffset + 2, 12);
      const width = view.getUint32(0, false);
      const height = view.getUint32(4, false);
      const timestampMs = view.getUint32(8, false);
      const h264Data = plain.subarray(14);

      if (width !== this.lastVideoW || height !== this.lastVideoH) {
        this.lastVideoW = width;
        this.lastVideoH = height;
        if (!isKeyframe) {
          this.awaitingKeyframe = true;
          this.requestKeyframe();
          return;
        }
      }

      if (this.awaitingKeyframe) {
        if (!isKeyframe) return;
        this.awaitingKeyframe = false;
      }

      // Prevent decoder queue buildup for ultra-low latency
      if (this.videoDecoder.decodeQueueSize > 3 && !isKeyframe) {
        this.awaitingKeyframe = true;
        this.requestKeyframe();
        return;
      }

      try {
        const chunk = new EncodedVideoChunk({
          type: isKeyframe ? "key" : "delta",
          timestamp: timestampMs * 1000, // microseconds
          data: h264Data,
        });
        this.videoDecoder.decode(chunk);
      } catch {
        this.awaitingKeyframe = true;
        this.requestKeyframe();
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

      case "rtc_answer": {
        const sdp = msg.sdp as string | undefined;
        if (sdp && this.rtcPc) {
          void this.rtcPc
            .setRemoteDescription({ type: "answer", sdp })
            .then(async () => {
              this.rtcRemoteDescSet = true;
              const queued = this.pendingRemoteIce.splice(0);
              for (const cand of queued) {
                await this.rtcPc?.addIceCandidate(cand).catch(() => {});
              }
            })
            .catch(() => {
              this.closeP2p();
            });
        }
        break;
      }

      case "rtc_ice_candidate": {
        const candidate = msg.candidate as string | undefined;
        if (candidate && this.rtcPc) {
          const init: RTCIceCandidateInit = {
            candidate,
            sdpMid: (msg.sdp_mid as string | null) ?? undefined,
            sdpMLineIndex: (msg.sdp_mline_index as number | null) ?? undefined,
          };
          if (!this.rtcRemoteDescSet) {
            this.pendingRemoteIce.push(init);
          } else {
            void this.rtcPc.addIceCandidate(init).catch(() => {});
          }
        }
        break;
      }

      case "p2p_pong": {
        const sentTs = Number(msg.timestamp || 0);
        if (sentTs > 0) {
          this.cb.onMetrics({ rttMs: Math.max(1, Date.now() - sentTs) });
        }
        break;
      }
    }
  }

  private sendEncryptedViaWs(inner: Record<string, unknown>): Promise<void> {
    const task = this.sendQueue.then(async () => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN || !this.e2e || !this.sessionId) {
        return;
      }
      const payload = await this.e2e.encryptJson(inner);
      if (this.ws && this.ws.readyState === WebSocket.OPEN && this.sessionId) {
        this.ws.send(
          JSON.stringify({
            type: "encrypted_relay",
            session_id: this.sessionId,
            payload,
          })
        );
      }
    });
    this.sendQueue = task.catch(() => {});
    return task;
  }

  private sendEncrypted(inner: Record<string, unknown>): Promise<void> {
    if (this.p2pControlReady && this.rtcControlDc?.readyState === "open") {
      try {
        this.rtcControlDc.send(JSON.stringify(inner));
        return Promise.resolve();
      } catch {
        this.p2pControlReady = false;
      }
    }
    // Strictly P2P-only for session control/input/files (do not relay data over server)
    return Promise.resolve();
  }

  requestKeyframe() {
    void this.sendEncrypted({ kind: "request_keyframe" });
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

  sendStreamConfig(
    fps: number,
    bitrateKbps: number,
    displayIndex: number,
    scale: number,
    showCursor = false
  ) {
    void this.sendEncrypted({
      kind: "stream_config",
      fps,
      quality: 75,
      bitrate_kbps: bitrateKbps,
      display_index: displayIndex,
      scale,
      show_cursor: showCursor,
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
