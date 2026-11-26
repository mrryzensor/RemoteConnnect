export interface SessionPermissions {
  allow_input: boolean;
  allow_audio: boolean;
  allow_clipboard: boolean;
  allow_file_transfer: boolean;
}

export const DEFAULT_PERMISSIONS: SessionPermissions = {
  allow_input: true,
  allow_audio: true,
  allow_clipboard: true,
  allow_file_transfer: true,
};

export interface SavedDevice {
  id: string;
  name: string;
  pin?: string | null;
  notes?: string | null;
}

export interface AppConfig {
  device_id: string;
  device_name: string;
  pin: string;
  pin_enabled: boolean;
  custom_password?: string | null;
  custom_password_enabled: boolean;
  default_permissions: SessionPermissions;
  server_url: string;
  auth_token?: string | null;
  saved_devices: SavedDevice[];
}

export interface DisplayInfo {
  id: number;
  name: string;
  width: number;
  height: number;
  is_primary: boolean;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified?: string | null;
}

export interface ClipboardFileInfo {
  name: string;
  path: string;
  size: number;
}

export type AgentEvent =
  | { event: "connecting"; server_url: string }
  | { event: "registered"; device_id: string; server_version: string }
  | { event: "disconnected"; reason: string }
  | { event: "incoming_request"; session_id: string; viewer_name: string }
  | { event: "session_started"; session_id: string; viewer_name: string }
  | { event: "session_ended"; session_id: string; reason: string }
  | { event: "error"; message: string };

export interface IncomingApproval {
  session_id: string;
  viewer_name: string;
}

export interface ActiveHostSession {
  session_id: string;
  viewer_name: string;
}

export function formatDeviceId(id: string): string {
  const digits = id.replace(/\D/g, "");
  if (digits.length === 9) {
    return `${digits.slice(0, 3)} ${digits.slice(3, 6)} ${digits.slice(6, 9)}`;
  }
  return id;
}

export function normalizeDeviceId(id: string): string {
  return id.replace(/[^a-zA-Z0-9]/g, "");
}

export function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}
