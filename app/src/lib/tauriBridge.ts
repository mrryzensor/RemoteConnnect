import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  DEFAULT_PERMISSIONS,
  DEFAULT_PREFERENCES,
  type ActiveHostSession,
  type AgentEvent,
  type AppConfig,
  type SavedDevice,
  type SessionPermissions,
  type UserPreferences,
} from "../types";

export function isTauriEnv(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

const STORAGE_KEY = "remoteconnect_web_config_v2";

function defaultWebServerUrl(): string {
  if (typeof window !== "undefined" && window.location.hostname !== "localhost" && window.location.hostname !== "127.0.0.1") {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${window.location.host}/ws`;
  }
  return "wss://remote.2mail.us/ws";
}

function loadWebConfig(): AppConfig {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as AppConfig;
  } catch {
    // ignore
  }
  const fallback: AppConfig = {
    device_id: "000000000",
    device_name: "Web-Viewer",
    pin: "------",
    pin_enabled: false,
    custom_password: null,
    custom_password_enabled: false,
    default_permissions: { ...DEFAULT_PERMISSIONS },
    server_url: defaultWebServerUrl(),
    auth_token: null,
    saved_devices: [],
  };
  localStorage.setItem(STORAGE_KEY, JSON.stringify(fallback));
  return fallback;
}

function saveWebConfig(cfg: AppConfig) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(cfg));
}

export async function getConfig(): Promise<AppConfig> {
  if (isTauriEnv()) {
    return invoke<AppConfig>("get_config");
  }
  return loadWebConfig();
}

export async function getAgentStatus(): Promise<string> {
  if (isTauriEnv()) {
    return invoke<string>("get_agent_status");
  }
  return "web-only";
}

export async function updateServerSettings(
  serverUrl: string,
  authToken: string | null,
  deviceName: string
): Promise<AppConfig> {
  if (isTauriEnv()) {
    return invoke<AppConfig>("update_server_settings", {
      serverUrl,
      authToken,
      deviceName,
    });
  }
  const cfg = loadWebConfig();
  cfg.server_url = serverUrl;
  cfg.auth_token = authToken?.trim() ? authToken.trim() : null;
  cfg.device_name = deviceName;
  saveWebConfig(cfg);
  return cfg;
}

export async function updateAccessSettings(params: {
  pin: string;
  pinEnabled: boolean;
  customPassword: string | null;
  customPasswordEnabled: boolean;
  defaultPermissions: SessionPermissions;
}): Promise<AppConfig> {
  if (isTauriEnv()) {
    return invoke<AppConfig>("update_access_settings", {
      pin: params.pin,
      pinEnabled: params.pinEnabled,
      customPassword: params.customPassword,
      customPasswordEnabled: params.customPasswordEnabled,
      defaultPermissions: params.defaultPermissions,
    });
  }
  const cfg = loadWebConfig();
  cfg.pin = params.pin;
  cfg.pin_enabled = params.pinEnabled;
  cfg.custom_password = params.customPassword;
  cfg.custom_password_enabled = params.customPasswordEnabled;
  cfg.default_permissions = params.defaultPermissions;
  saveWebConfig(cfg);
  return cfg;
}

export async function regeneratePin(): Promise<AppConfig> {
  if (isTauriEnv()) {
    return invoke<AppConfig>("regenerate_pin");
  }
  return loadWebConfig();
}

export async function respondIncomingRequest(
  sessionId: string,
  accepted: boolean,
  permissions: SessionPermissions
): Promise<void> {
  if (isTauriEnv()) {
    await invoke("respond_incoming_request", { sessionId, accepted, permissions });
  }
}

export async function disconnectHostSession(sessionId: string): Promise<void> {
  if (isTauriEnv()) {
    await invoke("disconnect_host_session", { sessionId });
  }
}

export async function saveDevice(device: SavedDevice): Promise<SavedDevice[]> {
  if (isTauriEnv()) {
    return invoke<SavedDevice[]>("save_device", { device });
  }
  const cfg = loadWebConfig();
  const idx = cfg.saved_devices.findIndex((d) => d.id === device.id);
  if (idx >= 0) cfg.saved_devices[idx] = device;
  else cfg.saved_devices.push(device);
  saveWebConfig(cfg);
  return cfg.saved_devices;
}

export async function removeDevice(deviceId: string): Promise<SavedDevice[]> {
  if (isTauriEnv()) {
    return invoke<SavedDevice[]>("remove_device", { deviceId });
  }
  const cfg = loadWebConfig();
  cfg.saved_devices = cfg.saved_devices.filter((d) => d.id !== deviceId);
  saveWebConfig(cfg);
  return cfg.saved_devices;
}

export async function onAgentEvent(
  cb: (ev: AgentEvent) => void
): Promise<UnlistenFn> {
  if (isTauriEnv()) {
    return listen<AgentEvent>("agent-event", (event) => cb(event.payload));
  }
  return () => {};
}

export async function getAutostart(): Promise<boolean> {
  if (isTauriEnv()) {
    return invoke<boolean>("get_autostart");
  }
  return false;
}

export async function setAutostart(enabled: boolean): Promise<boolean> {
  if (isTauriEnv()) {
    return invoke<boolean>("set_autostart", { enabled });
  }
  return false;
}

export async function getActiveHostSessions(): Promise<ActiveHostSession[]> {
  if (isTauriEnv()) {
    return invoke<ActiveHostSession[]>("get_active_host_sessions");
  }
  return [];
}

export async function setViewerMode(active: boolean): Promise<void> {
  if (isTauriEnv()) {
    await invoke("set_viewer_mode", { active }).catch(() => {});
  }
}

export async function showMainWindow(): Promise<void> {
  if (isTauriEnv()) {
    await invoke("show_main_window").catch(() => {});
  }
}

export async function hideWidgetWindow(): Promise<void> {
  if (isTauriEnv()) {
    await invoke("hide_widget_window").catch(() => {});
  }
}

export async function updatePreferences(
  preferences: UserPreferences
): Promise<AppConfig> {
  if (isTauriEnv()) {
    return invoke<AppConfig>("update_preferences", { preferences });
  }
  const cfg = loadWebConfig();
  cfg.preferences = { ...DEFAULT_PREFERENCES, ...preferences };
  saveWebConfig(cfg);
  return cfg;
}

export async function saveRecordingFile(
  fileName: string,
  dataB64: string
): Promise<string> {
  if (isTauriEnv()) {
    return invoke<string>("save_recording_file", { fileName, dataB64 });
  }
  throw new Error("saveRecordingFile only available in desktop app");
}

export async function openFolder(path: string): Promise<void> {
  if (isTauriEnv()) {
    await invoke("open_folder", { path });
  }
}

export async function relaunchWithUiMode(
  uiMode: "auto" | "webview" | "native"
): Promise<void> {
  if (isTauriEnv()) {
    await invoke("relaunch_with_ui_mode", { uiMode });
  }
}

export interface LatestReleaseInfo {
  current_version: string;
  latest_tag: string;
  has_update: boolean;
  windows_url: string;
  android_url: string;
}

export async function checkLatestRelease(): Promise<LatestReleaseInfo | null> {
  if (isTauriEnv()) {
    try {
      return await invoke<LatestReleaseInfo>("check_latest_release");
    } catch {
      return null;
    }
  }
  return null;
}

export async function getAppVersion(): Promise<string> {
  if (isTauriEnv()) {
    return invoke<string>("get_app_version").catch(() => "0.1.18");
  }
  return "0.1.18";
}

export async function hasDesktopShortcut(): Promise<boolean> {
  if (isTauriEnv()) {
    return invoke<boolean>("has_desktop_shortcut").catch(() => true);
  }
  return true;
}

export async function createDesktopShortcut(): Promise<string> {
  if (isTauriEnv()) {
    return invoke<string>("create_desktop_shortcut");
  }
  throw new Error("Solo disponible en escritorio");
}

export async function installWindowsUpdate(downloadUrl: string): Promise<void> {
  if (isTauriEnv()) {
    await invoke("install_windows_update", { downloadUrl });
  }
}

export function serverWsToHttpBase(serverUrl: string): string | null {
  try {
    const u = new URL(serverUrl);
    u.protocol = u.protocol === "wss:" ? "https:" : "http:";
    u.pathname = u.pathname.replace(/\/ws\/?$/, "");
    u.search = "";
    u.hash = "";
    return u.toString().replace(/\/$/, "");
  } catch {
    return null;
  }
}

export interface DeviceOnlineInfo {
  online: boolean;
  device_name?: string | null;
  os?: string | null;
}

export async function fetchDeviceOnlineStatus(
  serverUrl: string,
  deviceId: string
): Promise<DeviceOnlineInfo | null> {
  const base = serverWsToHttpBase(serverUrl);
  if (!base) return null;
  try {
    const res = await fetch(`${base}/api/devices/${encodeURIComponent(deviceId)}/status`, {
      cache: "no-store",
    });
    if (!res.ok) return null;
    const data = (await res.json()) as DeviceOnlineInfo;
    return {
      online: Boolean(data.online),
      device_name: data.device_name ?? null,
      os: data.os ?? null,
    };
  } catch {
    return null;
  }
}
