import { useEffect, useState } from "react";
import { PhoneOff, X } from "lucide-react";
import type { ActiveHostSession } from "../types";
import {
  disconnectHostSession,
  getActiveHostSessions,
  hideWidgetWindow,
  onAgentEvent,
} from "../lib/tauriBridge";

export function HostSessionWidget() {
  const [sessions, setSessions] = useState<ActiveHostSession[]>([]);
  const [everConnected, setEverConnected] = useState(false);

  useEffect(() => {
    void getActiveHostSessions().then((list) => {
      setSessions(list);
      if (list.length > 0) setEverConnected(true);
    });

    let unlisten: (() => void) | undefined;
    void onAgentEvent((ev) => {
      if (ev.event === "session_started") {
        setEverConnected(true);
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

  const isConnected = sessions.length > 0;
  const current = sessions[0];
  const label = isConnected
    ? sessions.length > 1
      ? `${current.viewer_name} (+${sessions.length - 1})`
      : current.viewer_name
    : everConnected
    ? "Desconectado"
    : "Sin conexión";

  const handleDisconnectAll = () => {
    for (const s of sessions) {
      void disconnectHostSession(s.session_id);
    }
  };

  return (
    <div className="host-widget-bar" data-tauri-drag-region>
      <div className="host-widget-info" data-tauri-drag-region>
        <span className={`host-widget-dot ${isConnected ? "connected" : "disconnected"}`} />
        <span className="host-widget-text" data-tauri-drag-region title={label}>
          {label}
        </span>
      </div>
      <div className="host-widget-actions">
        {isConnected && (
          <button
            className="host-widget-stop"
            onClick={handleDisconnectAll}
            title="Finalizar conexión remota"
          >
            <PhoneOff size={12} />
            Cortar
          </button>
        )}
        <button
          className="host-widget-close"
          onClick={() => void hideWidgetWindow()}
          title="Ocultar widget (la conexión continúa activa)"
        >
          <X size={13} />
        </button>
      </div>
    </div>
  );
}
