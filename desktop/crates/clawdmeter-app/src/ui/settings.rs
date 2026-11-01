//! The settings pages: Device, Accounts, Hooks and Settings,
//! one view behind four sidebar entries. The setup wizard borrows the Hooks
//! page.
//!
//! It edits the same config file the daemon reads (through
//! `ConfigFile`/`Settings`, which keep comments, unknown keys and hand-written
//! values), so a hand-edited config and the GUI never disagree. The daemon
//! re-reads the file every few seconds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clawdmeter_core::autostart::{self, LaunchMode};
use clawdmeter_core::config::{AlertOn, AttentionAnim, ClockMode, ConfigFile, LIVELINESS_MAX};
use clawdmeter_core::wifi::{Connection, Secret, Transport};
use clawdmeter_core::{Paths, Settings};
use clawdmeter_flash::device::{self, FirmwareInfo};
use gpui::{div, img, prelude::*, px, Context, EventEmitter, PathPromptOptions, SharedString, Window};

use super::theme::{Theme, FONT_DISPLAY, FONT_MONO, R_M, R_S};
use super::widgets::{
    button, caption, card, checkbox, field, field_stacked, heading, icon, notice, para, pill, segmented,
    stepped_slider, subheading, toggle, ButtonKind, Tone,
};
use super::{command, live_status, not_running_text, run_blocking, Navigate, REFRESH};
use crate::diff::{line_diff, with_context, DiffLine};
use crate::errors;
use crate::hooks::install::{self as hooks, HookPlan, HookStatus, HOOKS};
use crate::ipc::{self, AppCommand, FirmwareRecord, StatusSnapshot};
use crate::tilde;
use crate::{boards, Page};

struct HookRow {
    dir: PathBuf,
    status: HookStatus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookAction {
    Install,
    Remove,
}

struct HookPreview {
    action: HookAction,
    plans: Vec<HookPlan>,
}

/// How long "Sent" stays on the test alert button.
const TEST_ALERT_SENT: Duration = Duration::from_secs(3);
/// How long a test alert failure stays beside the button.
const TEST_ALERT_FAILED: Duration = Duration::from_secs(8);

#[derive(Default)]
struct DeviceState {
    probing: bool,
    port: Option<String>,
    info: Option<FirmwareInfo>,
    busy: bool,
    /// Result of clearing the board's pairing over USB.
    unpair: Option<(Tone, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollChoice {
    Auto,
    Every(u64),
}

/// The settings pages.
pub struct SettingsView {
    paths: Paths,
    page: Page,
    /// Shown inside the setup wizard (no page heading).
    embedded: bool,
    /// One of this view's pages is on screen.
    visible: bool,
    snap: Option<StatusSnapshot>,
    settings: Settings,
    config_error: Option<String>,
    hooks: Vec<HookRow>,
    preview: Option<HookPreview>,
    hook_msg: Option<(Tone, String)>,
    /// The last test alert's outcome, shown on and beside its button for a
    /// few seconds. `Err` holds the reason it didn't reach the app.
    test_alert: Option<Result<(), String>>,
    /// Bumped per test alert so an older reset timer leaves a newer result.
    test_alert_seq: u32,
    /// The binary the login entry launches (`None`: couldn't be
    /// determined). See [`clawdmeter_core::launch`].
    launch_exe: Option<PathBuf>,
    /// The binary the hooks run: the same, except an AppImage's copy under
    /// the state dir.
    hook_exe: Option<PathBuf>,
    device: DeviceState,
    /// Probe USB for a board (off in tests).
    usb: bool,
    autostart_on: bool,
    /// The login entry launches something other than [`Self::launch_exe`]
    /// (gone, or another copy): what it launches, if readable.
    autostart_stale: Option<Option<PathBuf>>,
    /// The old daemon's login entry (systemd unit, LaunchAgent or Run value).
    legacy: Option<autostart::LegacyEntry>,
    notice: Option<(Tone, String)>,
    /// The Settings page's Advanced section is open.
    advanced: bool,
    ticks: u32,
    /// The WiFi form: network name and password (never saved).
    wifi_ssid: String,
    wifi_pass: String,
    ssid_focus: gpui::FocusHandle,
    pass_focus: gpui::FocusHandle,
    /// The network name was prefilled (once per window).
    wifi_prefilled: bool,
}

/// The WiFi form's text boxes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WifiField {
    Ssid,
    Pass,
}

/// The WiFi section's status line.
fn wifi_line(snap: Option<&StatusSnapshot>, s: &Settings) -> (Tone, String) {
    let Some(snap) = snap else { return (Tone::Plain, "Clawdmeter no se está ejecutando".into()) };
    match (snap.link.as_str(), snap.transport) {
        ("connected", Some(Transport::Wifi)) => match &snap.wifi_addr {
            Some(ip) => (Tone::Ok, format!("Conectado por WiFi ({ip})")),
            None => (Tone::Ok, "Conectado por WiFi".into()),
        },
        ("connected", _) if snap.wifi_fallback => (Tone::Warn, "WiFi no disponible, usando Bluetooth".into()),
        ("connected", _) if s.connection == Connection::Wifi && s.wifi_key.is_none() => {
            (Tone::Ok, "Conectado por Bluetooth · envía una red al aparato para usar WiFi".into())
        }
        ("connected", _) => (Tone::Ok, "Conectado por Bluetooth".into()),
        _ => (Tone::Plain, "Sin conexión con el aparato".into()),
    }
}

/// What the device says about its own WiFi.
fn device_wifi_text(w: &clawdmeter_core::wifi::WifiStatus) -> String {
    let net = w.ssid.as_deref().map(|s| format!(" a {s}")).unwrap_or_default();
    match w.st.as_str() {
        "ok" => match &w.ip {
            Some(ip) => format!("El aparato está conectado{net} ({ip})."),
            None => format!("El aparato está conectado{net}."),
        },
        "connecting" => format!("El aparato se está conectando{net}…"),
        "fail" => format!("El aparato no pudo conectarse{net}. Revisa la contraseña y que la red sea de 2,4 GHz."),
        _ => "El WiFi del aparato está apagado.".into(),
    }
}

impl EventEmitter<Navigate> for SettingsView {}

impl SettingsView {
    /// Build and load the config. USB is probed while the Device page is up.
    pub fn new(paths: Paths, page: Page, cx: &mut Context<Self>) -> Self {
        Self::with_usb(paths, page, true, cx)
    }

    /// [`SettingsView::new`]; `usb: false` never touches serial ports (tests).
    pub fn with_usb(paths: Paths, page: Page, usb: bool, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            if this.update(cx, |v, cx| v.tick(cx)).is_err() {
                break;
            }
        })
        .detach();
        let hook_exe = hooks::hook_binary(&paths.state_dir).ok();
        let mut v = SettingsView {
            settings: Settings::from_config(&ConfigFile::default(), &paths, None),
            paths,
            page,
            embedded: false,
            visible: false,
            snap: None,
            config_error: None,
            hooks: Vec::new(),
            preview: None,
            hook_msg: None,
            test_alert: None,
            test_alert_seq: 0,
            launch_exe: clawdmeter_core::launch::stable_launch_path().ok(),
            hook_exe,
            device: DeviceState::default(),
            usb,
            autostart_on: false,
            autostart_stale: None,
            legacy: None,
            notice: None,
            advanced: false,
            ticks: 0,
            wifi_ssid: String::new(),
            wifi_pass: String::new(),
            ssid_focus: cx.focus_handle(),
            pass_focus: cx.focus_handle(),
            wifi_prefilled: false,
        };
        v.reload();
        v.show(page, false, cx);
        v
    }

    /// Put `page` on screen (`embedded`: inside the setup wizard). Other
    /// pages hide this view.
    pub fn show(&mut self, page: Page, embedded: bool, cx: &mut Context<Self>) {
        self.visible = matches!(page, Page::Device | Page::Accounts | Page::Hooks | Page::Settings);
        if !self.visible {
            return;
        }
        self.page = page;
        self.embedded = embedded;
        self.reload();
        if page == Page::Device && self.usb && !self.device.probing && self.device.port.is_none() {
            self.probe_device(cx);
        }
        if page == Page::Device && !self.wifi_prefilled {
            self.wifi_prefilled = true;
            match self.settings.wifi_ssid.clone() {
                Some(s) => self.wifi_ssid = s,
                // The network this computer is on (Windows); not in tests.
                None if self.usb => run_blocking(cx, crate::transport::current_ssid, |v, ssid, _| {
                    if let (Some(s), true) = (ssid, v.wifi_ssid.is_empty()) {
                        v.wifi_ssid = s;
                    }
                }),
                None => {}
            }
        }
        cx.notify();
    }

    /// Accounts whose hooks need installing or updating.
    pub fn pending_hooks(&self) -> usize {
        self.hooks.iter().filter(|h| h.status.needs_install()).count()
    }

    /// Open the review of installing hooks in every account that needs them
    /// (what the page's install button does).
    pub fn review_hook_install(&mut self, cx: &mut Context<Self>) {
        let pending = self.hooks.iter().filter(|h| h.status.needs_install()).map(|h| h.dir.clone()).collect();
        self.plan_hooks(HookAction::Install, pending);
        cx.notify();
    }

    /// Open or close the Settings page's Advanced section.
    pub fn show_advanced(&mut self, open: bool, cx: &mut Context<Self>) {
        self.advanced = open;
        cx.notify();
    }

    /// Accounts whose settings.json couldn't be read.
    pub fn unreadable_hooks(&self) -> usize {
        self.hooks.iter().filter(|h| matches!(h.status, HookStatus::Unreadable(_))).count()
    }

    /// Accounts the hooks page lists (every configured account).
    pub fn hook_accounts(&self) -> usize {
        self.hooks.len()
    }

    /// Accounts whose hooks are set up.
    pub fn installed_hooks(&self) -> usize {
        self.hooks.iter().filter(|h| h.status == HookStatus::Installed).count()
    }

    fn reload(&mut self) {
        match Settings::load(&self.paths) {
            Ok(s) => {
                self.settings = s;
                self.config_error = None;
            }
            Err(e) => {
                self.config_error = Some(format!(
                    "No se pudo leer {}: {e}",
                    tilde(&self.paths.config_file.display().to_string(), &self.paths.home)
                ))
            }
        }
        self.hooks = self
            .settings
            .config_dirs
            .iter()
            .map(|d| HookRow {
                dir: d.clone(),
                status: match &self.hook_exe {
                    Some(exe) => hooks::dir_status(d, exe),
                    None => HookStatus::Unreadable("No se encontró el ejecutable de Clawdmeter.".into()),
                },
            })
            .collect();
        self.autostart_on = autostart::is_enabled(&self.paths.home, LaunchMode::Tray);
        self.autostart_stale = self
            .launch_exe
            .as_ref()
            .filter(|want| autostart::is_stale(&self.paths.home, LaunchMode::Tray, want))
            .map(|_| autostart::entry_exe(&self.paths.home, LaunchMode::Tray));
        self.legacy = autostart::legacy_entry(&self.paths.home);
        self.snap = live_status(&self.paths);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.ticks += 1;
        self.snap = live_status(&self.paths);
        // Pick up hand edits and other windows' changes every few seconds,
        // but never under an open diff preview.
        if self.ticks.is_multiple_of(6) && self.preview.is_none() {
            self.reload();
        }
        // Notice a board being plugged in (only while the Device page is up,
        // so it never competes with the flasher for the port).
        let on_device = self.visible && self.page == Page::Device;
        if self.usb && on_device && self.ticks.is_multiple_of(4) && !self.device.probing && self.device.port.is_none() {
            if let Some(p) = device::list_ports().into_iter().next() {
                self.device.port = Some(p.path);
                self.probe_device(cx);
            }
        }
        cx.notify();
    }

    /// Change settings: re-read the file, apply the change, write only the
    /// keys whose value differs.
    fn save(&mut self, change: impl FnOnce(&mut Settings, &mut ConfigFile)) {
        let path = self.paths.config_file.clone();
        match write_settings(&self.paths, change) {
            Ok(()) => self.notice = None,
            Err(e) => self.notice = Some((Tone::Bad, errors::save_failed("los ajustes", &path, &self.paths.home, &e))),
        }
        self.reload();
    }

    fn probe_device(&mut self, cx: &mut Context<Self>) {
        self.device.probing = true;
        run_blocking(
            cx,
            || {
                let port = device::list_ports().into_iter().next().map(|p| p.path);
                let Some(port) = port else { return (None, None) };
                let info = device::probe_info(&port, Duration::from_millis(1500)).ok().flatten();
                (Some(port), info)
            },
            |v, (port, info), _| {
                v.device.probing = false;
                v.device.port = port;
                if let Some(i) = &info {
                    let _ =
                        ipc::save_firmware_record(&v.paths, &FirmwareRecord { env: i.env.clone(), ver: i.ver.clone() });
                }
                v.device.info = info;
            },
        );
    }

    /// Clear every Bluetooth bond on the board (firmware `unpair`), so it
    /// can pair with this or another computer again.
    fn unpair(&mut self, cx: &mut Context<Self>) {
        let Some(port) = self.device.port.clone() else { return };
        self.device.busy = true;
        self.device.unpair = None;
        run_blocking(
            cx,
            move || device::query_json(&port, "unpair", "unpair", Duration::from_millis(1500)),
            |v, r, _| {
                v.device.busy = false;
                v.device.unpair = Some(match r {
                    Ok(Some(_)) => (
                        Tone::Ok,
                        "Se borró el emparejamiento de la placa. Quita Clawdmeter de los ajustes de Bluetooth de este \
                         ordenador y vuelve a emparejarla."
                            .into(),
                    ),
                    Ok(None) => (
                        Tone::Bad,
                        "El dispositivo no responde. Vuelve a conectar el USB o actualiza el firmware y reinténtalo."
                            .into(),
                    ),
                    Err(e) => (Tone::Bad, e.to_string()),
                });
            },
        );
    }

    fn add_account(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Añadir carpeta de cuenta de Claude Code".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |v, cx| {
                    for p in paths {
                        if !v.settings.config_dirs.contains(&p) {
                            v.save(|s, _| s.config_dirs.push(p));
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The binary to set the hooks (`hooks`) or the login entry up with,
    /// or why not.
    fn launchable(&self, hooks: bool) -> Result<PathBuf, String> {
        let missing = || "No se encontró el ejecutable de Clawdmeter.".to_owned();
        let exe = self.launch_exe.clone().ok_or_else(missing)?;
        if let Some(u) = clawdmeter_core::launch::unstable(&exe) {
            return Err(format!("Clawdmeter se está ejecutando desde {}. {}", u.location_noun(), u.advice()));
        }
        if hooks {
            self.hook_exe.clone().ok_or_else(missing)
        } else {
            Ok(exe)
        }
    }

    fn plan_hooks(&mut self, action: HookAction, dirs: Vec<PathBuf>) {
        self.hook_msg = None;
        let exe = match (action, self.launchable(true)) {
            (_, Ok(exe)) => exe,
            (HookAction::Install, Err(why)) => {
                self.hook_msg = Some((Tone::Bad, why));
                return;
            }
            // Removing doesn't write a path.
            (HookAction::Remove, Err(_)) => match self.hook_exe.clone() {
                Some(exe) => exe,
                None => return,
            },
        };
        let mut plans = Vec::new();
        for d in dirs {
            let plan = match action {
                HookAction::Install => hooks::plan_install(&d, &exe),
                HookAction::Remove => hooks::plan_uninstall(&d),
            };
            match plan {
                Ok(p) if p.changes() => plans.push(p),
                Ok(_) => {}
                Err(e) => {
                    let f = tilde(&d.join("settings.json").display().to_string(), &self.paths.home);
                    self.hook_msg = Some((Tone::Bad, format!("{f}: {e}")));
                    return;
                }
            }
        }
        if plans.is_empty() {
            self.hook_msg = Some((Tone::Ok, "No hace falta ningún cambio.".into()));
        } else {
            self.preview = Some(HookPreview { action, plans });
        }
    }

    fn apply_hooks(&mut self) {
        let Some(p) = self.preview.take() else { return };
        let mut done = 0;
        for plan in &p.plans {
            if let Err(e) = plan.apply() {
                // Files are written one by one: say which ones made it.
                let what = tilde(&plan.path.display().to_string(), &self.paths.home);
                let saved = match done {
                    0 => String::new(),
                    1 => " Otro archivo ya se había guardado.".into(),
                    n => format!(" Otros {n} archivos ya se habían guardado."),
                };
                self.hook_msg = Some((Tone::Bad, format!("No se pudo escribir {what}. {e}{saved}")));
                self.reload();
                return;
            }
            done += 1;
        }
        let accounts = if done == 1 { "1 cuenta".to_string() } else { format!("{done} cuentas") };
        self.hook_msg = Some((
            Tone::Ok,
            match p.action {
                HookAction::Install => {
                    format!("Hooks instalados en {accounts}. Tus ajustes originales se guardan en settings.json.bak.")
                }
                HookAction::Remove => {
                    format!("Hooks quitados de {accounts}. Tus ajustes originales se guardan en settings.json.bak.")
                }
            },
        ));
        self.reload();
    }

    fn send_test_alert(&mut self, cx: &mut Context<Self>) {
        let path = self.paths.attention_file();
        let body =
            serde_json::json!({"m": "Alerta de prueba de Clawdmeter", "s": clawdmeter_core::attention::TEST_SESSION});
        let written =
            std::fs::create_dir_all(&self.paths.state_dir).and_then(|_| std::fs::write(&path, body.to_string()));
        self.test_alert = Some(match written {
            Ok(()) if self.snap.is_none() => Err("Clawdmeter no se está ejecutando.".into()),
            Ok(()) => Ok(()),
            Err(e) => Err(format!("No se pudo enviar. {}", errors::io_message(&e))),
        });
        self.test_alert_seq += 1;
        let seq = self.test_alert_seq;
        let hold = if self.test_alert == Some(Ok(())) { TEST_ALERT_SENT } else { TEST_ALERT_FAILED };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(hold).await;
            let _ = this.update(cx, |v, cx| {
                if v.test_alert_seq == seq {
                    v.test_alert = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // ---------------------------------------------------------------- panes

    fn device_pane(&mut self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let snap = self.snap.clone();
        let rec = ipc::load_firmware_record(&self.paths);
        let env = self.device.info.as_ref().map(|i| i.env.clone()).or_else(|| rec.as_ref().map(|r| r.env.clone()));
        let ver = self
            .device
            .info
            .as_ref()
            .map(|i| i.ver.clone())
            .or_else(|| rec.as_ref().map(|r| r.ver.clone()))
            .or_else(|| snap.as_ref().and_then(|s| s.device_firmware.clone()));
        let board = env.as_deref().and_then(boards::board);
        let name: SharedString = match (board, &env) {
            (Some(b), _) => b.name.into(),
            (None, Some(e)) => e.clone().into(),
            (None, None) => "Placa sin identificar".into(),
        };
        // Without a live USB answer, the identity is what was last seen.
        let cached = self.device.info.is_none() && env.is_some();
        let detail: Option<SharedString> = match (board, cached) {
            (Some(b), true) => Some(format!("{} · última detección", b.product).into()),
            (Some(b), false) => Some(b.product.into()),
            (None, true) => Some("Última detección".into()),
            (None, false) => None,
        };
        let update = snap.as_ref().and_then(|s| s.update_available().map(str::to_owned));
        let (link_tone, link): (Tone, SharedString) = match snap.as_ref() {
            None => (Tone::Plain, "Clawdmeter no se está ejecutando".into()),
            Some(s) => match s.link.as_str() {
                "connected" if s.transport == Some(Transport::Wifi) => (Tone::Ok, "WiFi conectado".into()),
                "connected" => (Tone::Ok, "Bluetooth conectado".into()),
                "no_adapter" => (Tone::Bad, "Bluetooth no disponible".into()),
                "connecting" => (Tone::Accent, "Conectando…".into()),
                _ => (Tone::Plain, "Buscando Clawdmeter".into()),
            },
        };
        let usb: SharedString = match (&self.device.port, self.device.probing) {
            (_, true) => "Comprobando USB…".into(),
            (Some(p), _) => format!("USB: {p}").into(),
            (None, _) if board.is_none() => "Conecta el USB para identificar la placa.".into(),
            (None, _) => "USB no conectado".into(),
        };
        let firmware: SharedString = match (&ver, &update) {
            (Some(v), Some(u)) => {
                format!("Firmware v{} · v{} disponible", v.trim_start_matches('v'), u.trim_start_matches('v')).into()
            }
            (Some(v), None) => format!("Firmware v{}", v.trim_start_matches('v')).into(),
            (None, _) => "Versión de firmware desconocida".into(),
        };
        let art = div().flex().flex_shrink_0().items_center().justify_center().size(px(150.)).child(match board {
            Some(b) => img(b.illustration()).size(px(150.)).into_any_element(),
            None => super::widgets::clawd(4.).into_any_element(),
        });
        let (label, kind) = match &update {
            Some(_) => ("Actualizar firmware", ButtonKind::Primary),
            None => ("Grabar firmware…", ButtonKind::Normal),
        };
        let card = card(t).flex_row().items_center().gap(px(20.)).p(px(20.)).child(art).child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().font_family(FONT_DISPLAY).text_size(px(20.)).child(name))
                        .when_some(detail, |d, x| d.child(div().text_size(px(12.5)).text_color(t.mute).child(x))),
                )
                .child(div().flex().flex_wrap().gap(px(6.)).child(pill(t, link, link_tone)))
                .child(div().text_size(px(13.)).text_color(t.dim).child(firmware))
                .child(div().text_size(px(13.)).text_color(t.mute).child(usb))
                .child(div().flex().pt(px(4.)).child(button(
                    t,
                    "update-now",
                    label,
                    kind,
                    true,
                    cx.listener(|_, _, _, cx| cx.emit(Navigate(Page::Flash))),
                ))),
        );
        let mut pane = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(heading(t, "Dispositivo"))
            .child(div().pt(px(8.)).pb(px(10.)).child(card));

        let me = cx.entity().downgrade();
        pane = pane.child(field(
            t,
            "Reloj",
            "Sustituye el título Uso por un reloj. Auto sigue el formato de hora de este ordenador.",
            segmented(
                t,
                "clock",
                &[
                    (ClockMode::Off, "No"),
                    (ClockMode::Auto, "Auto"),
                    (ClockMode::H12, "12 h"),
                    (ClockMode::H24, "24 h"),
                ],
                &self.settings.clock,
                true,
                move |m, _, cx| {
                    let _ = me.update(cx, |v, cx| {
                        v.save(|s, _| s.clock = m);
                        cx.notify();
                    });
                },
            ),
        ));
        let me = cx.entity().downgrade();
        let level = self.settings.liveliness;
        pane = pane.child(field_stacked(
            t,
            "Animación tontorrona",
            "Lo animado que está Clawd",
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .child(stepped_slider(
                    t,
                    "liveliness",
                    LIVELINESS_MAX,
                    level,
                    &[(1, "Sin tonterías"), (3, "Alguna tontería"), (5, "Todo tonterías")],
                    true,
                    move |level, _, cx| {
                        let _ = me.update(cx, |v, cx| {
                            v.save(|s, _| s.liveliness = level);
                            cx.notify();
                        });
                    },
                ))
                .child(
                    div()
                        .debug_selector(move || format!("liveliness-info-{level}"))
                        .text_size(px(13.))
                        .text_color(t.dim)
                        .text_center()
                        .child(liveliness_info(level)),
                ),
        ));
        let me = cx.entity().downgrade();
        pane = pane.child(field(
            t,
            "Animación de aviso",
            "Cuánto tiempo señala y saluda Clawd cuando Claude te necesita. La alerta sigue visible igualmente.",
            segmented(
                t,
                "attention-anim",
                &AttentionAnim::ALL.map(|a| (a, attention_anim_label(a))),
                &self.settings.attention_anim,
                true,
                move |a, _, cx| {
                    let _ = me.update(cx, |v, cx| {
                        v.save(|s, _| s.attention_anim = a);
                        cx.notify();
                    });
                },
            ),
        ));
        let me = cx.entity().downgrade();
        let pace = self.settings.pace_marker;
        pane = pane.child(field(
            t,
            "Marcador de ritmo",
            "Marca cuánto llevas de sesión y de semana. Las barras se ponen ámbar si el uso va por delante.",
            toggle(t, "pace-marker", pace, true, move |_, _, cx| {
                let _ = me.update(cx, |v, cx| {
                    v.save(|s, _| s.pace_marker = !pace);
                    cx.notify();
                });
            }),
        ));
        let (me1, me2) = (cx.entity().downgrade(), cx.entity().downgrade());
        let (alert, reset) = (self.settings.alert_chime, self.settings.reset_chime);
        pane = pane
            .child(field(
                t,
                "Sonido de alerta",
                "Suena cuando Claude te necesita, en placas con altavoz. Silénciala desde la bandeja hasta reiniciar.",
                toggle(t, "alert-chime", alert, true, move |_, _, cx| {
                    let _ = me1.update(cx, |v, cx| {
                        v.save(|s, _| s.alert_chime = !alert);
                        cx.notify();
                    });
                }),
            ))
            .child(field(
                t,
                "Sonido de reinicio de sesión",
                "Suena cuando se reinicia el uso de tu sesión, en placas con altavoz.",
                toggle(t, "reset-chime", reset, true, move |_, _, cx| {
                    let _ = me2.update(cx, |v, cx| {
                        v.save(|s, _| s.reset_chime = !reset);
                        cx.notify();
                    });
                }),
            ));
        let me = cx.entity().downgrade();
        let on = self.settings.update_check;
        pane = pane.child(field(
            t,
            "Buscar actualizaciones de firmware",
            "Consulta GitHub a diario. Las actualizaciones aparecen aquí y en la bandeja.",
            toggle(t, "update-check", on, true, move |_, _, cx| {
                let _ = me.update(cx, |v, cx| {
                    v.save(|s, _| s.update_check = !on);
                    cx.notify();
                });
            }),
        ));
        let p = self.paths.clone();
        pane = pane.child(field(
            t,
            "Olvidar dispositivo guardado",
            "Borra la dirección guardada en la app y busca un Clawdmeter emparejado. El emparejamiento Bluetooth se mantiene.",
            button(t, "forget", "Olvidar", ButtonKind::Small, true, move |_, _, _| command(&p, AppCommand::Forget)),
        ));
        let on_usb = self.device.port.is_some();
        let me = cx.entity().downgrade();
        pane = pane.child(field(
            t,
            "Borrar el emparejamiento de la placa",
            if on_usb {
                "La placa olvida todos los ordenadores con los que se emparejó para poder emparejarse de nuevo."
            } else {
                "La placa olvida todos los ordenadores con los que se emparejó. Conecta el USB para usarlo."
            },
            button(
                t,
                "unpair",
                "Borrar emparejamiento",
                ButtonKind::Small,
                on_usb && !self.device.busy,
                move |_, _, cx| {
                    let _ = me.update(cx, |v, cx| v.unpair(cx));
                },
            ),
        ));
        if let Some((tone, msg)) = &self.device.unpair {
            pane = pane.child(notice(t, *tone, msg.clone()));
        }
        self.wifi_section(t, window, pane, cx)
    }

    /// The Device page's WiFi section: the link choice, its state, and the
    /// form that sends a network to the device over Bluetooth.
    fn wifi_section(&mut self, t: &Theme, window: &Window, pane: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let snap = self.snap.as_ref();
        let me = cx.entity().downgrade();
        let (tone, line) = wifi_line(snap, &self.settings);
        let mut pane = pane
            .child(div().pt(px(18.)).child(subheading(t, "WiFi")))
            .child(para(
                t,
                "Para cuando el Bluetooth no llega: el aparato se une a tu red y la app le habla por ella. Necesita \
                 una placa con WiFi y el firmware de esta edición.",
            ))
            .child(field(
                t,
                "Conexión",
                "Con WiFi, el Bluetooth queda de respaldo. Solo Bluetooth apaga el WiFi del aparato para ahorrar batería.",
                segmented(
                    t,
                    "connection",
                    &[(Connection::Wifi, "WiFi (Bluetooth de respaldo)"), (Connection::Ble, "Solo Bluetooth")],
                    &self.settings.connection,
                    true,
                    move |c, _, cx| {
                        let _ = me.update(cx, |v, cx| {
                            v.save(|s, _| s.connection = c);
                            cx.notify();
                        });
                    },
                ),
            ))
            .child(div().flex().flex_wrap().gap(px(6.)).py(px(10.)).child(pill(t, line, tone)))
            .when_some(snap.and_then(|s| s.wifi.as_ref()).map(device_wifi_text), |d, text| {
                d.child(caption(t, text))
            });
        let ssid_box = self.text_box(t, window, WifiField::Ssid, cx);
        let pass_box = self.text_box(t, window, WifiField::Pass, cx);
        pane = pane.child(field(t, "Red", "El nombre de la red WiFi (2,4 GHz).", ssid_box)).child(field(
            t,
            "Contraseña",
            "Se envía al aparato y no se guarda en este ordenador.",
            pass_box,
        ));
        let on_ble = snap.is_some_and(|s| s.link == "connected" && s.transport == Some(Transport::Ble));
        let ready = on_ble && !self.wifi_ssid.trim().is_empty();
        let (p, ssid, pass) = (self.paths.clone(), self.wifi_ssid.trim().to_owned(), self.wifi_pass.clone());
        pane = pane.child(field(
            t,
            "Enviar al aparato",
            if on_ble {
                "Le pasa la red por Bluetooth. Después usa WiFi en cuanto la encuentra."
            } else {
                "Hace falta que el aparato esté conectado por Bluetooth."
            },
            button(t, "wifi-send", "Enviar al aparato", ButtonKind::Small, ready, move |_, _, _| {
                command(&p, AppCommand::WifiSend { ssid: ssid.clone(), pass: Secret(pass.clone()) })
            }),
        ));
        if let Some(msg) = snap.and_then(|s| s.wifi_send.clone()) {
            let tone = match snap.and_then(|s| s.wifi_send_ok) {
                Some(true) => Tone::Ok,
                Some(false) => Tone::Bad,
                None => Tone::Accent,
            };
            pane = pane.child(div().pt(px(8.)).child(notice(t, tone, msg)));
        }
        pane
    }

    /// A one-line text box: click to focus, type, Backspace, Ctrl/Cmd+V.
    /// shortcut: no cursor movement, selection or IME; enough for a network
    /// name and password. Upgrade to a real input if the app needs more text.
    fn text_box(&self, t: &Theme, window: &Window, which: WifiField, cx: &mut Context<Self>) -> impl IntoElement {
        let (handle, value, id, placeholder) = match which {
            WifiField::Ssid => (&self.ssid_focus, &self.wifi_ssid, "wifi-ssid", "Nombre de la red"),
            WifiField::Pass => (&self.pass_focus, &self.wifi_pass, "wifi-pass", "Contraseña"),
        };
        let focused = handle.is_focused(window);
        let shown: SharedString = match (value.is_empty(), which) {
            (true, _) => placeholder.into(),
            (false, WifiField::Pass) => "•".repeat(value.chars().count()).into(),
            (false, WifiField::Ssid) => value.clone().into(),
        };
        let h = handle.clone();
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .track_focus(handle)
            .flex()
            .items_center()
            .w(px(240.))
            .h(px(30.))
            .px(px(10.))
            .rounded(px(R_S))
            .border_1()
            .border_color(if focused { t.accent } else { t.line })
            .bg(t.surface)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(px(13.))
            .text_color(if value.is_empty() { t.mute } else { t.text })
            .cursor_text()
            .on_click(cx.listener(move |_, _, window, cx| {
                window.focus(&h);
                cx.notify();
            }))
            .on_key_down(cx.listener(move |v, e: &gpui::KeyDownEvent, _, cx| v.type_key(which, e, cx)))
            .child(shown)
            .when(focused, |d| d.child(div().ml(px(1.)).w(px(1.)).h(px(16.)).bg(t.text)))
    }

    fn type_key(&mut self, which: WifiField, e: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let command_key = k.modifiers.control || k.modifiers.platform;
        let (text, max) = match which {
            WifiField::Ssid => (&mut self.wifi_ssid, 32),
            WifiField::Pass => (&mut self.wifi_pass, 63),
        };
        if k.key == "backspace" {
            text.pop();
        } else if command_key && k.key == "v" {
            if let Some(s) = cx.read_from_clipboard().and_then(|c| c.text()) {
                text.push_str(s.trim_end_matches(['\r', '\n']));
            }
        } else if let (false, Some(c)) = (command_key, &k.key_char) {
            text.push_str(c);
        } else {
            return;
        }
        // The device's limits, in bytes.
        while text.len() > max {
            text.pop();
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn accounts_pane(&mut self, t: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let me = cx.entity().downgrade();
        let mut pane = div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(div().flex().items_center().justify_between().child(heading(t, "Cuentas")).child(button(
                t,
                "add-dir",
                "Añadir carpeta de cuenta…",
                ButtonKind::Small,
                true,
                move |_, _, cx| {
                    let _ = me.update(cx, |v, cx| v.add_account(cx));
                },
            )))
            .child(para(
                t,
                "La carpeta predeterminada de Claude Code es ~/.claude. Añade una carpeta de configuración por cada \
                 cuenta que quieras vigilar. El dispositivo sigue la cuenta cuyo uso de sesión subió más recientemente.",
            ))
            .when_some(self.snap.as_ref(), |d, snap| {
                d.child(caption(
                    t,
                    match &snap.claude {
                        Some(c) => {
                            format!("El uso se lee con {}.", crate::claude_label(Path::new(c), &self.paths.home))
                        }
                        None => "No se encontró Claude Code; el uso se actualiza cada 12 min.".to_owned(),
                    },
                ))
            });
        let active = self.snap.as_ref().and_then(|s| s.account.clone());
        let errors: Vec<ipc::AccountError> = self.snap.as_ref().map(|s| s.errors.clone()).unwrap_or_default();
        let only_one = self.settings.config_dirs.len() <= 1;
        let mut list = div().flex().flex_col().rounded(px(R_M)).border_1().border_color(t.line);
        for (i, d) in self.settings.config_dirs.clone().into_iter().enumerate() {
            let ds = d.display().to_string();
            let err = errors.iter().find(|e| e.dir == ds).cloned();
            let is_active = active.as_deref() == Some(ds.as_str());
            let (meta, chip) = match (&err, is_active) {
                // Transient errors retry on their own; the rest need the user.
                (Some(e), _) if e.retrying => (e.message.clone(), Some(pill(t, "Reintentando", Tone::Warn))),
                (Some(e), _) => (e.message.clone(), Some(pill(t, "Requiere atención", Tone::Bad))),
                (None, true) => (String::new(), Some(pill(t, "En el dispositivo", Tone::Ok))),
                (None, false) if self.snap.is_some() => ("No seleccionada para el dispositivo".to_string(), None),
                (None, false) => ("Abre Clawdmeter para consultar el uso.".to_string(), None),
            };
            let me = cx.entity().downgrade();
            let dir = d.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(12.))
                    .when(i > 0, |r| r.border_t_1().border_color(t.line))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().font_family(FONT_MONO).text_size(px(13.)).child(tilde(&ds, &self.paths.home)))
                            .when(!meta.is_empty(), |d| {
                                d.child(div().text_size(px(12.5)).text_color(t.mute).child(meta))
                            }),
                    )
                    .when_some(chip, |r, c| r.child(c))
                    .child(button(
                        t,
                        SharedString::from(format!("rm-{i}")),
                        "Quitar",
                        ButtonKind::Small,
                        !only_one,
                        move |_, _, cx| {
                            let dir = dir.clone();
                            let _ = me.update(cx, |v, cx| {
                                v.save(|s, _| s.config_dirs.retain(|x| *x != dir));
                                cx.notify();
                            });
                        },
                    )),
            );
        }
        pane = pane.child(list).child(caption(
            t,
            if only_one {
                "Al quitar una carpeta se deja de vigilar; sus archivos y hooks instalados se mantienen. La última \
                 carpeta no se puede quitar."
            } else {
                "Al quitar una carpeta se deja de vigilar; sus archivos y hooks instalados se mantienen."
            },
        ));
        if self.snap.is_none() {
            pane = pane.child(notice(t, Tone::Plain, not_running_text()));
        }
        pane
    }

    fn hooks_pane(&mut self, t: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let mut pane = div().flex().flex_col().gap(px(14.)).when(!self.embedded, |d| {
            d.child(heading(t, "Hooks")).child(para(
                t,
                "Los hooks permiten que Clawd reaccione a la actividad y muestre alertas. El registro de eventos guarda \
                 marcas de tiempo, tipos de evento e ID de sesión. Las alertas guardan también el texto de la \
                 notificación o pregunta para el dispositivo. Los medidores de uso funcionan sin hooks.",
            ))
        });
        let busy = self.preview.is_some();
        // With one account to change, the review button below is the only
        // entry point; rows keep their own actions when there are several.
        let single = self.pending_hooks() == 1;
        let mut list = div().flex().flex_col().rounded(px(R_M)).border_1().border_color(t.line);
        for (i, row) in self.hooks.iter().enumerate() {
            let (meta, chip, action): (String, (&str, Tone), Option<(&str, HookAction)>) = match &row.status {
                HookStatus::Installed => (
                    format!("{} hooks instalados", HOOKS.len()),
                    ("Instalados", Tone::Ok),
                    Some(("Revisar retirada", HookAction::Remove)),
                ),
                HookStatus::Missing => (
                    "Los hooks de Clawdmeter no están instalados. Los demás hooks se mantienen.".into(),
                    ("No instalados", Tone::Plain),
                    Some(("Revisar instalación", HookAction::Install)),
                ),
                HookStatus::Legacy => (
                    "Usa attention-hook.sh. Actualiza a los hooks integrados de Clawdmeter.".into(),
                    ("Hooks antiguos", Tone::Warn),
                    Some(("Revisar actualización", HookAction::Install)),
                ),
                HookStatus::Stale(old) => (
                    if let Some(u) = clawdmeter_core::launch::unstable(Path::new(old)) {
                        format!("Ejecuta {}, {}.", tilde(old, &self.paths.home), u.location())
                    } else if Path::new(old).exists() {
                        format!("Usa otro ejecutable de Clawdmeter: {}.", tilde(old, &self.paths.home))
                    } else {
                        format!("Ejecuta {}, que ya no existe.", tilde(old, &self.paths.home))
                    },
                    ("Desactualizados", Tone::Warn),
                    Some(("Revisar actualización", HookAction::Install)),
                ),
                HookStatus::Partial => (
                    "Faltan algunos hooks.".into(),
                    ("Incompletos", Tone::Warn),
                    Some(("Revisar actualización", HookAction::Install)),
                ),
                HookStatus::Unreadable(e) => (e.clone(), ("Ilegible", Tone::Bad), None),
            };
            let mut r = div()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(14.))
                .py(px(12.))
                .when(i > 0, |r| r.border_t_1().border_color(t.line))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(3.))
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .text_size(px(13.))
                                .truncate()
                                .child(tilde(&row.dir.join("settings.json").display().to_string(), &self.paths.home)),
                        )
                        .child(div().text_size(px(12.5)).text_color(t.mute).child(meta)),
                )
                .child(pill(t, chip.0, chip.1));
            let action = action.filter(|(_, a)| !(single && *a == HookAction::Install));
            if let Some((label, action)) = action {
                let me = cx.entity().downgrade();
                let dir = row.dir.clone();
                r = r.child(button(
                    t,
                    SharedString::from(format!("hook-{i}")),
                    label,
                    ButtonKind::Small,
                    !busy,
                    move |_, _, cx| {
                        let dir = dir.clone();
                        let _ = me.update(cx, |v, cx| {
                            v.plan_hooks(action, vec![dir]);
                            cx.notify();
                        });
                    },
                ));
            }
            list = list.child(r);
        }
        pane = pane.child(list);

        if let Some(p) = &self.preview {
            let n = p.plans.len();
            let title = match (p.action, n) {
                (HookAction::Install, 1) => "Revisar 1 archivo".to_string(),
                (HookAction::Install, n) => format!("Revisar {n} archivos"),
                (HookAction::Remove, _) => "Revisar retirada de hooks".to_string(),
            };
            let (me1, me2) = (cx.entity().downgrade(), cx.entity().downgrade());
            let apply = match p.action {
                HookAction::Install => "Instalar hooks",
                HookAction::Remove => "Quitar hooks",
            };
            pane = pane.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .pt(px(6.))
                    .child(div().flex_1().child(subheading(t, title)))
                    .child(button(t, "apply-hooks", apply, ButtonKind::Primary, true, move |_, _, cx| {
                        let _ = me1.update(cx, |v, cx| {
                            v.apply_hooks();
                            cx.notify();
                        });
                    }))
                    .child(button(t, "cancel-hooks", "Cancelar", ButtonKind::Normal, true, move |_, _, cx| {
                        let _ = me2.update(cx, |v, cx| {
                            v.preview = None;
                            cx.notify();
                        });
                    })),
            );
            pane = pane.child(caption(
                t,
                "Revísalo antes de aplicar. Tus ajustes originales se guardan en settings.json.bak.",
            ));
            for plan in &p.plans {
                // Diff what changes, not how the file was formatted: compare
                // against the old file pretty-printed the way it will be saved.
                let before = plan.before.as_deref().unwrap_or("");
                let pretty_before = serde_json::from_str::<serde_json::Value>(before)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
                    .map(|s| s + "\n");
                let reformatted = pretty_before.as_deref().is_some_and(|p| p.trim() != before.trim());
                let diff = with_context(line_diff(pretty_before.as_deref().unwrap_or(before), &plan.after), 2);
                let mut block = div()
                    .flex()
                    .flex_col()
                    .py(px(10.))
                    .rounded(px(R_S))
                    .bg(t.bg)
                    .border_1()
                    .border_color(t.line)
                    .font_family(FONT_MONO)
                    .text_size(px(12.))
                    .line_height(px(18.));
                for l in diff {
                    let (sign, text, fg, bg) = match l {
                        DiffLine::Same(s) => (" ", s, t.mute, None),
                        DiffLine::Added(s) => ("+", s, t.ok, Some(with_alpha(t.ok, 0.10))),
                        DiffLine::Removed(s) => ("-", s, t.bad, Some(with_alpha(t.bad, 0.10))),
                    };
                    block = block.child(
                        div()
                            .flex()
                            .px(px(12.))
                            .text_color(fg)
                            .when_some(bg, |d, bg| d.bg(bg))
                            .child(div().w(px(16.)).flex_shrink_0().child(sign))
                            .child(div().flex_1().min_w_0().child(text)),
                    );
                }
                let file = tilde(&plan.path.display().to_string(), &self.paths.home);
                let what = match (plan.before.is_none(), reformatted) {
                    (true, _) => format!("{file} (archivo nuevo)"),
                    (false, true) => format!("{file} (también reindentado)"),
                    (false, false) => file,
                };
                pane = pane.child(div().flex().flex_col().gap(px(6.)).child(caption(t, what)).child(block));
            }
        } else {
            let pending: Vec<PathBuf> =
                self.hooks.iter().filter(|h| h.status.needs_install()).map(|h| h.dir.clone()).collect();
            let any_old = self.hooks.iter().any(|h| h.status.needs_install() && h.status != HookStatus::Missing);
            let me1 = cx.entity().downgrade();
            let n = pending.len();
            let label = match (any_old, n) {
                (true, 1) => "Revisar actualización".to_string(),
                (true, n) => format!("Revisar actualizaciones de {n} cuentas"),
                (false, 1) => "Revisar instalación".to_string(),
                (false, n) => format!("Revisar instalación en {n} cuentas"),
            };
            pane = pane.child(div().flex().items_center().gap(px(8.)).when(n > 0, |d| {
                d.child(button(t, "install-all", label, ButtonKind::Primary, true, move |_, _, cx| {
                    let pending = pending.clone();
                    let _ = me1.update(cx, |v, cx| {
                        v.plan_hooks(HookAction::Install, pending);
                        cx.notify();
                    });
                }))
            }));
        }
        if let Some((tone, m)) = &self.hook_msg {
            pane = pane.child(notice(t, *tone, m.clone()));
        }
        if !self.embedded && self.preview.is_none() {
            pane = pane.child(self.alert_kinds(t, cx));
        }
        pane
    }

    /// Which waits alert (`alert_on`), for every account at once. The hooks
    /// stay installed as they are; the app drops the kinds that are off.
    fn alert_kinds(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        type Pick = fn(&mut AlertOn) -> &mut bool;
        let rows: [(&'static str, &'static str, &'static str, Pick); 4] = [
            (
                "alert-permission",
                "Aprobación de herramientas",
                "Claude necesita permiso para usar una herramienta.",
                |a| &mut a.permission,
            ),
            ("alert-question", "Preguntas", "Claude tiene una pregunta para ti.", |a| &mut a.question),
            ("alert-idle", "Sesiones inactivas", "Claude lleva un rato esperando tu siguiente mensaje.", |a| {
                &mut a.idle
            }),
            ("alert-elicitation", "Peticiones MCP", "Un servidor MCP necesita información tuya.", |a| {
                &mut a.elicitation
            }),
        ];
        let mut list = div()
            .flex()
            .flex_col()
            .pt(px(10.))
            .child(subheading(t, "Tipos de alerta"))
            .child(caption(t, "Se aplica a todas las cuentas, en el dispositivo y en la bandeja."));
        let current = self.settings.alert_on;
        for (id, label, help, pick) in rows {
            let mut probe = current;
            let on = *pick(&mut probe);
            let me = cx.entity().downgrade();
            list = list.child(checkbox(t, id, label, help, on, move |_, _, cx| {
                let _ = me.update(cx, |v, cx| {
                    v.save(|s, _| {
                        let flag = pick(&mut s.alert_on);
                        *flag = !*flag;
                    });
                    cx.notify();
                });
            }));
        }
        // Try the alert types (and the chimes) from here.
        let me = cx.entity().downgrade();
        let label = if self.test_alert == Some(Ok(())) { "Enviada" } else { "Enviar alerta de prueba" };
        list.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pt(px(12.))
                .child(button(t, "test-alert", label, ButtonKind::Normal, true, move |_, _, cx| {
                    let _ = me.update(cx, |v, cx| {
                        v.send_test_alert(cx);
                        cx.notify();
                    });
                }))
                .when_some(self.test_alert.clone().and_then(Result::err), |d, why| {
                    d.child(div().text_size(px(12.5)).text_color(t.bad).child(why))
                }),
        )
    }

    fn settings_pane(&mut self, t: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let me = cx.entity().downgrade();
        let on = self.autostart_on;
        let home = self.paths.home.clone();
        let mut pane = div().flex().flex_col().gap(px(6.)).child(heading(t, "Ajustes")).child(field(
            t,
            "Abrir al iniciar sesión",
            "Abre Clawdmeter cuando inicias sesión.",
            toggle(t, "autostart", on, true, move |_, _, cx| {
                let home = home.clone();
                let _ = me.update(cx, |v, cx| {
                    let r = match v.launchable(false) {
                        _ if on => autostart::disable(&home, LaunchMode::Tray).map_err(|e| errors::io_message(&e)),
                        Ok(exe) => autostart::enable(&home, &exe, LaunchMode::Tray).map_err(|e| errors::io_message(&e)),
                        Err(why) => Err(why),
                    };
                    if let Err(why) = r {
                        v.notice = Some((Tone::Bad, format!("No se pudo cambiar Abrir al iniciar sesión. {why}")));
                    }
                    v.reload();
                    cx.notify();
                });
            }),
        ));
        if let (Some(old), Some(exe)) = (self.autostart_stale.clone(), self.launch_exe.clone()) {
            let text = match old {
                Some(o) if clawdmeter_core::launch::unstable(&o).is_some() => format!(
                    "Abrir al iniciar sesión abre {}, {}.",
                    tilde(&o.display().to_string(), &self.paths.home),
                    clawdmeter_core::launch::unstable(&o).map_or("", |u| u.location())
                ),
                Some(o) if !o.exists() => format!(
                    "Abrir al iniciar sesión abre {}, que ya no existe.",
                    tilde(&o.display().to_string(), &self.paths.home)
                ),
                Some(o) => format!(
                    "Abrir al iniciar sesión abre otra copia de Clawdmeter: {}.",
                    tilde(&o.display().to_string(), &self.paths.home)
                ),
                None => "No se puede leer Abrir al iniciar sesión.".into(),
            };
            let me = cx.entity().downgrade();
            let home = self.paths.home.clone();
            pane =
                pane.child(div().pb(px(6.)).child(notice(t, Tone::Warn, text).child(div().flex().pl(px(18.)).child(
                    button(t, "fix-autostart", "Usar esta copia", ButtonKind::Small, true, move |_, _, cx| {
                        let _ = me.update(cx, |v, cx| {
                            let r = match v.launchable(false) {
                                Ok(_) => {
                                    autostart::enable(&home, &exe, LaunchMode::Tray).map_err(|e| errors::io_message(&e))
                                }
                                Err(why) => Err(why),
                            };
                            if let Err(why) = r {
                                v.notice =
                                    Some((Tone::Bad, format!("No se pudo cambiar Abrir al iniciar sesión. {why}")));
                            }
                            v.reload();
                            cx.notify();
                        });
                    }),
                ))));
        }

        let poll = if self.settings.poll_interval_explicit {
            PollChoice::Every(self.settings.poll_interval)
        } else {
            PollChoice::Auto
        };
        let mut options = vec![
            (PollChoice::Auto, "Automático"),
            (PollChoice::Every(120), "Trabajando: 2 min"),
            (PollChoice::Every(300), "Trabajando: 5 min"),
        ];
        let custom;
        if let PollChoice::Every(n) = poll {
            if n != 120 && n != 300 {
                custom = format!("Trabajando: {n} s");
                options.push((poll, custom.as_str()));
            }
        }
        let me = cx.entity().downgrade();
        pane = pane.child(field_stacked(
            t,
            "Consultas de uso",
            "En automático, consulta cada 3 minutos mientras trabajas y con menos frecuencia en reposo. Las esperas \
             entre reintentos se aplican a todas las consultas.",
            segmented(t, "poll", &options, &poll, true, move |c, _, cx| {
                let _ = me.update(cx, |v, cx| {
                    let paths = v.paths.clone();
                    v.save(|s, cfg| match c {
                        PollChoice::Auto => {
                            cfg.remove("poll_interval");
                            let fresh = Settings::from_config(cfg, &paths, None);
                            s.poll_interval = fresh.poll_interval;
                            s.poll_interval_explicit = false;
                        }
                        PollChoice::Every(n) => {
                            s.poll_interval = n;
                            cfg.set("poll_interval", &n.to_string());
                        }
                    });
                    cx.notify();
                });
            }),
        ));
        // A missing CLI stops everything, so it isn't tucked under Advanced.
        if self.snap.as_ref().is_some_and(|s| s.claude.is_none()) {
            let cfgp = self.paths.config_file.clone();
            let label = if cfgp.exists() { "Abrir configuración" } else { "Crear configuración" };
            pane = pane.child(
                div().pt(px(12.)).child(
                    notice(
                        t,
                        Tone::Plain,
                        "No se encontró Claude Code, así que el uso se actualiza cada 12 minutos. Instala Claude Code o la app \
                         de escritorio de Claude para actualizar mientras trabajas, o define claude_path en la configuración.",
                    )
                    .child(div().flex().pl(px(18.)).child(button(
                        t,
                        "fix-cli",
                        label,
                        ButtonKind::Small,
                        true,
                        move |_, _, cx| open_config(&cfgp, cx),
                    ))),
                ),
            );
        }
        let p = self.paths.clone();
        pane = pane.child(field(
            t,
            "Salir de Clawdmeter",
            "Detiene las actualizaciones de uso y cierra Clawdmeter. El dispositivo conserva la última lectura.",
            button(t, "quit", "Salir", ButtonKind::Small, self.snap.is_some(), move |_, window, _| {
                command(&p, AppCommand::Quit);
                window.remove_window();
            }),
        ));
        if let Some(legacy) = self.legacy.clone() {
            let me = cx.entity().downgrade();
            let home = self.paths.home.clone();
            pane = pane.child(field(
                t,
                "Servicio antiguo detectado",
                format!(
                    "Desactiva {} para que solo un proceso de Clawdmeter se conecte al dispositivo.",
                    tilde(&legacy.describe(), &self.paths.home)
                ),
                button(t, "legacy", "Desactivar servicio antiguo", ButtonKind::Primary, true, move |_, _, cx| {
                    autostart::disable_legacy(&home);
                    let _ = me.update(cx, |v, cx| {
                        v.reload();
                        if let Some(still) = &v.legacy {
                            v.notice = Some((
                                Tone::Bad,
                                format!(
                                    "No se pudo desactivar {}. Quítalo a mano.",
                                    tilde(&still.describe(), &v.paths.home)
                                ),
                            ));
                        }
                        cx.notify();
                    });
                }),
            ));
        }
        // Rarely needed: where things live and which CLI runs.
        let open = self.advanced;
        let text = t.text;
        pane = pane.child(
            div()
                .id("advanced")
                .group("advanced")
                .debug_selector(|| "advanced".into())
                .flex()
                .items_center()
                .gap(px(6.))
                .pt(px(14.))
                .pb(px(4.))
                .text_size(px(14.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.text_color(t.text))
                .on_click(cx.listener(|v, _, _, cx| {
                    v.advanced = !v.advanced;
                    cx.notify();
                }))
                .child(
                    icon(if open { "chevron-down" } else { "chevron-right" }, 16., t.dim)
                        .group_hover("advanced", move |s| s.text_color(text)),
                )
                .child("Avanzado"),
        );
        if open {
            let mut advanced = div().flex().flex_col();
            let claude = self.snap.as_ref().and_then(|s| s.claude.clone());
            advanced = advanced.child(field(
                t,
                "Ejecutable de Claude Code",
                match &claude {
                    Some(c) => format!("El uso se lee con {}.", crate::claude_label(Path::new(c), &self.paths.home)),
                    None if self.snap.is_some() => "No encontrado.".into(),
                    None => "Abre Clawdmeter para detectar Claude Code.".into(),
                },
                div(),
            ));
            let log = self.paths.state_dir.join("daemon.log");
            advanced = advanced.child(field(
                t,
                "Archivo de registro",
                tilde(&log.display().to_string(), &self.paths.home),
                button(t, "open-log", "Abrir registro", ButtonKind::Small, log.exists(), move |_, _, cx| {
                    cx.open_with_system(&log)
                }),
            ));
            let cfgp = self.paths.config_file.clone();
            advanced = advanced.child(field(
                t,
                "Archivo de configuración",
                tilde(&cfgp.display().to_string(), &self.paths.home),
                button(
                    t,
                    "open-cfg",
                    if cfgp.exists() { "Abrir configuración" } else { "Crear configuración" },
                    ButtonKind::Small,
                    true,
                    move |_, _, cx| open_config(&cfgp, cx),
                ),
            ));
            pane = pane.child(advanced);
        }
        pane
    }
}

/// What a new config file starts with: the keys people reach for, commented.
const CONFIG_TEMPLATE: &str = "\
# Ajustes de Clawdmeter: clave = valor. La app vuelve a leer este archivo en unos segundos.
# claude_path = /path/to/claude
# config_dirs = ~/.claude, ~/.claude-work
# heartbeat_interval = 60
";

/// Open the config file in the system editor, creating it first if needed.
fn open_config(path: &std::path::Path, cx: &mut gpui::App) {
    if !path.exists() {
        let made =
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| std::fs::write(path, CONFIG_TEMPLATE));
        if let Err(e) = made {
            log::warn!("couldn't create {}: {e}", path.display());
            return;
        }
    }
    cx.open_with_system(path);
}

fn with_alpha(c: gpui::Rgba, a: f32) -> gpui::Hsla {
    let mut h: gpui::Hsla = c.into();
    h.a = a;
    h
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        let mut pane = match self.page {
            Page::Accounts => self.accounts_pane(&t, cx),
            Page::Hooks => self.hooks_pane(&t, cx),
            Page::Settings => self.settings_pane(&t, cx),
            _ => self.device_pane(&t, window, cx),
        };
        if let Some(e) = self.config_error.clone().or_else(|| self.notice.as_ref().map(|(_, m)| m.clone())) {
            pane = pane.child(div().pt(px(12.)).text_size(px(13.)).text_color(t.bad).child(e));
        }
        pane
    }
}

/// Re-read the config file, apply `change`, and write only the keys whose
/// value differs.
pub(crate) fn write_settings(
    paths: &Paths,
    change: impl FnOnce(&mut Settings, &mut ConfigFile),
) -> std::io::Result<()> {
    let path = &paths.config_file;
    let mut cfg = ConfigFile::load(path)?;
    let mut s = Settings::from_config(&cfg, paths, None);
    change(&mut s, &mut cfg);
    s.apply_to(&mut cfg, paths);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cfg.save(path)
}

/// What Clawd does at a liveliness level, under the slider.
fn liveliness_info(level: u8) -> &'static str {
    match level {
        1 => "Quieto en su rincón, un verbo nuevo cada minuto. Sigue reaccionando a toques y alertas.",
        2 => "Casi quieto: menos numeritos en el rincón y espectáculos solo tras rachas largas de trabajo.",
        3 => "Numeritos de vez en cuando, un espectáculo tras una racha de trabajo y salvapantallas si no estás.",
        4 => "Más animado: más numeritos, y los espectáculos llegan antes y más a menudo.",
        _ => "Espectáculos y reacciones frecuentes. Hasta una racha moderada se gana un espectáculo.",
    }
}

fn attention_anim_label(a: AttentionAnim) -> &'static str {
    match a {
        AttentionAnim::Min2 => "2m",
        AttentionAnim::S30 => "30s",
        AttentionAnim::S10 => "10s",
        AttentionAnim::Off => "No",
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;

    fn home(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!("cm-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::under(&root);
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        std::fs::create_dir_all(root.join("acct")).unwrap();
        std::fs::write(&paths.config_file, format!("# mine\nconfig_dirs = {}\n", root.join("acct").display())).unwrap();
        paths
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.run_until_parked();
        let b = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} isn't on screen"));
        cx.simulate_click(b.center(), Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn toggles_and_choices_write_the_config(cx: &mut TestAppContext) {
        let paths = home("cfg");
        let p = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Device, false, cx));
        click(cx, "reset-chime");
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.starts_with("# mine\n"), "comments kept: {text}");
        assert!(text.contains("reset_chime = on"), "{text}");
        assert!(cx.debug_bounds("liveliness-info-3").is_some(), "the default level's line");
        click(cx, "liveliness-1");
        assert!(std::fs::read_to_string(&paths.config_file).unwrap().contains("liveliness = 1"));
        assert!(cx.debug_bounds("liveliness-info-1").is_some(), "the line follows the slider");
        click(cx, "liveliness-3"); // Back to the default: the key stays, the value follows.
        assert!(std::fs::read_to_string(&paths.config_file).unwrap().contains("liveliness = 3"));
        click(cx, "attention-anim-0"); // Off
        assert!(std::fs::read_to_string(&paths.config_file).unwrap().contains("attention_anim = off"));
        click(cx, "attention-anim-2"); // 30 s
        assert!(std::fs::read_to_string(&paths.config_file).unwrap().contains("attention_anim = 30s"));
        click(cx, "pace-marker");
        assert!(std::fs::read_to_string(&paths.config_file).unwrap().contains("pace_marker = on"));

        view.update(cx, |v, cx| v.show(Page::Settings, false, cx));
        click(cx, "poll-2"); // While working: 5 min
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("poll_interval = 300"), "{text}");
        click(cx, "poll-0"); // Automatic removes the key again.
        assert!(!std::fs::read_to_string(&paths.config_file).unwrap().contains("poll_interval"));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[gpui::test]
    fn hooks_install_after_preview(cx: &mut TestAppContext) {
        let paths = home("hooks");
        let settings = paths.home.join("acct/settings.json");
        let before = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"/old/attention-hook.sh event"}]}]}}"#;
        std::fs::write(&settings, before).unwrap();
        let p = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Hooks, false, cx));
        view.read_with(cx, |v, _| assert_eq!(v.hooks[0].status, HookStatus::Legacy));

        click(cx, "install-all");
        // Nothing is written until the preview is confirmed.
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
        view.read_with(cx, |v, _| assert!(v.preview.is_some()));
        click(cx, "apply-hooks");

        let after = std::fs::read_to_string(&settings).unwrap();
        assert!(!after.contains("attention-hook.sh"), "{after}");
        assert!(after.contains(" hook clear event"), "{after}");
        assert_eq!(std::fs::read_to_string(settings.with_extension("json.bak")).unwrap(), before);
        view.read_with(cx, |v, _| {
            assert_eq!(v.hooks[0].status, HookStatus::Installed);
            assert!(v.preview.is_none());
        });
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    fn stale_login_entry_is_offered_a_fix(cx: &mut TestAppContext) {
        let paths = home("autostart");
        let gone = Path::new("/tmp/.mount_Clawdgone/usr/bin/clawdmeter");
        autostart::enable(&paths.home, gone, LaunchMode::Tray).unwrap();
        let p = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Settings, false, cx));
        view.read_with(cx, |v, _| assert_eq!(v.autostart_stale, Some(Some(gone.to_path_buf()))));
        click(cx, "fix-autostart");
        let want = view.read_with(cx, |v, _| {
            assert_eq!(v.autostart_stale, None);
            v.launch_exe.clone().unwrap()
        });
        assert_eq!(autostart::entry_exe(&paths.home, LaunchMode::Tray), Some(want));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[gpui::test]
    fn test_alert_reports_on_its_button(cx: &mut TestAppContext) {
        let paths = home("test-alert");
        let p = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Hooks, false, cx));
        click(cx, "test-alert");
        assert!(paths.attention_file().exists());
        // No app-status.json: the app isn't running to pick it up.
        view.read_with(cx, |v, _| {
            assert_eq!(v.test_alert, Some(Err("Clawdmeter no se está ejecutando.".into())));
            assert!(v.hook_msg.is_none(), "no banner");
        });
        cx.executor().advance_clock(TEST_ALERT_FAILED);
        cx.run_until_parked();
        view.read_with(cx, |v, _| assert_eq!(v.test_alert, None));
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[gpui::test]
    fn alert_kinds_save_to_the_config(cx: &mut TestAppContext) {
        let paths = home("alerts");
        let p = paths.clone();
        let (_view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Hooks, false, cx));
        click(cx, "alert-idle");
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("alert_on = permission, question, idle, elicitation"), "{text}");
        click(cx, "alert-permission");
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(text.contains("alert_on = question, idle, elicitation"), "{text}");
        let _ = std::fs::remove_dir_all(&paths.home);
    }

    #[gpui::test]
    fn unreadable_accounts_are_counted_apart(cx: &mut TestAppContext) {
        let paths = home("unreadable");
        let (a, b) = (paths.home.join("acct"), paths.home.join("broken"));
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("settings.json"), "{ not json").unwrap();
        std::fs::write(&paths.config_file, format!("config_dirs = {}, {}\n", a.display(), b.display())).unwrap();
        let p = paths.clone();
        let (view, cx) = cx.add_window_view(move |_, cx| SettingsView::with_usb(p, Page::Hooks, false, cx));
        click(cx, "install-all");
        click(cx, "apply-hooks");
        view.read_with(cx, |v, _| {
            assert_eq!((v.installed_hooks(), v.unreadable_hooks(), v.hook_accounts()), (1, 1, 2));
            assert_eq!(v.pending_hooks(), 0, "the broken file can't be fixed by installing");
        });
        let _ = std::fs::remove_dir_all(&paths.home);
    }
}
