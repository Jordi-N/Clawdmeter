//! The first-run setup wizard: Welcome → Your board → Flash → Pair → Hooks
//! → Done. It fills the window, every step can be skipped, and finishing or
//! skipping it records `onboarding_done` ([`crate::onboarding`]). The board
//! and flash steps are the flasher; the hooks step is the Hooks page.

use std::path::PathBuf;

use clawdmeter_core::{usage, Paths, Settings};
use gpui::{div, prelude::*, px, Context, Entity, EventEmitter, SharedString, Window};

use super::flasher::{FlashEvent, FlashMode, FlasherView, Step as FlashStep};
use super::settings::SettingsView;
use super::theme::{Theme, FONT_DISPLAY, R_PILL};
use super::widgets::{button, card, clawd, dot, notice, para, pill, ButtonKind, Tone};
use super::{live_status, REFRESH};
use crate::Page;

/// The wizard's steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// What Clawdmeter is.
    Welcome,
    /// The flasher: plug in and identify, then flash.
    Device,
    /// Bluetooth pairing.
    Pair,
    /// Claude Code hooks.
    Hooks,
    /// Summary.
    Done,
}

const LABELS: [&str; 6] = ["Bienvenida", "Tu placa", "Grabar", "Emparejar", "Hooks", "Listo"];

/// What happened to the board in this run of the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardOutcome {
    Flashed,
    UpToDate,
    Skipped,
}

/// The wizard tells the window it's done (finished or skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupEvent {
    /// Show the app's pages.
    Finished,
}

/// The setup wizard.
pub struct SetupView {
    paths: Paths,
    step: Step,
    flasher: Entity<FlasherView>,
    settings: Entity<SettingsView>,
    board: Option<BoardOutcome>,
    connected: bool,
    running: bool,
    /// No tray: the window is the app.
    windowed: bool,
    /// The Claude Code that will read usage (`None`: not found).
    claude: Option<PathBuf>,
}

impl EventEmitter<SetupEvent> for SetupView {}

impl SetupView {
    /// Build around the window's flasher and settings views.
    pub fn new(
        paths: Paths,
        flasher: Entity<FlasherView>,
        settings: Entity<SettingsView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(&flasher, |v, _, e: &FlashEvent, cx| {
            if v.step == Step::Device {
                v.board = Some(match e {
                    FlashEvent::Flashed => BoardOutcome::Flashed,
                    FlashEvent::UpToDate => BoardOutcome::UpToDate,
                });
                v.goto(Step::Pair, cx);
            }
        })
        .detach();
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            if this.update(cx, |v, cx| v.tick(cx)).is_err() {
                break;
            }
        })
        .detach();
        let claude_path = Settings::load(&paths).ok().and_then(|s| s.claude_path);
        let claude = usage::find_claude(claude_path.as_deref(), &paths.home);
        SetupView {
            claude,
            paths,
            step: Step::Welcome,
            flasher,
            settings,
            board: None,
            connected: false,
            running: false,
            windowed: false,
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let s = live_status(&self.paths);
        let windowed = s.as_ref().is_some_and(|s| s.windowed);
        let (running, connected) = (s.is_some(), s.is_some_and(|s| s.link == "connected"));
        if (running, connected, windowed) != (self.running, self.connected, self.windowed) {
            self.running = running;
            self.connected = connected;
            self.windowed = windowed;
            cx.notify();
        }
    }

    /// Start from the welcome step.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.board = None;
        self.flasher.update(cx, |f, cx| {
            f.restart();
            f.set_mode(FlashMode::Setup, cx);
        });
        self.goto(Step::Welcome, cx);
    }

    /// The window is leaving the wizard for a page: put the borrowed views
    /// back and count setup as done.
    pub fn leave(&mut self, cx: &mut Context<Self>) {
        self.flasher.update(cx, |f, cx| {
            f.set_mode(FlashMode::Page, cx);
            f.set_active(false, cx);
        });
        if let Err(e) = crate::onboarding::mark_done(&self.paths) {
            log::warn!("couldn't record that setup is done: {e}");
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.leave(cx);
        cx.emit(SetupEvent::Finished);
    }

    /// Show a step.
    pub fn goto(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.flasher.update(cx, |f, cx| f.set_active(step == Step::Device, cx));
        // The Hooks page shows inside the hooks step; otherwise the settings
        // view stays hidden (and off USB).
        let page = if step == Step::Hooks { Page::Hooks } else { Page::Setup };
        self.settings.update(cx, |s, cx| s.show(page, true, cx));
        self.tick(cx);
        cx.notify();
    }

    fn index(&self, cx: &Context<Self>) -> usize {
        match self.step {
            Step::Welcome => 0,
            Step::Device => match self.flasher.read(cx).step() {
                FlashStep::Connect | FlashStep::Identify => 1,
                _ => 2,
            },
            Step::Pair => 3,
            Step::Hooks => 4,
            Step::Done => 5,
        }
    }

    /// Whether a step behind the current one actually got done (a skipped
    /// one shows neutral, not green).
    fn completed(&self, i: usize, cx: &Context<Self>) -> bool {
        match i {
            1 => match self.board {
                Some(b) => b != BoardOutcome::Skipped,
                None => self.flasher.read(cx).picked().is_some(),
            },
            2 => matches!(self.board, Some(BoardOutcome::Flashed | BoardOutcome::UpToDate)),
            3 => self.connected,
            4 => {
                let s = self.settings.read(cx);
                s.installed_hooks() > 0 && s.installed_hooks() == s.hook_accounts()
            }
            _ => true,
        }
    }

    /// The board this run is about: picked in the flasher, else the one
    /// last flashed or probed.
    fn board_info(&self, cx: &Context<Self>) -> Option<&'static crate::boards::BoardInfo> {
        let picked = self.flasher.read(cx).picked().map(str::to_owned);
        let env = picked.or_else(|| crate::ipc::load_firmware_record(&self.paths).map(|r| r.env))?;
        crate::boards::board(&env)
    }

    fn progress(&self, t: &Theme, cx: &Context<Self>) -> impl IntoElement {
        let at = self.index(cx);
        let mut row = div().flex().items_center().justify_center().gap(px(4.)).flex_wrap();
        for (i, label) in LABELS.iter().enumerate() {
            let (done, on) = (i < at && self.completed(i, cx), i == at);
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(4.))
                    .rounded(px(R_PILL))
                    .when(on, |d| d.bg(t.panel))
                    .text_size(px(12.5))
                    .text_color(if on || i < at { t.text } else { t.mute })
                    .child(div().size(px(7.)).rounded(px(R_PILL)).bg(if on {
                        t.accent
                    } else if done {
                        t.ok
                    } else {
                        t.track
                    }))
                    .child(*label),
            );
        }
        row
    }

    fn title(text: &'static str) -> impl IntoElement {
        div().font_family(FONT_DISPLAY).text_size(px(26.)).text_center().child(text)
    }

    fn welcome(&self, t: &Theme) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .pt(px(24.))
            .child(clawd(7.))
            .child(Self::title("Configura Clawdmeter"))
            .child(div().max_w(px(460.)).text_center().child(para(
                t,
                "Ve tu uso de Claude Code en el escritorio. Clawd reacciona a la actividad y te avisa cuando una \
                 sesión necesita respuesta.",
            )))
            .child(div().max_w(px(460.)).text_center().child(para(
                t,
                "Conecta tu placa con un cable USB de datos. La configuración instala el firmware, empareja el Bluetooth \
                 y, si quieres, añade los hooks de Claude Code. Necesitas Claude Code, o la app de escritorio de \
                 Claude, instalado y con la sesión iniciada.",
            )))
            .child(match &self.claude {
                Some(c) => div()
                    .max_w(px(460.))
                    .text_center()
                    .text_size(px(12.5))
                    .text_color(t.mute)
                    .child(format!("El uso se leerá con {}.", crate::claude_label(c, &self.paths.home)))
                    .into_any_element(),
                None => notice(
                    t,
                    Tone::Plain,
                    "No se encontró Claude Code, así que el uso se actualizará cada 12 minutos. Instala Claude Code o la \
                     app de escritorio de Claude para actualizar mientras trabajas.",
                )
                .max_w(px(460.))
                .into_any_element(),
            })
    }

    fn pair(&self, t: &Theme, cx: &Context<Self>) -> gpui::Div {
        let how = if cfg!(target_os = "macos") {
            "Abre Ajustes del Sistema → Bluetooth y pulsa Conectar junto a Clawdmeter."
        } else if cfg!(windows) {
            "Abre Configuración → Bluetooth y dispositivos → Agregar dispositivo → Bluetooth y elige Clawdmeter."
        } else {
            "Abre los ajustes de Bluetooth de tu escritorio y empareja Clawdmeter. Desde una terminal: bluetoothctl, \
             luego scan on, pair <dirección> y trust <dirección>."
        };
        let status = match (self.running, self.connected) {
            (_, true) => pill(t, "Conectado", Tone::Ok).into_any_element(),
            (true, false) => pill(t, "Esperando conexión Bluetooth", Tone::Accent).into_any_element(),
            (false, false) => notice(
                t,
                Tone::Plain,
                "El proceso en segundo plano no se está ejecutando. Empareja el dispositivo y abre Clawdmeter para \
                 comprobar la conexión.",
            )
            .w(px(520.))
            .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .pt(px(12.))
            .child(Self::title("Empareja por Bluetooth"))
            .child(div().max_w(px(500.)).text_center().child(para(
                t,
                "Empareja Clawdmeter en los ajustes de Bluetooth del ordenador. Aparece como un teclado para que sus \
                 botones puedan enviar pulsaciones.",
            )))
            .child(
                card(t).w(px(520.)).child(div().text_size(px(14.)).line_height(px(21.)).child(how)).child(
                    div()
                        .text_size(px(12.5))
                        .text_color(t.mute)
                        .line_height(px(18.))
                        .child(unpair_hint(self.board_info(cx))),
                ),
            )
            .child(status)
    }

    fn hooks(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let (installed, total) = {
            let s = self.settings.read(cx);
            (s.installed_hooks(), s.hook_accounts())
        };
        let mut d = div().flex().flex_col().gap(px(16.)).pt(px(12.)).child(Self::title("Añade los hooks de Claude Code")).child(
            div().flex().justify_center().child(div().max_w(px(520.)).text_center().child(para(
                t,
                "Los hooks permiten que Clawd reaccione a la actividad y muestre alertas. Guardan en local los metadatos \
                     de los eventos y, para las alertas, el texto de la notificación o pregunta.",
            ))),
        );
        // Only every account installed skips the list; an unreadable one
        // stays in view with its error.
        if installed > 0 && installed == total {
            let accounts = if installed == 1 { "tu cuenta".to_string() } else { format!("las {installed} cuentas") };
            d = d.child(div().flex().justify_center().child(pill(
                t,
                SharedString::from(format!("Hooks instalados en {accounts}")),
                Tone::Ok,
            )));
        } else {
            d = d.child(self.settings.clone());
        }
        d
    }

    fn done(&self, t: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let board = match self.board {
            Some(BoardOutcome::Flashed) => ("Último firmware instalado", Tone::Ok),
            Some(BoardOutcome::UpToDate) => ("Firmware al día", Tone::Ok),
            Some(BoardOutcome::Skipped) | None => {
                ("No se instaló firmware en esta configuración. Usa Grabar para instalarlo.", Tone::Plain)
            }
        };
        let link = if self.connected {
            ("Bluetooth conectado", Tone::Ok)
        } else {
            ("Sin conexión. Empareja en los ajustes de Bluetooth.", Tone::Plain)
        };
        let (installed, total, unreadable) = {
            let s = self.settings.read(cx);
            (s.installed_hooks(), s.hook_accounts(), s.unreadable_hooks())
        };
        let hooks: (SharedString, Tone) = {
            let unread = match unreadable {
                0 => String::new(),
                1 => " No se pudo leer 1 cuenta.".into(),
                n => format!(" No se pudieron leer {n} cuentas."),
            };
            match installed {
                0 if unreadable == 0 => ("Hooks sin instalar. Añádelos desde Hooks.".into(), Tone::Plain),
                n => {
                    let tone = if n == total { Tone::Ok } else { Tone::Plain };
                    (format!("Hooks instalados en {n} de {total} cuentas.{unread}").into(), tone)
                }
            }
        };
        let row = |(text, tone): (SharedString, Tone)| {
            div().flex().items_center().gap(px(10.)).child(dot(t, tone)).child(div().text_size(px(14.)).child(text))
        };
        let (board, link) = ((SharedString::from(board.0), board.1), (SharedString::from(link.0), link.1));
        let after = match (self.running, self.windowed) {
            (false, _) => None,
            (true, true) => Some("Deja esta ventana abierta para actualizar el uso."),
            (true, false) => Some("Clawdmeter sigue en la bandeja. Ábrelo desde ahí para ver el uso o los ajustes."),
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(16.))
            .pt(px(24.))
            .child(clawd(6.))
            .child(Self::title("Resumen de la configuración"))
            .child(card(t).min_w(px(380.)).gap(px(12.)).child(row(board)).child(row(link)).child(row(hooks)))
            .when_some(after, |d, a| d.child(div().max_w(px(460.)).text_center().child(para(t, a))))
    }
}

/// How to clear a pairing to another computer, for the board at hand.
fn unpair_hint(board: Option<&crate::boards::BoardInfo>) -> &'static str {
    match board {
        Some(b) if b.pwr_button => {
            "¿Ya estaba emparejada con otro ordenador? Mantén PWR 3 segundos y suelta para borrar el emparejamiento anterior."
        }
        Some(_) => {
            "¿Ya estaba emparejada con otro ordenador? Mientras la pantalla muestra el aviso de emparejamiento, mantén \
             el dedo encima 5 segundos para borrar el emparejamiento anterior."
        }
        None => {
            "¿Ya estaba emparejada con otro ordenador? Mantén PWR 3 segundos y suelta para borrar el emparejamiento \
             anterior. En placas sin botón PWR, mantén pulsada la pantalla 5 segundos mientras muestra el aviso de \
             emparejamiento."
        }
    }
}

impl Render for SetupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::for_appearance(window.appearance());
        let body = match self.step {
            Step::Welcome => self.welcome(&t),
            Step::Device => div().child(self.flasher.clone()),
            Step::Pair => self.pair(&t, cx),
            Step::Hooks => self.hooks(&t, cx),
            Step::Done => self.done(&t, cx),
        };
        let back = match self.step {
            Step::Welcome => None,
            Step::Device => Some(Step::Welcome),
            Step::Pair => Some(Step::Device),
            Step::Hooks => Some(Step::Pair),
            Step::Done => Some(Step::Hooks),
        };
        // Nothing walks away from a board mid-write.
        let busy = self.step == Step::Device && self.flasher.read(cx).flashing();
        let mut right = div().flex().items_center().gap(px(8.));
        if let Some(b) = back {
            right = right.child(button(
                &t,
                "setup-back",
                "Atrás",
                ButtonKind::Small,
                !busy,
                cx.listener(move |v, _, _, cx| {
                    // On the board steps, Back walks the flasher back first.
                    if v.step == Step::Device
                        && v.flasher.update(cx, |f, cx| {
                            let moved = f.back();
                            cx.notify();
                            moved
                        })
                    {
                        cx.notify();
                        return;
                    }
                    if b == Step::Device {
                        v.flasher.update(cx, |f, _| f.restart());
                    }
                    v.goto(b, cx);
                }),
            ));
        }
        right = match self.step {
            Step::Welcome => right.child(button(
                &t,
                "setup-next",
                "Empezar",
                ButtonKind::Primary,
                true,
                cx.listener(|v, _, _, cx| v.goto(Step::Device, cx)),
            )),
            // The flasher has the step's primary action.
            Step::Device => right.child(button(
                &t,
                "setup-next",
                "Saltar este paso",
                ButtonKind::Normal,
                !busy,
                cx.listener(|v, _, _, cx| {
                    v.board = Some(BoardOutcome::Skipped);
                    v.goto(Step::Pair, cx);
                }),
            )),
            Step::Pair | Step::Hooks => {
                let next = if self.step == Step::Pair { Step::Hooks } else { Step::Done };
                // While hooks wait to be installed, installing is the step's
                // primary action and moving on is the quiet one.
                let pending = self.step == Step::Hooks && self.settings.read(cx).pending_hooks() > 0;
                let unpaired = self.step == Step::Pair && !self.connected;
                let (label, kind) = if pending {
                    ("Saltar hooks", ButtonKind::Normal)
                } else if unpaired {
                    ("Continuar sin emparejar", ButtonKind::Normal)
                } else {
                    ("Continuar", ButtonKind::Primary)
                };
                right.child(button(
                    &t,
                    "setup-next",
                    label,
                    kind,
                    true,
                    cx.listener(move |v, _, _, cx| v.goto(next, cx)),
                ))
            }
            Step::Done => right.child(button(
                &t,
                "setup-finish",
                "Abrir Clawdmeter",
                ButtonKind::Primary,
                true,
                cx.listener(|v, _, _, cx| v.finish(cx)),
            )),
        };
        let footer = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(px(64.))
            .px(px(24.))
            .border_t_1()
            .border_color(t.line)
            .bg(t.bg)
            .child(
                div()
                    .id("skip-setup")
                    .debug_selector(|| "skip-setup".into())
                    .text_size(px(13.))
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.text))
                    .when(self.step != Step::Done && !busy, |d| {
                        d.child("Saltar configuración").on_click(cx.listener(|v, _, _, cx| v.finish(cx)))
                    }),
            )
            .child(right);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_shrink_0().pt(px(18.)).pb(px(6.)).child(self.progress(&t, cx)))
            .child(div().id("setup-body").flex_1().min_h_0().overflow_y_scroll().child(
                div().flex().justify_center().child(div().w_full().max_w(px(680.)).px(px(32.)).py(px(20.)).child(body)),
            ))
            .child(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpair_advice_follows_the_board() {
        let lcd4 = crate::boards::board("waveshare_lcd_4");
        let amoled = crate::boards::board("waveshare_amoled_216");
        assert!(unpair_hint(lcd4).contains("el dedo encima"));
        assert!(!unpair_hint(lcd4).contains("PWR"), "the LCD-4's PWR key is a reset");
        assert!(unpair_hint(amoled).contains("Mantén PWR 3 segundos"));
        assert!(unpair_hint(None).contains("sin botón PWR"));
    }
}
