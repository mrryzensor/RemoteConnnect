# RemoteConnect

Aplicación de escritorio remoto estilo **AnyDesk / RustDesk** con **servidor propio en Rust**, cifrado de extremo a extremo (**ECDH P-256 + HKDF-SHA256 + AES-256-GCM**), agente nativo multiplataforma y aplicación de escritorio con **Tauri v2 + React**.

## Arquitectura

- **`crates/protocol`** (`remoteconnect-protocol`): Mensajes de señalización, protocolo de sesión y cifrado E2E (ECDH P-256 + AES-256-GCM) interoperable con WebCrypto.
- **`crates/server`** (`remoteconnect-server`): Servidor propio de rendez-vous y relay ciego en Rust (`axum` + `tokio` + WebSockets). Nunca tiene acceso a las claves de sesión ni a los frames descifrados.
- **`crates/agent`** (`remoteconnect-agent`): Agente nativo en Rust (captura de pantalla con `xcap`, control de mouse/teclado con `enigo`, portapapeles con `arboard` y transferencia de archivos por bloques). Se puede ejecutar standalone o embebido dentro de la app Tauri.
- **`app/`**: App de escritorio con **Tauri v2 + React + TypeScript** que integra tanto el **Host** (ID único de 9 dígitos, PIN, aprobación interactiva) como el **Visor** (canvas en tiempo real, selector de monitor, calidad/FPS, portapapeles y gestor de archivos).

## Inicio Rápido

### 1. Iniciar el servidor propio en Rust

```powershell
cargo run -p remoteconnect-server -- --bind 0.0.0.0:8080
```

Con token de autenticación privado (recomendado para producción):

```powershell
cargo run -p remoteconnect-server -- --bind 0.0.0.0:8080 --auth-token "mi-secreto-seguro"
```

### 2. Ejecutar la app de escritorio (Tauri v2)

```powershell
cd app
npm install
npm run tauri dev
```

Al abrirse, la app genera automáticamente un **ID de 9 dígitos** y un **PIN** persistentes, y se conecta a tu servidor Rust. Puedes:
- Compartir tu ID + PIN (o aprobar manualmente solicitudes entrantes).
- Conectarte a otro equipo ingresando su ID (y PIN opcional).
- Guardar dispositivos frecuentes en tu libreta local.

### 3. (Opcional) Ejecutar el Agente Standalone (sin interfaz gráfica)

Ideal para servidores o PCs sin pantalla abierta:

```powershell
cargo run -p remoteconnect-agent -- --server ws://127.0.0.1:8080/ws --pin 123456
```

### 4. (Opcional) Servir también el Visor Web desde el propio servidor Rust

```powershell
cd app && npm run build
cd ..
cargo run -p remoteconnect-server -- --bind 0.0.0.0:8080 --web-dir app/dist
```

Luego abre `http://127.0.0.1:8080` desde cualquier navegador (PC o móvil).
