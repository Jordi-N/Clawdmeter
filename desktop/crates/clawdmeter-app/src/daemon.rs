//! The daemon runtime: drives [`clawdmeter_core::Engine`] with real I/O
//! (the `claude` CLI, events.log, attention files, the device link over BLE
//! or WiFi).
//!
//! It runs on its own thread with a current-thread tokio runtime, so the GUI
//! event loop never shares a reactor with it. The UI talks to it through a
//! [`DaemonHandle`]: commands in, a [`Status`] snapshot out.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use clawdmeter_ble::{
    pinned_address, should_wake, AddressCache, Bluetooth, DeviceEvent, DeviceEvents, DeviceLink, LinkError,
    LinkOptions, Reconnect, Sighting, MAX_WRITE,
};
use clawdmeter_core::agents;
use clawdmeter_core::attention::AttentionInbox;
use clawdmeter_core::backoff;
use clawdmeter_core::engine::PollPlan;
use clawdmeter_core::events::EventTail;
use clawdmeter_core::probe;
use clawdmeter_core::usage;
use clawdmeter_core::wifi::{self, Connection, Secret, Transport, WifiStatus};
use clawdmeter_core::{unix_now, ConfigFile, Engine, EngineStatus, HostClock, Paths, Settings, WriteKind};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::transport::{self, Link, TcpLink};

/// Loop tick: events reach the device within about this long.
const TICK: Duration = Duration::from_millis(500);
/// Config re-read interval (s).
const SETTINGS_EVERY: i64 = 5;
/// Connection check interval (s).
const LINK_CHECK_EVERY: i64 = 5;
/// How long the device has to acknowledge a provisioning line (s). The
/// official firmware never does.
const PROVISION_ACK_SECS: i64 = 10;

/// Requests from the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Poll `/usage` now.
    RefreshNow,
    /// Drop the link and look for the device again (e.g. after pairing).
    Reconnect,
    /// Forget the cached device address, then look again (after swapping boards).
    Forget,
    /// Mute the alert chimes (device and this computer) for this run,
    /// whatever the config says.
    SetChimesMuted(bool),
    /// Send a WiFi network to the device over BLE (fork firmware).
    ProvisionWifi {
        /// Network name.
        ssid: String,
        /// Its password.
        pass: Secret,
    },
    /// Stop the daemon thread.
    Shutdown,
}

/// The device link, for the tray's status dot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LinkState {
    /// Not tried yet.
    #[default]
    Starting,
    /// No adapter, or Bluetooth is off.
    NoAdapter(String),
    /// No paired device found; retrying.
    Searching,
    /// Connecting to a known device.
    Connecting,
    /// Connected.
    Connected {
        /// Cache key (address).
        key: String,
        /// Device name.
        name: Option<String>,
        /// Over which link.
        via: Transport,
    },
}

/// The WiFi side, for the Device page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiInfo {
    /// The device's address while the link is WiFi.
    pub addr: Option<String>,
    /// What the device last reported about its WiFi.
    pub device: Option<WifiStatus>,
    /// WiFi is the choice but the link is Bluetooth (WiFi not reachable).
    pub fallback: bool,
    /// The last "send to the device": `Some(true)` acknowledged,
    /// `Some(false)` failed, `None` waiting; and what to tell the user.
    pub send: Option<(Option<bool>, String)>,
}

/// Everything the tray needs, updated every tick.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Device link.
    pub link: LinkState,
    /// Usage / hooks / attention.
    pub engine: EngineStatus,
    /// Unix seconds of the last successful device write.
    pub last_write_at: Option<i64>,
    /// `claude` CLI path in use (None = not found).
    pub claude: Option<PathBuf>,
    /// WiFi link details.
    pub wifi: WifiInfo,
}

/// Handle held by the UI.
#[derive(Debug)]
pub struct DaemonHandle {
    /// Send commands.
    pub commands: mpsc::UnboundedSender<Command>,
    /// Watch status.
    pub status: watch::Receiver<Status>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DaemonHandle {
    /// Ask the daemon to stop and wait for it.
    pub fn shutdown(mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start the daemon on a background thread.
pub fn spawn(paths: Paths) -> DaemonHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (st_tx, st_rx) = watch::channel(Status::default());
    let thread = std::thread::Builder::new()
        .name("clawdmeter-daemon".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(run(paths, cmd_rx, st_tx));
        })
        .expect("spawn daemon thread");
    DaemonHandle { commands: cmd_tx, status: st_rx, thread: Some(thread) }
}

type PollResults = Vec<(PathBuf, probe::Polled)>;

/// A connect attempt's outcome, per transport.
enum Attempt {
    Ble(Result<(DeviceLink, DeviceEvents), LinkError>),
    Wifi(std::io::Result<(TcpLink, DeviceEvents)>),
}

/// Read each account of a poll plan: the probe, `/usage`, or a reading known
/// without a call. The probe runs from `cwd` (the state dir).
fn poll_all(claude: Option<PathBuf>, plan: PollPlan, home: PathBuf, cwd: PathBuf) -> PollResults {
    let now = chrono::Utc::now();
    plan.accounts
        .into_iter()
        .zip(plan.plans)
        .map(|(d, p)| {
            let r = probe::poll(claude.as_deref(), &d, &home, &cwd, &p, now);
            (d, r)
        })
        .collect()
}

/// Edit the config file in place (keys the daemon owns: the WiFi key, the
/// network name, the connection choice).
fn edit_config(paths: &Paths, edit: impl FnOnce(&mut ConfigFile)) -> std::io::Result<()> {
    let mut cfg = ConfigFile::load(&paths.config_file)?;
    edit(&mut cfg);
    cfg.save(&paths.config_file)
}

/// Send the network to the device over a BLE link: makes (and saves) the
/// TCP key first if there is none, so a key the device stored is never lost.
async fn provision(paths: &Paths, link: &Link, s: &Settings, ssid: &str, pass: &Secret) -> Result<(), String> {
    let key = match &s.wifi_key {
        Some(k) => k.clone(),
        None => {
            let k = transport::new_key().map_err(|e| format!("No se pudo generar la clave: {e}"))?;
            edit_config(paths, |c| c.set("wifi_key", &k.0))
                .map_err(|e| format!("No se pudo guardar la clave: {}", crate::errors::io_message(&e)))?;
            k
        }
    };
    // `#` would start a comment in the config file; the name only prefills the form.
    if !ssid.contains(['#', '\n', '\r']) {
        let _ = edit_config(paths, |c| c.set("wifi_ssid", ssid));
    }
    link.write(wifi::provision_line(ssid, pass, &key).as_bytes())
        .await
        .map_err(|e| format!("No se pudo enviar por Bluetooth: {e}"))
}

/// The daemon loop. Returns on [`Command::Shutdown`] or when every command
/// sender is dropped.
pub async fn run(paths: Paths, mut commands: mpsc::UnboundedReceiver<Command>, status: watch::Sender<Status>) {
    let settings = Settings::load(&paths).unwrap_or_else(|e| {
        log::warn!("config unreadable ({e}); using defaults");
        Settings::from_config(&Default::default(), &paths, None)
    });
    log::info!(
        "accounts: {}",
        settings.config_dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
    );
    let mut engine = Engine::new(settings);
    // Blocks the usage endpoint set survive restarts (a call during one
    // lengthens it).
    engine.restore_blocks(&backoff::load_blocks(&paths.usage_blocks_file()), unix_now());
    let mut saved_blocks = engine.blocks(unix_now());
    let host = HostClock { utc_offset_secs: 0, prefers_12h: clawdmeter_core::platform::prefers_12h() };
    let inbox = AttentionInbox::new(&paths.state_dir);
    let (mut tail, seed) = EventTail::open(paths.events_log());
    engine.seed_events(&seed);
    engine.set_hooks_installed(tail.exists());
    let (mut agents_tail, agents_seed) = EventTail::open_with(paths.agents_log(), agents::parse_line);
    for ev in &agents_seed {
        engine.on_agent_event(ev);
    }
    let cache = AddressCache::new(paths.ble_address_file());
    let wifi_cache = AddressCache::new(paths.state_dir.join("wifi-address"));
    let link_opts = LinkOptions::default();

    let mut claude = usage::find_claude(engine.settings().claude_path.as_deref(), &paths.home);
    match &claude {
        Some(c) => log::info!("claude CLI: {}", c.display()),
        None => log::warn!(
            "claude CLI not found (set claude_path in {}); usage comes from the usage endpoint every 12 min",
            paths.config_file.display()
        ),
    }
    engine.set_claude_found(claude.is_some());

    let mut link: Option<Link> = None;
    let mut device_events: Option<DeviceEvents> = None;
    let mut connecting: Option<JoinHandle<Attempt>> = None;
    let mut polling: Option<JoinHandle<PollResults>> = None;
    let mut link_state = LinkState::Starting;
    let mut reconnect = Reconnect::default();
    // The WiFi link's own retry schedule; while it waits, Bluetooth is tried.
    let mut wifi_retry = Reconnect::default();
    let mut wifi = WifiInfo::default();
    // The radio on/off line already sent on this link.
    let mut radio_sent: Option<bool> = None;
    // A provisioning line awaiting the device's ack, until this unix second.
    let mut provisioning: Option<i64> = None;
    let bt = Bluetooth::default();
    let mut wakeups = clawdmeter_ble::spawn_watcher(bt.clone());
    let mut last_settings = unix_now();
    let mut last_link_check = unix_now();
    let mut last_write_at = None;
    let mut chimes_muted = false;
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        let now = unix_now();

        // ---- commands ----
        loop {
            match commands.try_recv() {
                Ok(Command::RefreshNow) => engine.request_refresh(now),
                Ok(cmd @ (Command::Reconnect | Command::Forget)) => {
                    if let Some(l) = link.take() {
                        l.close().await;
                    }
                    if cmd == Command::Forget {
                        if let Some(key) = cache.load() {
                            cache.invalidate(&key);
                        }
                        if let Some(ip) = wifi_cache.load() {
                            wifi_cache.invalidate(&ip);
                        }
                    }
                    device_events = None;
                    link_state = LinkState::Searching;
                    reconnect.retry_in(now, 0);
                    wifi_retry.retry_in(now, 0);
                }
                Ok(Command::SetChimesMuted(m)) => {
                    chimes_muted = m;
                    let mut s = engine.settings().clone();
                    if m {
                        s.alert_chime = false;
                        s.desktop_chime = false;
                    } else if let Ok(fresh) = Settings::load(&paths) {
                        s = fresh;
                    }
                    engine.update_settings(s);
                }
                Ok(Command::ProvisionWifi { ssid, pass }) => {
                    // Fresh: the key may have been written since the last re-read.
                    let s = Settings::load(&paths).unwrap_or_else(|_| engine.settings().clone());
                    let sent = match &link {
                        Some(l @ Link::Ble(_)) => provision(&paths, l, &s, &ssid, &pass).await,
                        _ => Err("Conecta el aparato por Bluetooth para enviarle la red.".into()),
                    };
                    wifi.send = Some(match sent {
                        Ok(()) => {
                            log::info!("WiFi network sent to the device");
                            provisioning = Some(now + PROVISION_ACK_SECS);
                            (None, "Enviado. Esperando la respuesta del aparato…".into())
                        }
                        Err(e) => {
                            log::warn!("WiFi provisioning failed: {e}");
                            (Some(false), e)
                        }
                    });
                    last_settings = 0;
                }
                Ok(Command::Shutdown) | Err(mpsc::error::TryRecvError::Disconnected) => {
                    if let Some(l) = link.take() {
                        l.close().await;
                    }
                    log::info!("daemon stopped");
                    return;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
            }
        }

        // ---- config (re-read every SETTINGS_EVERY) ----
        if now - last_settings >= SETTINGS_EVERY {
            last_settings = now;
            if let Ok(mut s) = Settings::load(&paths) {
                if chimes_muted {
                    s.alert_chime = false;
                    s.desktop_chime = false;
                }
                if s.claude_path != engine.settings().claude_path || claude.is_none() {
                    claude = usage::find_claude(s.claude_path.as_deref(), &paths.home);
                }
                engine.update_settings(s);
                engine.set_claude_found(claude.is_some());
            }
        }

        // ---- fast clock ----
        for ev in tail.poll() {
            log::debug!("hook {:?} {}", ev.kind, ev.session);
            engine.on_hook_event(&ev, now);
        }
        engine.set_hooks_installed(tail.exists());
        for ev in agents_tail.poll() {
            engine.on_agent_event(&ev);
        }
        let alert = inbox.take_alert(SystemTime::now());
        let clears = inbox.take_clears();
        if alert.is_some() || !clears.is_empty() {
            engine.on_attention(alert, &clears);
        }
        if engine.take_desktop_chime() {
            crate::chime::play();
        }
        if engine.expire_attention(SystemTime::now()) {
            log::info!("attention alert expired");
        }

        // ---- slow clock ----
        if let Some(rx) = device_events.as_mut() {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    DeviceEvent::Refresh => {
                        log::info!("refresh requested by device");
                        engine.request_refresh(now);
                    }
                    DeviceEvent::Dismissed => {
                        log::info!("attention alert dismissed on the device");
                        engine.on_device_dismissed();
                    }
                    DeviceEvent::Wifi(st) => {
                        log::info!("device WiFi: {}", st.st);
                        if provisioning.take().is_some() {
                            // The device took the network: WiFi becomes the choice.
                            let r = edit_config(&paths, |c| c.set("connection", Connection::Wifi.as_str()));
                            wifi.send = Some(match r {
                                Ok(()) => (Some(true), "El aparato ha recibido la red.".into()),
                                Err(e) => (Some(false), crate::errors::io_message(&e)),
                            });
                            last_settings = 0;
                        }
                        wifi.device = Some(st);
                    }
                }
            }
        }
        if provisioning.is_some_and(|until| now >= until) {
            provisioning = None;
            wifi.send = Some((
                Some(false),
                "El aparato no respondió. Puede que su firmware no tenga WiFi; actualízalo y reinténtalo.".into(),
            ));
        }
        if polling.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Ok(results) = polling.take().expect("checked").await {
                for (d, r) in &results {
                    if let Err(e) = &r.result {
                        log::warn!("{}: {e}", d.display());
                    }
                }
                engine.on_poll_results(unix_now(), results);
                let blocks = engine.blocks(unix_now());
                if blocks != saved_blocks {
                    if let Err(e) = backoff::save_blocks(&paths.usage_blocks_file(), &blocks) {
                        log::warn!("couldn't save usage blocks: {e}");
                    }
                    saved_blocks = blocks;
                }
            }
        }
        if polling.is_none() {
            if let Some(plan) = engine.begin_poll(now) {
                log::debug!(
                    "polling ({:?}): {:?}",
                    plan.reason,
                    plan.accounts.iter().zip(&plan.plans).collect::<Vec<_>>()
                );
                let (c, home, cwd) = (claude.clone(), paths.home.clone(), paths.state_dir.clone());
                polling = Some(tokio::task::spawn_blocking(move || poll_all(c, plan, home, cwd)));
            }
        }

        // ---- link ----
        let device_key = pinned_address().or_else(|| cache.load());
        let s = engine.settings().clone();
        let wifi_ready = s.wifi_ready() && (s.wifi_host.is_some() || device_key.is_some());
        if connecting.as_ref().is_some_and(JoinHandle::is_finished) {
            let opened = match connecting.take().expect("checked").await {
                Ok(Attempt::Ble(Ok((l, rx)))) => {
                    log::info!("connected to {} ({})", l.name().unwrap_or("device"), l.key());
                    Some((Link::Ble(l), rx))
                }
                Ok(Attempt::Ble(Err(e))) => {
                    link_state = match &e {
                        LinkError::NoAdapter(s) => LinkState::NoAdapter(s.clone()),
                        _ => LinkState::Searching,
                    };
                    let wait = reconnect.failed(now);
                    if !matches!(e, LinkError::NotFound) || wait == 1 {
                        log::info!("{e}; retrying in {wait}s");
                    }
                    None
                }
                Ok(Attempt::Wifi(Ok((l, rx)))) => {
                    log::info!("connected over WiFi ({})", l.addr());
                    wifi_retry.connected();
                    // Moving off the Bluetooth fallback.
                    if let Some(old) = link.take() {
                        old.close().await;
                    }
                    Some((Link::Wifi(l), rx))
                }
                Ok(Attempt::Wifi(Err(e))) => {
                    let wait = wifi_retry.failed(now);
                    if wait == 1 {
                        log::info!("WiFi: {e}; using Bluetooth meanwhile");
                    } else {
                        log::debug!("WiFi: {e}; retrying in {wait}s");
                    }
                    None
                }
                Err(e) => {
                    log::error!("connect task failed: {e}");
                    None
                }
            };
            if let Some((l, rx)) = opened {
                let (key, name) = match &l {
                    Link::Ble(b) => (b.key().to_owned(), b.name().map(str::to_owned)),
                    Link::Wifi(t) => (
                        device_key.clone().unwrap_or_else(|| t.addr().ip().to_string()),
                        Some(clawdmeter_ble::DEVICE_NAME.into()),
                    ),
                };
                wifi.addr = match &l {
                    Link::Wifi(t) => Some(t.addr().ip().to_string()),
                    Link::Ble(_) => None,
                };
                link_state = LinkState::Connected { key, name, via: l.transport() };
                link = Some(l);
                device_events = Some(rx);
                reconnect.connected();
                radio_sent = None;
                inbox.reset();
                engine.on_connected();
            }
        }
        // Bluetooth only now: drop a WiFi link, turning the radio off first.
        if link.as_ref().is_some_and(|l| !wifi::keep(wifi_ready, l.transport())) {
            let l = link.take().expect("checked");
            if s.wifi_key.is_some() && s.connection == Connection::Ble {
                let _ = l.write(wifi::enable_line(false).as_bytes()).await;
            }
            l.close().await;
            log::info!("WiFi no longer chosen; switching to Bluetooth");
            device_events = None;
            link_state = LinkState::Searching;
            reconnect.retry_in(now, 0);
        }
        // The OS saw the device come back (BlueZ reconnected the bonded
        // device, or a scan saw it advertise): don't sit out the wait.
        while let Ok(w) = wakeups.try_recv() {
            if link.is_some() || !should_wake(&w, cache.load().as_deref()) {
                continue;
            }
            if connecting.is_none() && !reconnect.due(now) {
                let how = match w.sighting {
                    Sighting::Connected => "connected at the OS level",
                    Sighting::Advertising => "advertising",
                };
                log::info!("device {} {how}; reconnecting now", w.key);
            }
            reconnect.wake(now, connecting.is_some());
        }
        if connecting.is_none() {
            let current = link.as_ref().map(Link::transport);
            match wifi::next_attempt(wifi_ready, current, wifi_retry.due(now), reconnect.due(now)) {
                Some(Transport::Wifi) => {
                    let (host, id, wc) = (s.wifi_host.clone(), device_key.clone(), wifi_cache.clone());
                    let key = s.wifi_key.clone().unwrap_or_default();
                    connecting =
                        Some(tokio::spawn(
                            async move { Attempt::Wifi(transport::connect_wifi(host, id, wc, key).await) },
                        ));
                }
                Some(Transport::Ble) => {
                    if link_state == LinkState::Starting {
                        link_state = LinkState::Connecting;
                    }
                    let (bt, cache, opts) = (bt.clone(), cache.clone(), link_opts.clone());
                    connecting = Some(tokio::spawn(async move {
                        let r = match bt.adapter().await {
                            Ok(adapter) => DeviceLink::open(&adapter, &cache, &opts).await,
                            Err(e) => Err(e),
                        };
                        Attempt::Ble(r)
                    }));
                }
                None => {}
            }
        }
        if let Some(l) = &link {
            if now - last_link_check >= LINK_CHECK_EVERY {
                last_link_check = now;
                if !l.is_connected().await {
                    log::info!("device disconnected, reconnecting...");
                    link = None;
                    device_events = None;
                    link_state = LinkState::Searching;
                    reconnect.retry_in(now, 2);
                    wifi_retry.retry_in(now, 2);
                }
            }
        }

        // ---- write ----
        // A provisioned device's radio follows the connection choice; the
        // official firmware is never sent this (it has no key).
        if let (Some(l), true) = (&link, s.wifi_key.is_some()) {
            let on = s.connection == Connection::Wifi;
            if radio_sent != Some(on) {
                match l.write(wifi::enable_line(on).as_bytes()).await {
                    Ok(()) => radio_sent = Some(on),
                    Err(e) => log::warn!("couldn't switch the device's WiFi {}: {e}", if on { "on" } else { "off" }),
                }
            }
        }
        if let Some(l) = &link {
            if let Some(w) = engine
                .next_write(now, HostClock { utc_offset_secs: clawdmeter_core::platform::utc_offset_secs(), ..host })
            {
                let bytes = w.payload.encode(MAX_WRITE);
                let label = match w.kind {
                    WriteKind::Poll => "Sending",
                    WriteKind::Heartbeat => "Heartbeat",
                    WriteKind::Attention => "Attention",
                    WriteKind::Event => "Event",
                    WriteKind::NoData => "No data",
                    WriteKind::Agents => "Agents",
                };
                log::info!("{label}: {}", String::from_utf8_lossy(&bytes));
                match l.write(&bytes).await {
                    Ok(()) => {
                        engine.mark_written(now, &w);
                        last_write_at = Some(now);
                    }
                    Err(e) => {
                        log::warn!("write failed: {e}");
                        if !l.is_connected().await {
                            link = None;
                            device_events = None;
                            link_state = LinkState::Searching;
                            reconnect.retry_in(now, 2);
                            wifi_retry.retry_in(now, 2);
                        }
                    }
                }
            }
        }

        wifi.fallback = wifi_ready && link.as_ref().is_some_and(|l| l.transport() == Transport::Ble);
        if link.as_ref().is_none_or(|l| l.transport() != Transport::Wifi) {
            wifi.addr = None;
        }
        let _ = status.send(Status {
            link: link_state.clone(),
            engine: engine.status(now),
            last_write_at,
            claude: claude.clone(),
            wifi: wifi.clone(),
        });
    }
}

/// One poll of every account, printed as the payload the device would get
/// (`clawdmeter --once`). No BLE.
pub fn once(paths: &Paths) -> i32 {
    let settings = match Settings::load(paths) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("config: {e}");
            return 1;
        }
    };
    let claude = usage::find_claude(settings.claude_path.as_deref(), &paths.home);
    let now = unix_now();
    let mut engine = Engine::new(settings.clone());
    engine.restore_blocks(&backoff::load_blocks(&paths.usage_blocks_file()), now);
    // A blocked account isn't asked: a call during a block lengthens it.
    let blocked = engine.blocks(now);
    for (d, b) in &blocked {
        println!("{}: {}", d.display(), usage::paused_message(b.until, clawdmeter_core::platform::utc_offset_secs()));
    }
    // Nothing is known yet, so the idle source for every account (no message).
    let accounts: Vec<PathBuf> = settings.config_dirs.iter().filter(|d| !blocked.contains_key(*d)).cloned().collect();
    let plan = PollPlan {
        reason: clawdmeter_core::cadence::PollReason::Startup,
        plans: vec![probe::Plan::Usage; accounts.len()],
        accounts,
    };
    let results = poll_all(claude, plan, paths.home.clone(), paths.state_dir.clone());
    let mut ok = false;
    for (d, r) in &results {
        match &r.result {
            Ok(r) if r.is_spend_only() => {
                ok = true;
                println!("{}: spend limit {}% used", d.display(), r.session_pct());
            }
            Ok(r) => {
                ok = true;
                println!(
                    "{}: session {}%{}, week {}%",
                    d.display(),
                    r.session_pct(),
                    if r.window_started() { "" } else { " (window not started)" },
                    r.week.as_ref().map_or(0, |w| w.pct)
                );
            }
            Err(e) => println!("{}: {e}", d.display()),
        }
    }
    engine.on_poll_results(now, results);
    if let Err(e) = backoff::save_blocks(&paths.usage_blocks_file(), &engine.blocks(unix_now())) {
        eprintln!("couldn't save usage blocks: {e}");
    }
    let host = HostClock {
        utc_offset_secs: clawdmeter_core::platform::utc_offset_secs(),
        prefers_12h: clawdmeter_core::platform::prefers_12h(),
    };
    if let Some(w) = engine.next_write(now, host) {
        println!("{}", String::from_utf8_lossy(&w.payload.encode(MAX_WRITE)));
    }
    if ok {
        0
    } else {
        1
    }
}
