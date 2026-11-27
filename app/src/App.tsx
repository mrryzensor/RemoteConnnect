import { useEffect, useMemo, useState } from "react";
import {
  Check,
  Clipboard,
  Copy,
  Eye,
  EyeOff,
  FolderOpen,
  KeyRound,
  Laptop,
  Lock,
  Menu,
  Moon,
  MousePointer,
  Play,
  Plus,
  RefreshCw,
  Search,
  Server,
  Settings,
  ShieldCheck,
  Sliders,
  Sun,
  Trash2,
  Volume2,
  X,
} from "lucide-react";
import {
  DEFAULT_PERMISSIONS,
  formatDeviceId,
  normalizeDeviceId,
  type ActiveHostSession,
  type AppConfig,
  type IncomingApproval,
  type SavedDevice,
  type SessionPermissions,
} from "./types";
import {
  disconnectHostSession,
  fetchDeviceOnlineStatus,
  getAgentStatus,
  getAutostart,
  getConfig,
  isTauriEnv,
  onAgentEvent,
  regeneratePin,
  removeDevice,
  respondIncomingRequest,
  saveDevice,
  setAutostart,
  setViewerMode,
  updateAccessSettings,
  updatePreferences,
  updateServerSettings,
} from "./lib/tauriBridge";
import { ApprovalModal } from "./components/ApprovalModal";
import { HostSessionWidget } from "./components/HostSessionWidget";
import { RemoteViewer } from "./components/RemoteViewer";
import { SettingsModal } from "./components/SettingsModal";

interface ActiveViewerSession {
  targetDeviceId: string;
  pin?: string;
}

const THEME_KEY = "remoteconnect_theme";

function getInitialTheme(): "dark" | "light" {
  try {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // ignore
  }
  return window.matchMedia?.("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

export default function App() {
  const isWidgetMode =
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("mode") === "widget";

  if (isWidgetMode) {
    return <HostSessionWidget />;
  }

  return <MainApp />;
}

function MainApp() {
  const [theme, setTheme] = useState<"dark" | "light">(getInitialTheme);
  const [sidebarOpen, setSidebarOpen] = useState(false);

  const [config, setConfig] = useState<AppConfig | null>(null);
  const [agentStatus, setAgentStatus] = useState<string>("connecting");
  const [autostartEnabled, setAutostartEnabled] = useState<boolean>(false);
  const [deviceOnlineMap, setDeviceOnlineMap] = useState<Record<string, boolean>>({});
  const [refreshingStatuses, setRefreshingStatuses] = useState(false);
  const [targetIdInput, setTargetIdInput] = useState("");
  const [targetPinInput, setTargetPinInput] = useState("");
  const [showTargetPin, setShowTargetPin] = useState(false);
  const [showLocalPin, setShowLocalPin] = useState(true);
  const [copiedField, setCopiedField] = useState<"id" | "pin" | null>(null);
  const [deviceQuery, setDeviceQuery] = useState("");

  const [activeViewer, setActiveViewer] = useState<ActiveViewerSession | null>(null);

  // Host-side incoming approval & active sessions
  const [pendingApproval, setPendingApproval] = useState<IncomingApproval | null>(null);
  const [hostSessions, setHostSessions] = useState<ActiveHostSession[]>([]);

  // Server & App Settings modals
  const [showServerModal, setShowServerModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [serverUrlDraft, setServerUrlDraft] = useState("");
  const [authTokenDraft, setAuthTokenDraft] = useState("");
  const [deviceNameDraft, setDeviceNameDraft] = useState("");

  // Access & Permissions modal
  const [showAccessModal, setShowAccessModal] = useState(false);
  const [pinDraft, setPinDraft] = useState("");
  const [pinEnabledDraft, setPinEnabledDraft] = useState(true);
  const [customPwDraft, setCustomPwDraft] = useState("");
  const [customPwEnabledDraft, setCustomPwEnabledDraft] = useState(false);
  const [permsDraft, setPermsDraft] = useState<SessionPermissions>({
    ...DEFAULT_PERMISSIONS,
  });

  // Add saved device modal
  const [showAddDevice, setShowAddDevice] = useState(false);
  const [newDevName, setNewDevName] = useState("");
  const [newDevId, setNewDevId] = useState("");
  const [newDevPin, setNewDevPin] = useState("");

  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    if (/Android/i.test(navigator.userAgent)) {
      document.documentElement.classList.add("is-android");
    }
    try {
      localStorage.setItem(THEME_KEY, theme);
    } catch {
      // ignore
    }
  }, [theme]);

  const toggleTheme = () => setTheme((t) => (t === "dark" ? "light" : "dark"));

  const notify = (msg: string) => {
    setToast(msg);
    window.setTimeout(() => setToast(null), 3500);
  };

  const copyWithFeedback = (text: string, field: "id" | "pin", label: string) => {
    navigator.clipboard.writeText(text).catch(() => {});
    setCopiedField(field);
    notify(label);
    window.setTimeout(() => setCopiedField(null), 1600);
  };

  const openAccessModal = (cfg: AppConfig) => {
    setPinDraft(cfg.pin);
    setPinEnabledDraft(cfg.pin_enabled);
    setCustomPwDraft(cfg.custom_password ?? "");
    setCustomPwEnabledDraft(cfg.custom_password_enabled);
    setPermsDraft({ ...cfg.default_permissions });
    setShowAccessModal(true);
  };

  useEffect(() => {
    void (async () => {
      const cfg = await getConfig();
      setConfig(cfg);
      setServerUrlDraft(cfg.server_url);
      setAuthTokenDraft(cfg.auth_token ?? "");
      setDeviceNameDraft(cfg.device_name);
      const st = await getAgentStatus();
      setAgentStatus(st);
      if (isTauriEnv()) {
        const auto = await getAutostart().catch(() => false);
        setAutostartEnabled(auto);
      }
    })();

    let unlisten: (() => void) | undefined;
    void onAgentEvent((ev) => {
      switch (ev.event) {
        case "connecting":
          setAgentStatus("connecting");
          break;
        case "registered":
          setAgentStatus("online");
          break;
        case "disconnected":
          setAgentStatus("offline");
          break;
        case "incoming_request":
          setPendingApproval({
            session_id: ev.session_id,
            viewer_name: ev.viewer_name,
          });
          break;
        case "session_started":
          setHostSessions((prev) => [
            ...prev.filter((s) => s.session_id !== ev.session_id),
            { session_id: ev.session_id, viewer_name: ev.viewer_name },
          ]);
          notify(`Sesión remota iniciada por ${ev.viewer_name}`);
          break;
        case "session_ended":
          setHostSessions((prev) => {
            const ended = prev.find((s) => s.session_id === ev.session_id);
            if (ended) {
              notify(`Desconectado: ${ended.viewer_name} cerró la sesión`);
            }
            return prev.filter((s) => s.session_id !== ev.session_id);
          });
          break;
        case "error":
          notify(`Error del servidor: ${ev.message}`);
          break;
      }
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    void setViewerMode(Boolean(activeViewer));
  }, [activeViewer]);

  const refreshDeviceStatuses = async (
    serverUrl: string,
    devices: SavedDevice[]
  ) => {
    if (devices.length === 0) return;
    setRefreshingStatuses(true);
    try {
      const results = await Promise.all(
        devices.map(async (dev) => {
          const info = await fetchDeviceOnlineStatus(serverUrl, dev.id);
          return [dev.id, info?.online ?? false] as const;
        })
      );
      setDeviceOnlineMap((prev) => {
        const next = { ...prev };
        for (const [id, online] of results) {
          next[id] = online;
        }
        return next;
      });
    } finally {
      setRefreshingStatuses(false);
    }
  };

  useEffect(() => {
    if (!config || config.saved_devices.length === 0 || activeViewer) return;
    void refreshDeviceStatuses(config.server_url, config.saved_devices);
    const id = window.setInterval(() => {
      void refreshDeviceStatuses(config.server_url, config.saved_devices);
    }, 8000);
    return () => window.clearInterval(id);
  }, [config?.server_url, config?.saved_devices, activeViewer]);

  const onlineDevicesCount = useMemo(() => {
    if (!config) return 0;
    return config.saved_devices.filter((d) => deviceOnlineMap[d.id]).length;
  }, [config, deviceOnlineMap]);

  const filteredDevices = useMemo(() => {
    if (!config) return [];
    const q = deviceQuery.trim().toLowerCase();
    if (!q) return config.saved_devices;
    return config.saved_devices.filter(
      (d) =>
        d.name.toLowerCase().includes(q) ||
        d.id.toLowerCase().includes(q) ||
        formatDeviceId(d.id).includes(q)
    );
  }, [config, deviceQuery]);

  if (!config) {
    return (
      <div
        style={{
          height: "100%",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: "var(--text-secondary)",
          fontSize: 14,
        }}
      >
        Cargando RemoteConnect...
      </div>
    );
  }

  if (activeViewer) {
    return (
      <>
        <RemoteViewer
          serverUrl={config.server_url}
          targetDeviceId={activeViewer.targetDeviceId}
          viewerName={config.device_name}
          pin={activeViewer.pin}
          authToken={config.auth_token}
          preferences={config.preferences}
          theme={theme}
          onToggleTheme={toggleTheme}
          onConnected={async ({ deviceId, hostName, pin }) => {
            const existing = config.saved_devices.find((d) => d.id === deviceId);
            const updatedList = await saveDevice({
              id: deviceId,
              name: existing?.name || hostName || deviceId,
              pin: pin?.trim() ? pin.trim() : existing?.pin ?? null,
              notes: existing?.notes ?? null,
            });
            setConfig((prev) =>
              prev ? { ...prev, saved_devices: updatedList } : prev
            );
          }}
          onExit={(msg) => {
            setActiveViewer(null);
            if (msg) notify(msg);
          }}
        />
        {pendingApproval && (
          <ApprovalModal
            request={pendingApproval}
            defaultPermissions={config.default_permissions}
            onDecision={async (sid, ok, perms) => {
              setPendingApproval(null);
              await respondIncomingRequest(sid, ok, perms);
            }}
          />
        )}
      </>
    );
  }

  const startConnection = (rawId: string, pin?: string) => {
    const clean = normalizeDeviceId(rawId);
    if (!clean) {
      notify("Ingresa un ID de dispositivo válido");
      return;
    }
    setActiveViewer({ targetDeviceId: clean, pin });
  };

  const handleSaveServer = async () => {
    const updated = await updateServerSettings(
      serverUrlDraft.trim(),
      authTokenDraft.trim() || null,
      deviceNameDraft.trim() || "Mi-Equipo"
    );
    setConfig(updated);
    setShowServerModal(false);
    notify("Configuración de servidor guardada");
  };

  const handleSaveAccess = async () => {
    const updated = await updateAccessSettings({
      pin: pinDraft.trim() || config.pin,
      pinEnabled: pinEnabledDraft,
      customPassword: customPwDraft.trim() || null,
      customPasswordEnabled: customPwEnabledDraft && Boolean(customPwDraft.trim()),
      defaultPermissions: permsDraft,
    });
    setConfig(updated);
    setShowAccessModal(false);
    notify("Seguridad, contraseña y permisos guardados");
  };

  const handleAddDevice = async () => {
    const cleanId = normalizeDeviceId(newDevId);
    if (!cleanId || !newDevName.trim()) {
      notify("Completa el nombre y el ID del dispositivo");
      return;
    }
    const dev: SavedDevice = {
      id: cleanId,
      name: newDevName.trim(),
      pin: newDevPin.trim() || null,
    };
    const list = await saveDevice(dev);
    setConfig({ ...config, saved_devices: list });
    setNewDevName("");
    setNewDevId("");
    setNewDevPin("");
    setShowAddDevice(false);
    notify("Dispositivo guardado en tu libreta");
  };

  const handleDeleteDevice = async (id: string) => {
    const list = await removeDevice(id);
    setConfig({ ...config, saved_devices: list });
  };

  return (
    <div className="app-shell">
      <div
        className={`sidebar-backdrop ${sidebarOpen ? "open" : ""}`}
        onClick={() => setSidebarOpen(false)}
      />

      {/* Left Sidebar: Local Host Identity, Security & Server */}
      <aside className={`sidebar ${sidebarOpen ? "open" : ""}`}>
        <div className="brand">
          <div className="brand-left">
            <div className="brand-icon">
              <ShieldCheck size={20} />
            </div>
            <div>
              <h1>RemoteConnect</h1>
              <div className="brand-sub">Rust E2E Desktop</div>
            </div>
          </div>
          <div className="brand-actions">
            <button
              className="icon-btn"
              onClick={toggleTheme}
              title={theme === "dark" ? "Tema claro" : "Tema oscuro"}
            >
              {theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}
            </button>
            <button
              className="icon-btn mobile-menu-btn"
              onClick={() => setSidebarOpen(false)}
              title="Cerrar menú"
            >
              <X size={18} />
            </button>
          </div>
        </div>

        <div className="card">
          <div className="card-title">
            <span>Este equipo</span>
            <span className={`status-pill ${agentStatus}`}>
              <span className="status-dot" />
              {agentStatus === "online"
                ? "En línea"
                : agentStatus === "connecting"
                ? "Conectando"
                : agentStatus === "web-only"
                ? "Visor Web"
                : agentStatus === "mobile-viewer"
                ? "Visor Android"
                : "Sin conexión"}
            </span>
          </div>

          {isTauriEnv() ? (
            <>
              <div className="id-box">
                <div>
                  <div className="id-label">Tu ID de acceso</div>
                  <div className="id-value">{formatDeviceId(config.device_id)}</div>
                </div>
                <button
                  className="icon-btn"
                  title="Copiar ID"
                  onClick={() =>
                    copyWithFeedback(config.device_id, "id", "ID copiado al portapapeles")
                  }
                >
                  {copiedField === "id" ? (
                    <Check size={16} color="var(--success)" />
                  ) : (
                    <Copy size={16} />
                  )}
                </button>
              </div>

              <div className="pin-row">
                <div>
                  <div style={{ fontSize: 11, color: "var(--text-muted)" }}>
                    PIN temporal {config.pin_enabled ? "• activo" : "• pausado"}
                  </div>
                  <div
                    className="pin-value"
                    style={{ opacity: config.pin_enabled ? 1 : 0.4 }}
                  >
                    {showLocalPin ? config.pin : "••••••"}
                  </div>
                </div>
                <div style={{ display: "flex", gap: 2 }}>
                  <button
                    className="icon-btn"
                    title={showLocalPin ? "Ocultar PIN" : "Mostrar PIN"}
                    onClick={() => setShowLocalPin((v) => !v)}
                  >
                    {showLocalPin ? <EyeOff size={15} /> : <Eye size={15} />}
                  </button>
                  <button
                    className="icon-btn"
                    title="Copiar PIN"
                    onClick={() => copyWithFeedback(config.pin, "pin", "PIN copiado")}
                  >
                    {copiedField === "pin" ? (
                      <Check size={15} color="var(--success)" />
                    ) : (
                      <Copy size={15} />
                    )}
                  </button>
                  <button
                    className="icon-btn"
                    title="Generar nuevo PIN"
                    onClick={async () => {
                      const updated = await regeneratePin();
                      setConfig(updated);
                      notify("Nuevo PIN generado");
                    }}
                  >
                    <RefreshCw size={15} />
                  </button>
                </div>
              </div>

              <div
                style={{
                  fontSize: 12,
                  color: "var(--text-secondary)",
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                  marginBottom: 10,
                }}
              >
                <Lock
                  size={13}
                  color={
                    config.custom_password_enabled
                      ? "var(--success)"
                      : "var(--text-muted)"
                  }
                />
                {config.custom_password_enabled
                  ? "Contraseña fija activa"
                  : "Sin contraseña fija"}
              </div>

              <div className="perm-chips">
                <span
                  className={`perm-chip ${
                    config.default_permissions.allow_input ? "active" : ""
                  }`}
                >
                  <MousePointer size={11} /> Control
                </span>
                <span
                  className={`perm-chip ${
                    config.default_permissions.allow_audio ? "active" : ""
                  }`}
                >
                  <Volume2 size={11} /> Audio
                </span>
                <span
                  className={`perm-chip ${
                    config.default_permissions.allow_clipboard ? "active" : ""
                  }`}
                >
                  <Clipboard size={11} /> Ctrl+C/V
                </span>
                <span
                  className={`perm-chip ${
                    config.default_permissions.allow_file_transfer ? "active" : ""
                  }`}
                >
                  <FolderOpen size={11} /> Archivos
                </span>
              </div>

              <button
                className="btn btn-secondary btn-sm"
                style={{ width: "100%" }}
                onClick={() => openAccessModal(config)}
              >
                <Sliders size={14} />
                Contraseña, PIN y Permisos
              </button>

              <label
                className="toggle-row"
                style={{
                  marginTop: 12,
                  paddingTop: 12,
                  borderTop: "1px solid var(--border)",
                  fontSize: 12.5,
                }}
              >
                <div>
                  <strong style={{ fontWeight: 600 }}>Iniciar con Windows</strong>
                  <div className="toggle-desc">En segundo plano (System Tray)</div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={autostartEnabled}
                    onChange={async (e) => {
                      const next = e.target.checked;
                      try {
                        const res = await setAutostart(next);
                        setAutostartEnabled(res);
                        notify(
                          res
                            ? "RemoteConnect iniciará automáticamente con Windows"
                            : "Inicio automático con Windows desactivado"
                        );
                      } catch {
                        notify("No se pudo actualizar el inicio automático");
                      }
                    }}
                  />
                  <span className="switch-slider" />
                </span>
              </label>
            </>
          ) : (
            <p style={{ fontSize: 13, color: "var(--text-secondary)", lineHeight: 1.55 }}>
              Ejecutando en navegador (modo visor). Para compartir este equipo, abre la
              app de escritorio o ejecuta <code>remoteconnect-agent</code>.
            </p>
          )}
        </div>

        {hostSessions.length > 0 && (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {hostSessions.map((s) => (
              <div key={s.session_id} className="active-host-banner">
                <span>En control: {s.viewer_name}</span>
                <button
                  className="btn btn-danger btn-sm"
                  onClick={() => disconnectHostSession(s.session_id)}
                >
                  Cortar
                </button>
              </div>
            ))}
          </div>
        )}

        <div className="card" style={{ marginTop: "auto" }}>
          <div className="card-title">
            <span>Servidor Propio (Rust)</span>
          </div>
          <div
            style={{
              fontFamily: "JetBrains Mono, monospace",
              fontSize: 11.5,
              color: "var(--text-secondary)",
              wordBreak: "break-all",
              marginBottom: 10,
            }}
          >
            {config.server_url}
          </div>
          <button
            className="btn btn-secondary btn-sm"
            style={{ width: "100%" }}
            onClick={() => setShowServerModal(true)}
          >
            <Server size={14} />
            Configurar servidor / token
          </button>
        </div>
      </aside>

      {/* Main Content Area */}
      <div className="main-wrapper">
        <header className="topbar">
          <div className="topbar-left">
            <button
              className="icon-btn mobile-menu-btn"
              onClick={() => setSidebarOpen(true)}
              title="Abrir panel de este equipo"
            >
              <Menu size={19} />
            </button>
            <span className="topbar-title">{config.device_name}</span>
          </div>

          <div className="topbar-right">
            <span className="e2e-pill">
              <Lock size={12} />
              Cifrado E2E AES-256-GCM
            </span>
            <button
              className="btn btn-secondary btn-sm"
              onClick={() => setShowSettingsModal(true)}
              title="Configuración general, grabaciones y calidad"
            >
              <Settings size={14} />
              Configuración
            </button>
            <button
              className="icon-btn"
              onClick={toggleTheme}
              title={theme === "dark" ? "Cambiar a tema claro" : "Cambiar a tema oscuro"}
            >
              {theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}
            </button>
          </div>
        </header>

        <main className="main-area">
          <section className="connect-hero">
            <h2>Controlar dispositivo remoto</h2>
            <p>
              Conéctate a cualquier equipo ingresando su ID de 9 dígitos. Video en tiempo
              real, audio remoto, transferencia de archivos con Ctrl+C/Ctrl+V y cifrado
              de extremo a extremo.
            </p>
            <form
              className="connect-form"
              onSubmit={(e) => {
                e.preventDefault();
                startConnection(targetIdInput, targetPinInput);
              }}
            >
              <div className="input-with-icon">
                <Laptop size={16} className="input-icon-left" />
                <input
                  placeholder="ID del dispositivo (ej. 482 910 374)"
                  value={targetIdInput}
                  onChange={(e) => setTargetIdInput(e.target.value)}
                />
              </div>

              <div className="input-with-icon">
                <KeyRound size={16} className="input-icon-left" />
                <input
                  type={showTargetPin ? "text" : "password"}
                  placeholder="Contraseña o PIN"
                  style={{ paddingRight: 38 }}
                  value={targetPinInput}
                  onChange={(e) => setTargetPinInput(e.target.value)}
                />
                <button
                  type="button"
                  className="icon-btn input-icon-right"
                  onClick={() => setShowTargetPin((v) => !v)}
                  title={showTargetPin ? "Ocultar" : "Mostrar"}
                >
                  {showTargetPin ? <EyeOff size={15} /> : <Eye size={15} />}
                </button>
              </div>

              <button type="submit" className="btn btn-primary">
                <Play size={15} />
                Conectar
              </button>
            </form>
          </section>

          <section>
            <div className="section-header">
              <h3>
                Dispositivos guardados
                <span className="count-badge">{config.saved_devices.length}</span>
                {config.saved_devices.length > 0 && (
                  <span className="status-pill online" style={{ fontSize: 11, padding: "2px 8px" }}>
                    <span className="status-dot" />
                    {onlineDevicesCount} en línea
                  </span>
                )}
              </h3>

              <div className="section-tools">
                {config.saved_devices.length > 0 && (
                  <>
                    <button
                      className="icon-btn"
                      onClick={() =>
                        void refreshDeviceStatuses(config.server_url, config.saved_devices)
                      }
                      title="Actualizar estado en línea"
                      disabled={refreshingStatuses}
                    >
                      <RefreshCw size={15} />
                    </button>
                    <div className="input-with-icon">
                      <Search size={14} className="input-icon-left" />
                      <input
                        type="search"
                        className="search-input"
                        placeholder="Buscar equipo..."
                        value={deviceQuery}
                        onChange={(e) => setDeviceQuery(e.target.value)}
                      />
                    </div>
                  </>
                )}
                <button
                  className="btn btn-secondary btn-sm"
                  onClick={() => setShowAddDevice(true)}
                >
                  <Plus size={14} />
                  Guardar equipo
                </button>
              </div>
            </div>

            {config.saved_devices.length === 0 ? (
              <div className="empty-state">
                <div className="empty-state-icon">
                  <Laptop size={24} />
                </div>
                <strong style={{ color: "var(--text-primary)", fontSize: 15 }}>
                  Tu libreta de equipos está vacía
                </strong>
                <p style={{ fontSize: 13, maxWidth: 380 }}>
                  Guarda tus PCs de oficina, casa o servidores con su contraseña o PIN
                  para conectarte con un solo clic.
                </p>
                <button
                  className="btn btn-primary btn-sm"
                  style={{ marginTop: 4 }}
                  onClick={() => setShowAddDevice(true)}
                >
                  <Plus size={14} />
                  Agregar primer dispositivo
                </button>
              </div>
            ) : filteredDevices.length === 0 ? (
              <div className="empty-state">
                <p style={{ fontSize: 13 }}>
                  No se encontraron dispositivos que coincidan con "{deviceQuery}".
                </p>
              </div>
            ) : (
              <div className="devices-grid">
                {filteredDevices.map((dev) => {
                  const statusKnown = dev.id in deviceOnlineMap;
                  const isOnline = Boolean(deviceOnlineMap[dev.id]);
                  return (
                    <div key={dev.id} className="device-tile">
                      <div className="device-tile-head">
                        <div className="device-avatar" style={{ position: "relative" }}>
                          {dev.name.slice(0, 2).toUpperCase()}
                          <span
                            className={`device-presence-dot ${
                              statusKnown ? (isOnline ? "online" : "offline") : "checking"
                            }`}
                          />
                        </div>
                        <div style={{ minWidth: 0, flex: 1 }}>
                          <div className="device-tile-name">{dev.name}</div>
                          <div className="device-tile-id">{formatDeviceId(dev.id)}</div>
                        </div>
                        <span
                          className={`status-pill ${
                            statusKnown ? (isOnline ? "online" : "offline") : "connecting"
                          }`}
                          style={{ fontSize: 10.5, padding: "3px 8px", flexShrink: 0 }}
                        >
                          <span className="status-dot" />
                          {statusKnown ? (isOnline ? "En línea" : "Sin conexión") : "..."}
                        </span>
                      </div>

                      <div className="device-tile-meta">
                        <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
                          <KeyRound size={12} />
                          {dev.pin ? "Acceso rápido guardado" : "Requiere aprobación"}
                        </span>
                      </div>

                      <div className="device-tile-actions">
                        <button
                          className="btn btn-primary btn-sm"
                          style={{ flex: 1 }}
                          onClick={() => startConnection(dev.id, dev.pin ?? undefined)}
                        >
                          <Play size={13} />
                          Conectar
                        </button>
                        <button
                          className="btn btn-secondary btn-sm"
                          onClick={() => handleDeleteDevice(dev.id)}
                          title="Eliminar de la libreta"
                        >
                          <Trash2 size={13} />
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </main>
      </div>

      {/* Full App Settings Modal */}
      {showSettingsModal && (
        <SettingsModal
          config={config}
          theme={theme}
          autostartEnabled={autostartEnabled}
          onToggleTheme={toggleTheme}
          onToggleAutostart={async (next) => {
            try {
              const res = await setAutostart(next);
              setAutostartEnabled(res);
              notify(
                res
                  ? "RemoteConnect iniciará automáticamente con Windows"
                  : "Inicio automático con Windows desactivado"
              );
            } catch {
              notify("No se pudo actualizar el inicio automático");
            }
          }}
          onSavePreferences={async (prefs) => {
            const updated = await updatePreferences(prefs);
            setConfig(updated);
            notify("Configuración guardada");
          }}
          onSaveServer={async (url, token, name) => {
            const updated = await updateServerSettings(url, token, name);
            setConfig(updated);
            setServerUrlDraft(updated.server_url);
            setAuthTokenDraft(updated.auth_token ?? "");
            setDeviceNameDraft(updated.device_name);
            notify("Configuración de servidor actualizada");
          }}
          onClose={() => setShowSettingsModal(false)}
        />
      )}

      {/* Access Security, Custom Password, PIN & Default Permissions Modal */}
      {showAccessModal && (
        <div className="modal-backdrop" onClick={() => setShowAccessModal(false)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                <Lock size={18} color="var(--accent)" />
                Acceso, Contraseña y Permisos
              </h3>
              <button className="icon-btn" onClick={() => setShowAccessModal(false)}>
                <X size={18} />
              </button>
            </div>

            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <label className="toggle-row">
                <div>
                  <strong>Acceso con PIN temporal</strong>
                  <div className="toggle-desc">Código corto ideal para soporte rápido</div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={pinEnabledDraft}
                    onChange={(e) => setPinEnabledDraft(e.target.checked)}
                  />
                  <span className="switch-slider" />
                </span>
              </label>
              {pinEnabledDraft && (
                <div className="field">
                  <label>PIN (6 dígitos o personalizado)</label>
                  <input
                    value={pinDraft}
                    onChange={(e) => setPinDraft(e.target.value)}
                    placeholder="123456"
                  />
                </div>
              )}
            </div>

            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <label className="toggle-row">
                <div>
                  <strong>Acceso directo con contraseña fija</strong>
                  <div className="toggle-desc">Conéctate sin aprobación manual</div>
                </div>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={customPwEnabledDraft}
                    onChange={(e) => setCustomPwEnabledDraft(e.target.checked)}
                  />
                  <span className="switch-slider" />
                </span>
              </label>
              {customPwEnabledDraft && (
                <div className="field">
                  <label>Tu contraseña permanente</label>
                  <input
                    type="password"
                    value={customPwDraft}
                    onChange={(e) => setCustomPwDraft(e.target.value)}
                    placeholder="Elige una contraseña segura"
                  />
                </div>
              )}
            </div>

            <div className="card" style={{ display: "flex", flexDirection: "column", gap: 12 }}>
              <div className="card-title" style={{ marginBottom: 2 }}>
                Permisos por defecto al conectarse
              </div>
              <label className="toggle-row">
                <span>Control de mouse y teclado</span>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={permsDraft.allow_input}
                    onChange={(e) =>
                      setPermsDraft({ ...permsDraft, allow_input: e.target.checked })
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>
              <label className="toggle-row">
                <span>Escuchar audio remoto</span>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={permsDraft.allow_audio}
                    onChange={(e) =>
                      setPermsDraft({ ...permsDraft, allow_audio: e.target.checked })
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>
              <label className="toggle-row">
                <span>Portapapeles compartido (Ctrl+C / Ctrl+V)</span>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={permsDraft.allow_clipboard}
                    onChange={(e) =>
                      setPermsDraft({ ...permsDraft, allow_clipboard: e.target.checked })
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>
              <label className="toggle-row">
                <span>Transferencia de archivos</span>
                <span className="switch">
                  <input
                    type="checkbox"
                    checked={permsDraft.allow_file_transfer}
                    onChange={(e) =>
                      setPermsDraft({
                        ...permsDraft,
                        allow_file_transfer: e.target.checked,
                      })
                    }
                  />
                  <span className="switch-slider" />
                </span>
              </label>
            </div>

            <div style={{ display: "flex", justifyContent: "flex-end", gap: 10 }}>
              <button
                className="btn btn-secondary"
                onClick={() => setShowAccessModal(false)}
              >
                Cancelar
              </button>
              <button className="btn btn-primary" onClick={handleSaveAccess}>
                Guardar cambios
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Server Settings Modal */}
      {showServerModal && (
        <div className="modal-backdrop" onClick={() => setShowServerModal(false)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>
                <Server size={18} color="var(--accent)" />
                Configuración del Servidor Rust
              </h3>
              <button className="icon-btn" onClick={() => setShowServerModal(false)}>
                <X size={18} />
              </button>
            </div>
            <div className="field">
              <label>Nombre de este equipo</label>
              <input
                value={deviceNameDraft}
                onChange={(e) => setDeviceNameDraft(e.target.value)}
                placeholder="Ej. Laptop-David"
              />
            </div>
            <div className="field">
              <label>URL WebSocket del servidor (ws:// o wss://)</label>
              <input
                value={serverUrlDraft}
                onChange={(e) => setServerUrlDraft(e.target.value)}
                placeholder="ws://127.0.0.1:8080/ws"
              />
            </div>
            <div className="field">
              <label>Token de autenticación del servidor (opcional)</label>
              <input
                type="password"
                value={authTokenDraft}
                onChange={(e) => setAuthTokenDraft(e.target.value)}
                placeholder="Dejar vacío si el servidor está en modo abierto"
              />
            </div>
            <div style={{ display: "flex", justifyContent: "flex-end", gap: 10 }}>
              <button
                className="btn btn-secondary"
                onClick={() => setShowServerModal(false)}
              >
                Cancelar
              </button>
              <button className="btn btn-primary" onClick={handleSaveServer}>
                Guardar y reconectar
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Add Saved Device Modal */}
      {showAddDevice && (
        <div className="modal-backdrop" onClick={() => setShowAddDevice(false)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>Guardar dispositivo</h3>
              <button className="icon-btn" onClick={() => setShowAddDevice(false)}>
                <X size={18} />
              </button>
            </div>
            <div className="field">
              <label>Nombre</label>
              <input
                value={newDevName}
                onChange={(e) => setNewDevName(e.target.value)}
                placeholder="Ej. PC Oficina"
              />
            </div>
            <div className="field">
              <label>ID de dispositivo (9 dígitos)</label>
              <input
                value={newDevId}
                onChange={(e) => setNewDevId(e.target.value)}
                placeholder="123 456 789"
              />
            </div>
            <div className="field">
              <label>Contraseña o PIN de acceso (opcional)</label>
              <input
                type="password"
                value={newDevPin}
                onChange={(e) => setNewDevPin(e.target.value)}
                placeholder="Contraseña fija o PIN"
              />
            </div>
            <div style={{ display: "flex", justifyContent: "flex-end", gap: 10 }}>
              <button
                className="btn btn-secondary"
                onClick={() => setShowAddDevice(false)}
              >
                Cancelar
              </button>
              <button className="btn btn-primary" onClick={handleAddDevice}>
                Guardar
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Incoming Connection Approval Modal */}
      {pendingApproval && (
        <ApprovalModal
          request={pendingApproval}
          defaultPermissions={config.default_permissions}
          onDecision={async (sid, ok, perms) => {
            setPendingApproval(null);
            await respondIncomingRequest(sid, ok, perms);
          }}
        />
      )}

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
