import { useState } from "react";
import { ShieldAlert, Check, X } from "lucide-react";
import type { IncomingApproval, SessionPermissions } from "../types";

interface Props {
  request: IncomingApproval;
  defaultPermissions: SessionPermissions;
  onDecision: (
    sessionId: string,
    accepted: boolean,
    permissions: SessionPermissions
  ) => void;
}

export function ApprovalModal({ request, defaultPermissions, onDecision }: Props) {
  const [perms, setPerms] = useState<SessionPermissions>({ ...defaultPermissions });

  return (
    <div className="modal-backdrop">
      <div className="modal-card">
        <div className="modal-header">
          <h3>
            <ShieldAlert size={20} color="var(--warning)" />
            Solicitud de conexión entrante
          </h3>
        </div>
        <p style={{ fontSize: 13.5, color: "var(--text-secondary)", lineHeight: 1.55 }}>
          <strong style={{ color: "var(--text-primary)" }}>{request.viewer_name}</strong>{" "}
          solicita conectarse a este equipo mediante túnel cifrado E2E. Elige los
          permisos que deseas otorgar para esta sesión:
        </p>

        <div
          className="card"
          style={{ display: "flex", flexDirection: "column", gap: 14, padding: 16 }}
        >
          <label className="toggle-row">
            <div>
              <div>Control de mouse y teclado</div>
              <div className="toggle-desc">Permite mover el cursor y escribir</div>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={perms.allow_input}
                onChange={(e) => setPerms({ ...perms, allow_input: e.target.checked })}
              />
              <span className="switch-slider" />
            </span>
          </label>

          <label className="toggle-row">
            <div>
              <div>Escuchar audio del equipo</div>
              <div className="toggle-desc">Transmite el sonido del sistema en vivo</div>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={perms.allow_audio}
                onChange={(e) => setPerms({ ...perms, allow_audio: e.target.checked })}
              />
              <span className="switch-slider" />
            </span>
          </label>

          <label className="toggle-row">
            <div>
              <div>Portapapeles compartido (Ctrl+C / Ctrl+V)</div>
              <div className="toggle-desc">Sincroniza texto y archivos copiados</div>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={perms.allow_clipboard}
                onChange={(e) =>
                  setPerms({ ...perms, allow_clipboard: e.target.checked })
                }
              />
              <span className="switch-slider" />
            </span>
          </label>

          <label className="toggle-row">
            <div>
              <div>Transferencia de archivos</div>
              <div className="toggle-desc">Explorar, subir y descargar archivos</div>
            </div>
            <span className="switch">
              <input
                type="checkbox"
                checked={perms.allow_file_transfer}
                onChange={(e) =>
                  setPerms({ ...perms, allow_file_transfer: e.target.checked })
                }
              />
              <span className="switch-slider" />
            </span>
          </label>
        </div>

        <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
          <button
            className="btn btn-danger"
            onClick={() => onDecision(request.session_id, false, perms)}
          >
            <X size={16} />
            Rechazar
          </button>
          <button
            className="btn btn-primary"
            onClick={() => onDecision(request.session_id, true, perms)}
          >
            <Check size={16} />
            Aceptar con estos permisos
          </button>
        </div>
      </div>
    </div>
  );
}
