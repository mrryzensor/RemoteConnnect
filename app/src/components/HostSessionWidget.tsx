import { useEffect, useState } from "react";
import { PhoneOff } from "lucide-react";
import type { ActiveHostSession } from "../types";
import {
  disconnectHostSession,
  getActiveHostSessions,
  onAgentEvent,
} from "../lib/tauriBridge";

export function HostSessionWidget() {
  const [sessions, setSessions] = useState<ActiveHostSession[]>([]);

  useEffect(() => {
    void getActiveHostSessions().then(setSessions);

    let unlisten: (() => void) | undefined;
    void onAgentEvent((ev) => {
      if (ev.event === "session_started") {
        setSessions((prev) => [
          ...prev.filter((s) => s.session_id !== ev.session_id),
          { session_id: ev.session_id, viewer_name: ev.viewer_name },
        ]);
      } else if (ev.event === "session_ended") {
        setSessions((prev) => prev.filter((s) => s.session_id !== ev.session_id));
      }
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, []);

  const current = sessions[0];
  const label = current
    ? sessions.length > 1
      ? `${current.viewer_name} (+${sessions.length - 1})`
      : current.viewer_name
    : "Sesión activa";

  const handleDisconnectAll = () => {
    for (const s of sessions) {
      void disconnectHostSession(s.session_id);
    }
  };

  return (
    <div className="host-widget-bar" data-tauri-drag-region>
      <div className="host-widget-info" data-tauri-drag-region>
        <span className="host-widget-dot" />
        <span className="host-widget-text" data-tauri-drag-region title={label}>
          {label}
        </span>
      </div>
      <button
        className="host-widget-stop"
        onClick={handleDisconnectAll}
        title="Finalizar conexión remota"
      >
        <PhoneOff size={12} />
        Cortar
      </button>
    </div>
  );
}
