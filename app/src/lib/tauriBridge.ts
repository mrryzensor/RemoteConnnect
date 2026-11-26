import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  DEFAULT_PERMISSIONS,
  type AgentEvent,
  type AppConfig,
  type SavedDevice,
  type SessionPermissions,
} from "../types";

export function isTauriEnv(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

const STORAGE_KEY = "remoteconnect_web_config_v2";

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
    server_url: "ws://127.0.0.1:8080/ws",
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
