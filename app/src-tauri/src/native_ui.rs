//! Modern GPU-accelerated native UI (`eframe`/`egui`) — 0% WebView2 required.
//!
//! Mirrors the WebView design (dark & light themes, sidebar with ID/PIN/permissions/autostart,
//! saved devices grid, incoming approval dialog, and full Settings modal) so PCs without
//! Microsoft Edge WebView2 Runtime get a first-class modern desktop experience.

#[cfg(target_os = "windows")]
pub fn run_native_fallback(cfg: crate::AppConfig, start_in_background: bool) {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use eframe::egui::{self, Color32, CornerRadius, FontId, Frame, Margin, RichText, Stroke, Vec2};
    use remoteconnect_agent::{start_agent, AgentConfig, AgentEvent, AgentHandle};
    use remoteconnect_protocol::{generate_pin, normalize_device_id, SessionPermissions};
    use tokio::sync::mpsc;

    use crate::{autostart, default_recordings_dir, shortcut, AppConfig, SavedDevice, UserPreferences};

    #[derive(Clone)]
    struct ActiveSession {
        session_id: String,
        viewer_name: String,
    }

    #[derive(Clone)]
    struct PendingRequest {
        session_id: String,
        viewer_name: String,
        perms: SessionPermissions,
    }

    struct SharedAgentState {
        status: String,
        sessions: Vec<ActiveSession>,
        pending: Option<PendingRequest>,
        toast: Option<(String, Instant)>,
    }

    fn format_id(id: &str) -> String {
        let digits: String = id.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() == 9 {
            format!("{} {} {}", &digits[0..3], &digits[3..6], &digits[6..9])
        } else {
            id.to_string()
        }
    }

    fn server_ws_to_http(ws_url: &str) -> String {
        let s = ws_url
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1);
        s.strip_suffix("/ws").unwrap_or(&s).to_string()
    }

    fn open_url_or_path(target: &str) {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn();
    }

    #[derive(PartialEq, Eq, Clone, Copy)]
    enum SettingsTab {
        General,
        Media,
        Server,
    }

    struct NativeRemoteApp {
        cfg: AppConfig,
        dark_mode: bool,
        autostart_enabled: bool,
        show_pin: bool,
        target_id_input: String,
        target_pin_input: String,
        device_query: String,

        // Agent bridge
        agent: Option<AgentHandle>,
        shared: Arc<Mutex<SharedAgentState>>,
        rt: Arc<tokio::runtime::Runtime>,

        // Modals
        show_settings: bool,
        settings_tab: SettingsTab,
        prefs_draft: UserPreferences,
        server_url_draft: String,
        device_name_draft: String,
        auth_token_draft: String,

        show_access_modal: bool,
        pin_draft: String,
        pin_enabled_draft: bool,
        custom_pw_draft: String,
        custom_pw_enabled_draft: bool,
        perms_draft: SessionPermissions,

        show_add_device: bool,
        new_dev_name: String,
        new_dev_id: String,
        new_dev_pin: String,
    }

    impl NativeRemoteApp {
        fn new(cfg: AppConfig) -> Self {
            let rt = Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime"));
            let shared = Arc::new(Mutex::new(SharedAgentState {
                status: "connecting".into(),
                sessions: vec![],
                pending: None,
                toast: None,
            }));

            let mut app = Self {
                prefs_draft: cfg.preferences.clone(),
                server_url_draft: cfg.server_url.clone(),
                device_name_draft: cfg.device_name.clone(),
                auth_token_draft: cfg.auth_token.clone().unwrap_or_default(),
                pin_draft: cfg.pin.clone(),
                pin_enabled_draft: cfg.pin_enabled,
                custom_pw_draft: cfg.custom_password.clone().unwrap_or_default(),
                custom_pw_enabled_draft: cfg.custom_password_enabled,
                perms_draft: cfg.default_permissions,
                cfg,
                dark_mode: true,
                autostart_enabled: autostart::is_enabled(),
                show_pin: true,
                target_id_input: String::new(),
                target_pin_input: String::new(),
                device_query: String::new(),
                agent: None,
                shared,
                rt,
                show_settings: false,
                settings_tab: SettingsTab::General,
                show_access_modal: false,
                show_add_device: false,
                new_dev_name: String::new(),
                new_dev_id: String::new(),
                new_dev_pin: String::new(),
            };
            app.restart_agent();
            app
        }

        fn notify(&self, msg: impl Into<String>) {
            if let Ok(mut st) = self.shared.lock() {
                st.toast = Some((msg.into(), Instant::now() + Duration::from_secs(3)));
            }
        }

        fn restart_agent(&mut self) {
            if let Some(old) = self.agent.take() {
                old.stop();
            }
            let agent_cfg = AgentConfig {
                server_url: self.cfg.server_url.clone(),
                device_id: self.cfg.device_id.clone(),
                device_name: self.cfg.device_name.clone(),
                pin: self.cfg.pin.clone(),
                pin_enabled: self.cfg.pin_enabled,
                custom_password: self.cfg.custom_password.clone(),
                custom_password_enabled: self.cfg.custom_password_enabled,
                default_permissions: self.cfg.default_permissions,
                auto_accept: false,
                auth_token: self.cfg.auth_token.clone(),
            };

            let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<AgentEvent>();
            let handle = self.rt.block_on(async move { start_agent(agent_cfg, ev_tx) });
            self.agent = Some(handle);

            let shared = self.shared.clone();
            let default_perms = self.cfg.default_permissions;
            self.rt.spawn(async move {
                while let Some(ev) = ev_rx.recv().await {
                    let Ok(mut st) = shared.lock() else { continue };
                    match ev {
                        AgentEvent::Connecting { .. } => st.status = "connecting".into(),
                        AgentEvent::Registered { .. } => st.status = "online".into(),
                        AgentEvent::Disconnected { .. } => st.status = "offline".into(),
                        AgentEvent::IncomingRequest {
                            session_id,
                            viewer_name,
                        } => {
                            st.pending = Some(PendingRequest {
                                session_id,
                                viewer_name,
                                perms: default_perms,
                            });
                        }
                        AgentEvent::SessionStarted {
                            session_id,
                            viewer_name,
                        } => {
                            st.sessions.retain(|s| s.session_id != session_id);
                            st.sessions.push(ActiveSession {
                                session_id,
                                viewer_name: viewer_name.clone(),
                            });
                            st.toast = Some((
                                format!("Sesión iniciada por {}", viewer_name),
                                Instant::now() + Duration::from_secs(4),
                            ));
                        }
                        AgentEvent::SessionEnded { session_id, .. } => {
                            let ended_name = st
                                .sessions
                                .iter()
                                .find(|s| s.session_id == session_id)
                                .map(|s| s.viewer_name.clone());
                            st.sessions.retain(|s| s.session_id != session_id);
                            if let Some(name) = ended_name {
                                st.toast = Some((
                                    format!("Desconectado: {} finalizó la sesión", name),
                                    Instant::now() + Duration::from_secs(4),
                                ));
                            }
                        }
                        AgentEvent::Error { message } => {
                            st.toast = Some((
                                format!("Error: {}", message),
                                Instant::now() + Duration::from_secs(4),
                            ));
                        }
                    }
                }
            });
        }

        fn launch_web_viewer(&self, device_id: Option<&str>) {
            let base = server_ws_to_http(&self.cfg.server_url);
            let url = match device_id.map(normalize_device_id).filter(|s| !s.is_empty()) {
                Some(id) => format!("{}?connect={}", base, id),
                None => base,
            };
            open_url_or_path(&url);
            self.notify("Abriendo visor en el navegador predeterminado...");
        }

        fn switch_to_webview(&mut self, ctx: &egui::Context) {
            self.cfg.preferences.ui_mode = "webview".into();
            self.cfg.save();
            if let Some(a) = self.agent.take() {
                a.stop();
            }
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).arg("--webview").spawn();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        fn apply_theme(&self, ctx: &egui::Context) {
            let mut visuals = if self.dark_mode {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };

            if self.dark_mode {
                visuals.panel_fill = Color32::from_rgb(9, 13, 22); // --bg-app
                visuals.window_fill = Color32::from_rgb(14, 20, 34); // --bg-sidebar
                visuals.extreme_bg_color = Color32::from_rgb(11, 16, 29); // --bg-input
                visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(19, 27, 46);
                visuals.widgets.noninteractive.fg_stroke =
                    Stroke::new(1.0, Color32::from_rgb(248, 250, 252));
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(24, 34, 56);
                visuals.widgets.hovered.bg_fill = Color32::from_rgb(30, 44, 72);
                visuals.widgets.active.bg_fill = Color32::from_rgb(14, 165, 233);
                visuals.selection.bg_fill = Color32::from_rgb(2, 132, 199);
            } else {
                visuals.panel_fill = Color32::from_rgb(243, 246, 251);
                visuals.window_fill = Color32::from_rgb(255, 255, 255);
                visuals.extreme_bg_color = Color32::from_rgb(241, 245, 249);
                visuals.widgets.inactive.bg_fill = Color32::from_rgb(241, 245, 249);
                visuals.widgets.hovered.bg_fill = Color32::from_rgb(226, 232, 240);
                visuals.selection.bg_fill = Color32::from_rgb(2, 132, 199);
            }

            visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
            visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
            visuals.widgets.active.corner_radius = CornerRadius::same(8);
            visuals.window_corner_radius = CornerRadius::same(14);
            ctx.set_visuals(visuals);
        }

        fn card_frame(&self) -> Frame {
            let fill = if self.dark_mode {
                Color32::from_rgb(19, 27, 46)
            } else {
                Color32::WHITE
            };
            let border = if self.dark_mode {
                Color32::from_rgba_unmultiplied(148, 163, 184, 38)
            } else {
                Color32::from_rgba_unmultiplied(15, 23, 42, 26)
            };
            Frame::new()
                .fill(fill)
                .stroke(Stroke::new(1.0, border))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(14))
        }
    }

    impl eframe::App for NativeRemoteApp {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.apply_theme(ctx);
            ctx.request_repaint_after(Duration::from_millis(250));

            let accent = if self.dark_mode {
                Color32::from_rgb(56, 189, 248)
            } else {
                Color32::from_rgb(2, 132, 199)
            };
            let muted = Color32::from_rgb(148, 163, 184);
            let success = Color32::from_rgb(34, 197, 94);
            let danger = Color32::from_rgb(239, 68, 68);
            let warning = Color32::from_rgb(245, 158, 11);

            let (status, sessions, pending, toast) = {
                let mut st = self.shared.lock().unwrap();
                if let Some((_, until)) = &st.toast {
                    if Instant::now() > *until {
                        st.toast = None;
                    }
                }
                (
                    st.status.clone(),
                    st.sessions.clone(),
                    st.pending.clone(),
                    st.toast.as_ref().map(|(m, _)| m.clone()),
                )
            };

            // ============================================================
            // Left Sidebar
            // ============================================================
            let sidebar_bg = if self.dark_mode {
                Color32::from_rgb(14, 20, 34)
            } else {
                Color32::WHITE
            };
            egui::SidePanel::left("rc_sidebar")
                .exact_width(310.0)
                .resizable(false)
                .frame(Frame::new().fill(sidebar_bg).inner_margin(Margin::same(16)))
                .show(ctx, |ui| {
                    // Brand header
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("RemoteConnect")
                                    .size(18.0)
                                    .strong(),
                            );
                            ui.label(
                                RichText::new(format!(
                                    "Modo Nativo GPU • v{}",
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .size(11.0)
                                .color(muted),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let theme_lbl = if self.dark_mode { "☀ Claro" } else { "🌙 Oscuro" };
                            if ui.button(theme_lbl).clicked() {
                                self.dark_mode = !self.dark_mode;
                            }
                        });
                    });

                    ui.add_space(14.0);

                    // Local Host Identity Card
                    self.card_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("ESTE EQUIPO").size(11.0).strong().color(muted));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let (lbl, col) = match status.as_str() {
                                    "online" => ("● En línea", success),
                                    "connecting" => ("● Conectando", warning),
                                    _ => ("● Sin conexión", danger),
                                };
                                ui.label(RichText::new(lbl).size(11.5).strong().color(col));
                            });
                        });

                        ui.add_space(10.0);
                        ui.label(RichText::new("Tu ID de acceso").size(11.0).color(muted));
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format_id(&self.cfg.device_id))
                                    .font(FontId::monospace(21.0))
                                    .strong()
                                    .color(accent),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Copiar").clicked() {
                                    ctx.copy_text(self.cfg.device_id.clone());
                                    self.notify("ID copiado al portapapeles");
                                }
                            });
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                let pin_state = if self.cfg.pin_enabled {
                                    "PIN temporal • activo"
                                } else {
                                    "PIN temporal • pausado"
                                };
                                ui.label(RichText::new(pin_state).size(11.0).color(muted));
                                let shown = if self.show_pin {
                                    self.cfg.pin.as_str()
                                } else {
                                    "••••••"
                                };
                                ui.label(RichText::new(shown).font(FontId::monospace(15.0)).strong());
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("↻").on_hover_text("Generar nuevo PIN").clicked() {
                                    self.cfg.pin = generate_pin();
                                    self.cfg.save();
                                    if let Some(a) = &self.agent {
                                        a.update_access(
                                            self.cfg.pin.clone(),
                                            self.cfg.pin_enabled,
                                            self.cfg.custom_password.clone(),
                                            self.cfg.custom_password_enabled,
                                            self.cfg.default_permissions,
                                        );
                                    }
                                    self.notify("Nuevo PIN generado");
                                }
                                if ui.button("Copiar").clicked() {
                                    ctx.copy_text(self.cfg.pin.clone());
                                    self.notify("PIN copiado");
                                }
                                if ui.button(if self.show_pin { "Ocultar" } else { "Ver" }).clicked() {
                                    self.show_pin = !self.show_pin;
                                }
                            });
                        });

                        ui.add_space(8.0);
                        let pw_text = if self.cfg.custom_password_enabled {
                            "🔒 Contraseña fija activa"
                        } else {
                            "🔓 Sin contraseña fija"
                        };
                        ui.label(RichText::new(pw_text).size(11.5).color(muted));

                        ui.add_space(8.0);
                        if ui
                            .add_sized([ui.available_width(), 30.0], egui::Button::new("Contraseña, PIN y Permisos"))
                            .clicked()
                        {
                            self.pin_draft = self.cfg.pin.clone();
                            self.pin_enabled_draft = self.cfg.pin_enabled;
                            self.custom_pw_draft =
                                self.cfg.custom_password.clone().unwrap_or_default();
                            self.custom_pw_enabled_draft = self.cfg.custom_password_enabled;
                            self.perms_draft = self.cfg.default_permissions;
                            self.show_access_modal = true;
                        }

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        if ui
                            .checkbox(&mut self.autostart_enabled, "Iniciar con Windows (2do plano)")
                            .changed()
                        {
                            match autostart::set_enabled(self.autostart_enabled) {
                                Ok(v) => {
                                    self.autostart_enabled = v;
                                    self.notify(if v {
                                        "Inicio automático con Windows activado"
                                    } else {
                                        "Inicio automático desactivado"
                                    });
                                }
                                Err(_) => self.notify("No se pudo cambiar el inicio automático"),
                            }
                        }
                    });

                    // Active incoming sessions
                    if !sessions.is_empty() {
                        ui.add_space(10.0);
                        for s in &sessions {
                            Frame::new()
                                .fill(Color32::from_rgba_unmultiplied(34, 197, 94, 28))
                                .stroke(Stroke::new(1.0, success))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("● En control: {}", s.viewer_name))
                                                .size(12.0)
                                                .strong()
                                                .color(success),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("Cortar").color(Color32::WHITE),
                                                        )
                                                        .fill(danger),
                                                    )
                                                    .clicked()
                                                {
                                                    if let Some(a) = &self.agent {
                                                        a.disconnect_session(s.session_id.clone());
                                                    }
                                                }
                                            },
                                        );
                                    });
                                });
                        }
                    }

                    // Bottom Server Card
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                        self.card_frame().show(ui, |ui| {
                            ui.label(
                                RichText::new("SERVIDOR PROPIO (RUST)")
                                    .size(10.5)
                                    .strong()
                                    .color(muted),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(&self.cfg.server_url)
                                    .font(FontId::monospace(11.0))
                                    .color(muted),
                            );
                            ui.add_space(8.0);
                            if ui
                                .add_sized(
                                    [ui.available_width(), 28.0],
                                    egui::Button::new("Configurar servidor / preferencias"),
                                )
                                .clicked()
                            {
                                self.prefs_draft = self.cfg.preferences.clone();
                                self.server_url_draft = self.cfg.server_url.clone();
                                self.device_name_draft = self.cfg.device_name.clone();
                                self.auth_token_draft =
                                    self.cfg.auth_token.clone().unwrap_or_default();
                                self.settings_tab = SettingsTab::Server;
                                self.show_settings = true;
                            }
                        });
                    });
                });

            // ============================================================
            // Topbar
            // ============================================================
            egui::TopBottomPanel::top("rc_topbar")
                .exact_height(52.0)
                .frame(Frame::new().fill(sidebar_bg).inner_margin(Margin::symmetric(20, 10)))
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.label(RichText::new(&self.cfg.device_name).size(14.5).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("⚙ Configuración").clicked() {
                                self.prefs_draft = self.cfg.preferences.clone();
                                self.server_url_draft = self.cfg.server_url.clone();
                                self.device_name_draft = self.cfg.device_name.clone();
                                self.auth_token_draft =
                                    self.cfg.auth_token.clone().unwrap_or_default();
                                self.settings_tab = SettingsTab::General;
                                self.show_settings = true;
                            }
                            if ui
                                .button("Cambiar a WebView2")
                                .on_hover_text("Reiniciar usando la interfaz WebView2")
                                .clicked()
                            {
                                self.switch_to_webview(ctx);
                            }
                            ui.label(
                                RichText::new("🔒 Cifrado E2E AES-256-GCM")
                                    .size(11.5)
                                    .color(success),
                            );
                        });
                    });
                });

            // ============================================================
            // Main Workspace
            // ============================================================
            egui::CentralPanel::default()
                .frame(
                    Frame::new()
                        .fill(ctx.style().visuals.panel_fill)
                        .inner_margin(Margin::same(24)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        // Connect Hero Card
                        self.card_frame().show(ui, |ui| {
                            ui.label(
                                RichText::new("Controlar dispositivo remoto")
                                    .size(20.0)
                                    .strong(),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "Conéctate a cualquier equipo ingresando su ID de 9 dígitos. Video H.264, conexión directa P2P WebRTC y cifrado de extremo a extremo.",
                                )
                                .size(12.5)
                                .color(muted),
                            );
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.target_id_input)
                                        .hint_text("ID del dispositivo (ej. 482 910 374)")
                                        .desired_width(240.0),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.target_pin_input)
                                        .hint_text("Contraseña o PIN")
                                        .password(true)
                                        .desired_width(170.0),
                                );
                                if ui
                                    .add_sized(
                                        [120.0, 30.0],
                                        egui::Button::new(
                                            RichText::new("▶ Conectar")
                                                .strong()
                                                .color(Color32::WHITE),
                                        )
                                        .fill(Color32::from_rgb(2, 132, 199)),
                                    )
                                    .clicked()
                                {
                                    let id = self.target_id_input.clone();
                                    self.launch_web_viewer(Some(&id));
                                }
                            });
                        });

                        ui.add_space(20.0);

                        // Saved Devices Header
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Dispositivos guardados ({})",
                                    self.cfg.saved_devices.len()
                                ))
                                .size(15.0)
                                .strong(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("+ Guardar equipo").clicked() {
                                    self.show_add_device = true;
                                }
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.device_query)
                                        .hint_text("Buscar equipo...")
                                        .desired_width(180.0),
                                );
                            });
                        });

                        ui.add_space(12.0);

                        let q = self.device_query.trim().to_lowercase();
                        let devices: Vec<SavedDevice> = self
                            .cfg
                            .saved_devices
                            .iter()
                            .filter(|d| {
                                q.is_empty()
                                    || d.name.to_lowercase().contains(&q)
                                    || d.id.to_lowercase().contains(&q)
                            })
                            .cloned()
                            .collect();

                        if devices.is_empty() {
                            self.card_frame().show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(16.0);
                                    ui.label(
                                        RichText::new("No hay dispositivos guardados")
                                            .size(14.0)
                                            .strong(),
                                    );
                                    ui.label(
                                        RichText::new(
                                            "Agrega tus PCs para conectarte en un solo clic.",
                                        )
                                        .size(12.0)
                                        .color(muted),
                                    );
                                    ui.add_space(16.0);
                                });
                            });
                        } else {
                            let mut to_delete: Option<String> = None;
                            egui::Grid::new("saved_devices_grid")
                                .num_columns(3)
                                .spacing(Vec2::new(14.0, 14.0))
                                .show(ui, |ui| {
                                    for (idx, dev) in devices.iter().enumerate() {
                                        self.card_frame().show(ui, |ui| {
                                            ui.set_width(240.0);
                                            ui.label(RichText::new(&dev.name).size(14.0).strong());
                                            ui.label(
                                                RichText::new(format_id(&dev.id))
                                                    .font(FontId::monospace(12.5))
                                                    .color(accent),
                                            );
                                            ui.add_space(6.0);
                                            let sub = if dev.pin.is_some() {
                                                "🔑 Acceso rápido guardado"
                                            } else {
                                                "Requiere aprobación"
                                            };
                                            ui.label(RichText::new(sub).size(11.0).color(muted));
                                            ui.add_space(8.0);
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("▶ Conectar")
                                                                .color(Color32::WHITE),
                                                        )
                                                        .fill(Color32::from_rgb(2, 132, 199)),
                                                    )
                                                    .clicked()
                                                {
                                                    self.launch_web_viewer(Some(&dev.id));
                                                }
                                                if ui.button("Eliminar").clicked() {
                                                    to_delete = Some(dev.id.clone());
                                                }
                                            });
                                        });
                                        if (idx + 1) % 3 == 0 {
                                            ui.end_row();
                                        }
                                    }
                                });
                            if let Some(id) = to_delete {
                                self.cfg.saved_devices.retain(|d| d.id != id);
                                self.cfg.save();
                                self.notify("Dispositivo eliminado");
                            }
                        }
                    });
                });

            // ============================================================
            // Incoming Connection Approval Modal
            // ============================================================
            if let Some(mut req) = pending {
                let mut decided = false;
                let mut accepted = false;
                egui::Window::new("Solicitud de conexión entrante")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label(
                            RichText::new(format!("{} desea conectarse a este equipo", req.viewer_name))
                                .size(14.0)
                                .strong(),
                        );
                        ui.add_space(8.0);
                        ui.checkbox(&mut req.perms.allow_input, "Permitir control de mouse y teclado");
                        ui.checkbox(&mut req.perms.allow_audio, "Permitir audio del sistema");
                        ui.checkbox(&mut req.perms.allow_clipboard, "Permitir portapapeles");
                        ui.checkbox(&mut req.perms.allow_file_transfer, "Permitir transferencia de archivos");
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Rechazar").color(Color32::WHITE))
                                        .fill(danger),
                                )
                                .clicked()
                            {
                                decided = true;
                                accepted = false;
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("Aceptar").color(Color32::WHITE))
                                        .fill(success),
                                )
                                .clicked()
                            {
                                decided = true;
                                accepted = true;
                            }
                        });
                    });

                if decided {
                    if let Some(a) = &self.agent {
                        a.respond_approval(req.session_id.clone(), accepted, req.perms);
                    }
                    self.shared.lock().unwrap().pending = None;
                } else if let Ok(mut st) = self.shared.lock() {
                    st.pending = Some(req);
                }
            }

            // ============================================================
            // Settings Modal
            // ============================================================
            if self.show_settings {
                let mut open = self.show_settings;
                egui::Window::new("Configuración")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(520.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.settings_tab, SettingsTab::General, "General");
                            ui.selectable_value(
                                &mut self.settings_tab,
                                SettingsTab::Media,
                                "Grabaciones y Video",
                            );
                            ui.selectable_value(&mut self.settings_tab, SettingsTab::Server, "Servidor");
                        });
                        ui.separator();
                        ui.add_space(8.0);

                        match self.settings_tab {
                            SettingsTab::General => {
                                if ui
                                    .checkbox(
                                        &mut self.autostart_enabled,
                                        "Iniciar automáticamente con Windows (System Tray)",
                                    )
                                    .changed()
                                {
                                    let _ = autostart::set_enabled(self.autostart_enabled);
                                }
                                ui.checkbox(
                                    &mut self.prefs_draft.minimize_to_tray,
                                    "Minimizar al System Tray al cerrar",
                                );
                                ui.checkbox(
                                    &mut self.prefs_draft.show_host_widget,
                                    "Mostrar widget flotante al recibir conexión",
                                );
                                ui.checkbox(&mut self.dark_mode, "Tema oscuro");
                                ui.add_space(6.0);
                                if !shortcut::exists() {
                                    if ui
                                        .button("➕ Crear acceso directo en el Escritorio")
                                        .clicked()
                                    {
                                        match shortcut::create() {
                                            Ok(_) => self.notify("Acceso directo creado en el Escritorio"),
                                            Err(e) => self.notify(e),
                                        }
                                    }
                                } else {
                                    ui.label(
                                        RichText::new("✓ Acceso directo en el Escritorio ya creado")
                                            .size(11.5)
                                            .color(success),
                                    );
                                }
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    ui.label("Modo de interfaz:");
                                    egui::ComboBox::from_id_salt("ui_mode_combo")
                                        .selected_text(match self.prefs_draft.ui_mode.as_str() {
                                            "webview" => "WebView2 (Completa)",
                                            "native" => "Nativo GPU (Sin WebView2)",
                                            _ => "Automático",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.prefs_draft.ui_mode,
                                                "auto".to_string(),
                                                "Automático",
                                            );
                                            ui.selectable_value(
                                                &mut self.prefs_draft.ui_mode,
                                                "webview".to_string(),
                                                "WebView2 (Completa)",
                                            );
                                            ui.selectable_value(
                                                &mut self.prefs_draft.ui_mode,
                                                "native".to_string(),
                                                "Nativo GPU (Sin WebView2)",
                                            );
                                        });
                                });
                            }
                            SettingsTab::Media => {
                                ui.label("Carpeta de grabaciones locales (.webm):");
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.prefs_draft.recordings_dir)
                                            .desired_width(360.0),
                                    );
                                    if ui.button("Abrir").clicked() {
                                        let dir = if self.prefs_draft.recordings_dir.trim().is_empty() {
                                            default_recordings_dir()
                                        } else {
                                            self.prefs_draft.recordings_dir.clone()
                                        };
                                        let _ = std::fs::create_dir_all(&dir);
                                        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                                    }
                                });
                                ui.add_space(8.0);
                                ui.add(
                                    egui::Slider::new(&mut self.prefs_draft.default_fps, 10..=60)
                                        .text("FPS predeterminados"),
                                );
                                ui.add(
                                    egui::Slider::new(
                                        &mut self.prefs_draft.default_bitrate_kbps,
                                        500..=8000,
                                    )
                                    .text("Bitrate H.264 (kbps)"),
                                );
                                ui.checkbox(
                                    &mut self.prefs_draft.prefer_p2p,
                                    "Preferir conexión directa P2P (WebRTC)",
                                );
                                ui.checkbox(
                                    &mut self.prefs_draft.mute_audio_on_connect,
                                    "Silenciar audio al conectar",
                                );
                            }
                            SettingsTab::Server => {
                                ui.label("Nombre de este equipo:");
                                ui.text_edit_singleline(&mut self.device_name_draft);
                                ui.label("URL WebSocket del servidor:");
                                ui.text_edit_singleline(&mut self.server_url_draft);
                                ui.label("Token del servidor (opcional):");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.auth_token_draft)
                                        .password(true),
                                );
                            }
                        }

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Guardar cambios").color(Color32::WHITE),
                                    )
                                    .fill(Color32::from_rgb(2, 132, 199)),
                                )
                                .clicked()
                            {
                                let switched_to_webview = self.prefs_draft.ui_mode == "webview";
                                if self.prefs_draft.recordings_dir.trim().is_empty() {
                                    self.prefs_draft.recordings_dir = default_recordings_dir();
                                }
                                let server_changed =
                                    self.server_url_draft.trim() != self.cfg.server_url
                                        || self.device_name_draft.trim() != self.cfg.device_name;
                                self.cfg.preferences = self.prefs_draft.clone();
                                self.cfg.server_url = self.server_url_draft.trim().to_string();
                                self.cfg.device_name = self.device_name_draft.trim().to_string();
                                self.cfg.auth_token =
                                    Some(self.auth_token_draft.trim().to_string())
                                        .filter(|s| !s.is_empty());
                                self.cfg.save();
                                if server_changed {
                                    self.restart_agent();
                                }
                                self.notify("Configuración guardada");
                                if switched_to_webview {
                                    self.switch_to_webview(ctx);
                                }
                            }
                            if ui.button("Cerrar").clicked() {
                                self.show_settings = false;
                            }
                        });
                    });
                if !open {
                    self.show_settings = false;
                }
            }

            // ============================================================
            // Access & Permissions Modal
            // ============================================================
            if self.show_access_modal {
                let mut open = self.show_access_modal;
                egui::Window::new("Contraseña, PIN y Permisos")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.checkbox(&mut self.pin_enabled_draft, "Acceso con PIN temporal");
                        if self.pin_enabled_draft {
                            ui.text_edit_singleline(&mut self.pin_draft);
                        }
                        ui.add_space(6.0);
                        ui.checkbox(
                            &mut self.custom_pw_enabled_draft,
                            "Acceso directo con contraseña fija",
                        );
                        if self.custom_pw_enabled_draft {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.custom_pw_draft)
                                    .password(true),
                            );
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.checkbox(&mut self.perms_draft.allow_input, "Control de mouse y teclado");
                        ui.checkbox(&mut self.perms_draft.allow_audio, "Audio remoto");
                        ui.checkbox(&mut self.perms_draft.allow_clipboard, "Portapapeles Ctrl+C/V");
                        ui.checkbox(&mut self.perms_draft.allow_file_transfer, "Transferencia de archivos");
                        ui.add_space(10.0);
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Guardar").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(2, 132, 199)),
                            )
                            .clicked()
                        {
                            self.cfg.pin = self.pin_draft.trim().to_string();
                            self.cfg.pin_enabled = self.pin_enabled_draft;
                            self.cfg.custom_password =
                                Some(self.custom_pw_draft.trim().to_string())
                                    .filter(|s| !s.is_empty());
                            self.cfg.custom_password_enabled = self.custom_pw_enabled_draft
                                && self.cfg.custom_password.is_some();
                            self.cfg.default_permissions = self.perms_draft;
                            self.cfg.save();
                            if let Some(a) = &self.agent {
                                a.update_access(
                                    self.cfg.pin.clone(),
                                    self.cfg.pin_enabled,
                                    self.cfg.custom_password.clone(),
                                    self.cfg.custom_password_enabled,
                                    self.cfg.default_permissions,
                                );
                            }
                            self.show_access_modal = false;
                            self.notify("Seguridad y permisos guardados");
                        }
                    });
                if !open {
                    self.show_access_modal = false;
                }
            }

            // ============================================================
            // Add Saved Device Modal
            // ============================================================
            if self.show_add_device {
                let mut open = self.show_add_device;
                egui::Window::new("Guardar equipo")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("Nombre:");
                        ui.text_edit_singleline(&mut self.new_dev_name);
                        ui.label("ID del dispositivo:");
                        ui.text_edit_singleline(&mut self.new_dev_id);
                        ui.label("PIN / Contraseña (opcional):");
                        ui.add(egui::TextEdit::singleline(&mut self.new_dev_pin).password(true));
                        ui.add_space(10.0);
                        if ui
                            .add(
                                egui::Button::new(RichText::new("Guardar").color(Color32::WHITE))
                                    .fill(Color32::from_rgb(2, 132, 199)),
                            )
                            .clicked()
                        {
                            let clean = normalize_device_id(&self.new_dev_id);
                            if !clean.is_empty() && !self.new_dev_name.trim().is_empty() {
                                let dev = SavedDevice {
                                    id: clean.clone(),
                                    name: self.new_dev_name.trim().to_string(),
                                    pin: Some(self.new_dev_pin.trim().to_string())
                                        .filter(|s| !s.is_empty()),
                                    notes: None,
                                };
                                self.cfg.saved_devices.retain(|d| d.id != clean);
                                self.cfg.saved_devices.push(dev);
                                self.cfg.save();
                                self.new_dev_name.clear();
                                self.new_dev_id.clear();
                                self.new_dev_pin.clear();
                                self.show_add_device = false;
                                self.notify("Dispositivo guardado");
                            }
                        }
                    });
                if !open {
                    self.show_add_device = false;
                }
            }

            // Toast notification
            if let Some(msg) = toast {
                egui::Area::new(egui::Id::new("rc_toast"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
                    .show(ctx, |ui| {
                        Frame::new()
                            .fill(sidebar_bg)
                            .stroke(Stroke::new(1.0, accent))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.label(RichText::new(msg).size(12.5).strong());
                            });
                    });
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1160.0, 740.0])
            .with_min_inner_size([860.0, 560.0])
            .with_title("RemoteConnect")
            .with_visible(!start_in_background),
        ..Default::default()
    };

    let _ = eframe::run_native(
        "RemoteConnect",
        options,
        Box::new(move |_cc| Ok(Box::new(NativeRemoteApp::new(cfg)))),
    );
}

#[cfg(not(target_os = "windows"))]
pub fn run_native_fallback(_cfg: crate::AppConfig, _start_in_background: bool) {}
