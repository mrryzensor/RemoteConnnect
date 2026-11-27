import { useEffect, useState } from "react";
import {
  Check,
  ExternalLink,
  FolderOpen,
  Monitor,
  RotateCcw,
  Server,
  Settings,
  Sliders,
  Video,
  X,
} from "lucide-react";
import {
  DEFAULT_PREFERENCES,
  type AppConfig,
  type UserPreferences,
} from "../types";
import {
  createDesktopShortcut,
  hasDesktopShortcut,
  isTauriEnv,
  openFolder,
  relaunchWithUiMode,
} from "../lib/tauriBridge";

interface Props {
  config: AppConfig;
  appVersion: string;
  theme: "dark" | "light";
  autostartEnabled: boolean;
  onToggleTheme: () => void;
  onToggleAutostart: (next: boolean) => Promise<void>;
  onCheckUpdates: () => Promise<string>;
  onSavePreferences: (prefs: UserPreferences) => Promise<void>;
  onSaveServer: (
    serverUrl: string,
    authToken: string | null,
    deviceName: string
  ) => Promise<void>;
  onClose: () => void;
}

type TabId = "general" | "media" | "server";

export function SettingsModal({
  config,
  appVersion,
  theme,
  autostartEnabled,
  onToggleTheme,
  onToggleAutostart,
  onCheckUpdates,
  onSavePreferences,
  onSaveServer,
  onClose,
}: Props) {
  const initialPrefs: UserPreferences = {
    ...DEFAULT_PREFERENCES,
    ...(config.preferences ?? {}),
  };

  const [tab, setTab] = useState<TabId>("general");
  const [prefs, setPrefs] = useState<UserPreferences>(initialPrefs);
  const [serverUrl, setServerUrl] = useState(config.server_url);
  const [deviceName, setDeviceName] = useState(config.device_name);
  const [authToken, setAuthToken] = useState(config.auth_token ?? "");
  const [saving, setSaving] = useState(false);
  const [shortcutExists, setShortcutExists] = useState(true);
  const [creatingShortcut, setCreatingShortcut] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);

  useEffect(() => {
    if (isTauriEnv()) {
      void hasDesktopShortcut().then(setShortcutExists);
    }
  }, []);

  const handleSave = async () => {
    setSaving(true);
    try {
      await onSavePreferences(prefs);
      const serverChanged =
        serverUrl.trim() !== config.server_url ||
        deviceName.trim() !== config.device_name ||
        (authToken.trim() || null) !== (config.auth_token ?? null);
      if (serverChanged) {
        await onSaveServer(
          serverUrl.trim() || config.server_url,
          authToken.trim() || null,
          deviceName.trim() || config.device_name
        );
      }
      onClose();
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal-card wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3>
            <Settings size={18} color="var(--accent)" />
            Configuración
          </h3>
          <button className="icon-btn" onClick={onClose} title="Cerrar">
            <X size={18} />
          </button>
        </div>

        <div className="settings-tabs">
          <button
            className={`settings-tab ${tab === "general" ? "active" : ""}`}
            onClick={() => setTab("general")}
          >
            <Monitor size={14} />
            General y Sistema
          </button>
          <button
            className={`settings-tab ${tab === "media" ? "active" : ""}`}
            onClick={() => setTab("media")}
          >
            <Video size={14} />
            Grabaciones y Video
          </button>
          <button
            className={`settings-tab ${tab === "server" ? "active" : ""}`}
            onClick={() => setTab("server")}
          >
            <Server size={14} />
            Servidor y Red
          </button>
        </div>

        {tab === "general" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {isTauriEnv() && (
              <>
                <div className="card" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
                  <label className="toggle-row">
                    <div>
                      <strong>Iniciar automáticamente con Windows</strong>
                      <div className="toggle-desc">
                        Se inicia oculto en el System Tray para poder conectarte siempre
                      </div>
                    </div>
                    <span className="switch">
                      <input
                        type="checkbox"
                        checked={autostartEnabled}
                        onChange={(e) => void onToggleAutostart(e.target.checked)}
                      />
                      <span className="switch-slider" />
                    </span>
                  </label>

                  <label className="toggle-row">
                    <div>
                      <strong>Minimizar al System Tray al cerrar (X)</strong>
                      <div className="toggle-desc">
                        Mantiene este equipo disponible en segundo plano al cerrar la ventana
                      </div>
                    </div>
                    <span className="switch">
                      <input
                        type="checkbox"
                        checked={prefs.minimize_to_tray}
                        onChange={(e) =>
                          setPrefs((p) => ({ ...p, minimize_to_tray: e.target.checked }))
                        }
                      />
                      <span className="switch-slider" />
                    </span>
                  </label>

                  <label className="toggle-row">
                    <div>
                      <strong>Mostrar widget flotante al recibir conexión</strong>
                      <div className="toggle-desc">
                        Muestra una barra minimalista cuando otra PC controla este equipo
                      </div>
                    </div>
                    <span className="switch">
                      <input
                        type="checkbox"
                        checked={prefs.show_host_widget}
                        onChange={(e) =>
                          setPrefs((p) => ({ ...p, show_host_widget: e.target.checked }))
                        }
                      />
                      <span className="switch-slider" />
                    </span>
                  </label>
                </div>

                <div className="card" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                  <div className="field">
                    <label>Modo de interfaz de escritorio</label>
                    <div style={{ display: "flex", gap: 8 }}>
                      <select
                        style={{ flex: 1 }}
                        value={prefs.ui_mode}
                        onChange={(e) =>
                          setPrefs((p) => ({
                            ...p,
                            ui_mode: e.target.value as "auto" | "webview" | "native",
                          }))
                        }
                      >
                        <option value="auto">Automático (WebView2 si existe, sino Nativo Win32)</option>
                        <option value="webview">WebView2 (Interfaz completa con visor integrado)</option>
                        <option value="native">Nativo Win32 (Ultra ligero, sin WebView2)</option>
                      </select>
                      <button
                        type="button"
                        className="btn btn-secondary btn-sm"
                        onClick={() => void relaunchWithUiMode(prefs.ui_mode)}
                        title="Guardar modo y reiniciar RemoteConnect ahora"
                      >
                        Aplicar y reiniciar
                      </button>
                    </div>
                    <div className="toggle-desc">
                      Permite alternar entre la interfaz WebView2 y el modo nativo Win32 sin WebView2.
                    </div>
                  </div>
                </div>

                {!shortcutExists ? (
                  <div className="card">
                    <div className="toggle-row">
                      <div>
                        <strong>Acceso directo en el Escritorio</strong>
                        <div className="toggle-desc">
                          No se detectó acceso directo en tu Escritorio; créalo en un clic
                        </div>
                      </div>
                      <button
                        type="button"
                        className="btn btn-primary btn-sm"
                        disabled={creatingShortcut}
                        onClick={async () => {
                          setCreatingShortcut(true);
                          try {
                            await createDesktopShortcut();
                            setShortcutExists(true);
                          } finally {
                            setCreatingShortcut(false);
                          }
                        }}
                      >
                        <ExternalLink size={13} />
                        {creatingShortcut ? "Creando..." : "Crear acceso directo"}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      fontSize: 12,
                      color: "var(--success-text)",
                      padding: "0 4px",
                    }}
                  >
                    <Check size={14} />
                    Acceso directo en el Escritorio ya configurado
                  </div>
                )}
              </>
            )}

            <div className="card">
              <label className="toggle-row">
                <div>
                  <strong>Tema oscuro</strong>
                  <div className="toggle-desc">
                    Alterna entre apariencia oscura y clara
                  </div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={theme === "dark"}
                    onChange={onToggleTheme}
                  />
                  <span className="switch-slider" />
                </span>
              </label>
            </div>

            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <div className="toggle-row">
                <div>
                  <strong>Versión instalada: v{appVersion}</strong>
                  <div className="toggle-desc">
                    Actualizaciones automáticas activas desde GitHub Releases
                  </div>
                </div>
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  disabled={checkingUpdate}
                  onClick={async () => {
                    setCheckingUpdate(true);
                    setUpdateMessage(null);
                    try {
                      const msg = await onCheckUpdates();
                      setUpdateMessage(msg);
                    } finally {
                      setCheckingUpdate(false);
                    }
                  }}
                >
                  <RotateCcw size={13} />
                  {checkingUpdate ? "Buscando..." : "Buscar actualizaciones"}
                </button>
              </div>
              {updateMessage && (
                <div style={{ fontSize: 12, color: "var(--accent)", fontWeight: 500 }}>
                  {updateMessage}
                </div>
              )}
            </div>
          </div>
        )}

        {tab === "media" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {isTauriEnv() && (
              <div className="card" style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                <div className="field">
                  <label>Carpeta de grabaciones locales (.webm)</label>
                  <div style={{ display: "flex", gap: 8 }}>
                    <input
                      style={{ flex: 1, fontFamily: "JetBrains Mono, monospace", fontSize: 12.5 }}
                      value={prefs.recordings_dir}
                      onChange={(e) =>
                        setPrefs((p) => ({ ...p, recordings_dir: e.target.value }))
                      }
                      placeholder="C:\Users\...\Videos\RemoteConnect"
                    />
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      onClick={() => void openFolder(prefs.recordings_dir)}
                      title="Abrir carpeta en el Explorador"
                    >
                      <FolderOpen size={14} />
                      Abrir
                    </button>
                    <button
                      type="button"
                      className="icon-btn"
                      onClick={() => setPrefs((p) => ({ ...p, recordings_dir: "" }))}
                      title="Restaurar carpeta predeterminada al guardar"
                    >
                      <RotateCcw size={15} />
                    </button>
                  </div>
                  <div className="toggle-desc">
                    Las grabaciones se guardan únicamente en esta PC (nunca en el servidor).
                  </div>
                </div>
              </div>
            )}

            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, fontWeight: 700, color: "var(--text-secondary)" }}>
                <Sliders size={13} />
                Calidad predeterminada al conectar
              </div>

              <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 12 }}>
                <div className="field">
                  <label>FPS ({prefs.default_fps})</label>
                  <select
                    value={prefs.default_fps}
                    onChange={(e) =>
                      setPrefs((p) => ({ ...p, default_fps: Number(e.target.value) }))
                    }
                  >
                    <option value={15}>15 FPS (Ahorro)</option>
                    <option value={30}>30 FPS (Equilibrado)</option>
                    <option value={60}>60 FPS (Ultra Fluido)</option>
                  </select>
                </div>

                <div className="field">
                  <label>Bitrate H.264</label>
                  <select
                    value={prefs.default_bitrate_kbps}
                    onChange={(e) =>
                      setPrefs((p) => ({
                        ...p,
                        default_bitrate_kbps: Number(e.target.value),
                      }))
                    }
                  >
                    <option value={1500}>1.5 Mbps</option>
                    <option value={2500}>2.5 Mbps (Recomendado)</option>
                    <option value={4000}>4.0 Mbps (Alta)</option>
                    <option value={5500}>5.5 Mbps (Máxima)</option>
                  </select>
                </div>

                <div className="field">
                  <label>Escala</label>
                  <select
                    value={Math.round(prefs.default_scale * 100)}
                    onChange={(e) =>
                      setPrefs((p) => ({
                        ...p,
                        default_scale: Number(e.target.value) / 100,
                      }))
                    }
                  >
                    <option value={75}>75%</option>
                    <option value={85}>85%</option>
                    <option value={100}>100% (Nativa)</option>
                  </select>
                </div>
              </div>

              <label className="toggle-row">
                <div>
                  <strong>Preferir conexión directa P2P (WebRTC)</strong>
                  <div className="toggle-desc">
                    Conecta directo entre PCs para mínima latencia, con respaldo automático al servidor
                  </div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={prefs.prefer_p2p}
                    onChange={(e) =>
                      setPrefs((p) => ({ ...p, prefer_p2p: e.target.checked }))
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>

              <label className="toggle-row">
                <div>
                  <strong>Iniciar sesiones con audio silenciado</strong>
                  <div className="toggle-desc">
                    Puedes activar el audio en cualquier momento desde la barra del visor
                  </div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={prefs.mute_audio_on_connect}
                    onChange={(e) =>
                      setPrefs((p) => ({ ...p, mute_audio_on_connect: e.target.checked }))
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>

              <div className="field">
                <label>Gesto de clic derecho en Android / táctil</label>
                <select
                  value={prefs.right_click_gesture}
                  onChange={(e) =>
                    setPrefs((p) => ({
                      ...p,
                      right_click_gesture: e.target.value as "long_press" | "two_finger_tap",
                    }))
                  }
                >
                  <option value="long_press">Mantener presionado (predeterminado)</option>
                  <option value="two_finger_tap">Toque rápido con 2 dedos</option>
                </select>
              </div>
            </div>
          </div>
        )}

        {tab === "server" && (
          <div className="card" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <div className="field">
              <label>Nombre de este equipo</label>
              <input
                value={deviceName}
                onChange={(e) => setDeviceName(e.target.value)}
                placeholder="Mi-PC-Oficina"
              />
            </div>
            <div className="field">
              <label>URL WebSocket del Servidor</label>
              <input
                value={serverUrl}
                onChange={(e) => setServerUrl(e.target.value)}
                placeholder="wss://remote.2mail.us/ws"
              />
            </div>
            <div className="field">
              <label>Token del servidor (opcional)</label>
              <input
                type="password"
                value={authToken}
                onChange={(e) => setAuthToken(e.target.value)}
                placeholder="Dejar vacío si el servidor es abierto"
              />
            </div>
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 10 }}>
          <button className="btn btn-secondary" onClick={onClose}>
            Cancelar
          </button>
          <button className="btn btn-primary" disabled={saving} onClick={() => void handleSave()}>
            {saving ? "Guardando..." : "Guardar configuración"}
          </button>
        </div>
      </div>
    </div>
  );
}
