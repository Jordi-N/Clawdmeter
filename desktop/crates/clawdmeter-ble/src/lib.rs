//! BLE link to a Clawdmeter (btleplug).
//!
//! Finding the device: only a device the OS already
//! knows is used (paired on Linux, OS-connected on macOS/Windows, where the
//! bonded HID keyboard link keeps it connected), matched by the custom data
//! service or, failing that, by the exact name `Clawdmeter`. There is no
//! name scan by default, so a stranger's unit in range is never picked; a
//! filtered scan is available for first-time setup (`allow_scan`).
//!
//! The chosen device's address is cached in `ble-address` in the state dir
//! (`~/.config/clawdmeter`); a failed connect drops the cache so the next
//! attempt re-derives it.
//!
//! Per-OS recovery:
//!
//! * Linux: the device is marked trusted in BlueZ on connect (like
//!   `bluetoothctl trust`), so BlueZ reconnects it by itself.
//! * macOS: a connect that fails to encrypt (CoreBluetooth error 15, a bond
//!   left over from before a reflash or the on-device bond clear) unpairs
//!   the stale bond with `blueutil` when it is installed, so the next attempt
//!   re-bonds silently.
//! * Windows: a bonded device the OS isn't currently connected to is reached
//!   by address: `CLAWDMETER_BLE_ADDRESS`, the cached address, or the
//!   address in the Bluetooth PnP table.
//! * `CLAWDMETER_BLE_ADDRESS` (every OS) names the device to try first.
//! * Everywhere: writes that keep failing on a link that still reports
//!   itself connected (a half-open "zombie" link) make
//!   [`DeviceLink::is_connected`] report false, so the caller reconnects.
//!
//! Adapters come from one [`Bluetooth`] handle per runtime, never a fresh
//! btleplug manager per attempt (on Linux each leaks a D-Bus connection).
//!
//! Logging: lines start with `ble: ` so `daemon.log` can be searched. Repeats
//! of an identical failure line are held back for 10 minutes and the next one
//! carries the count; a run of links that die within seconds of connecting
//! logs `ble: stale bond suspected`.
//!
//! Reconnect timing is [`Reconnect`] (backoff capped at
//! [`reconnect::BACKOFF_CAP`]), cut short by [`spawn_watcher`]'s wakeups when
//! the OS sees the device come back (BlueZ auto-reconnecting the bonded
//! device, or an advertisement during a scan).

#![warn(missing_docs)]

pub mod reconnect;
mod watch;

pub use reconnect::Reconnect;
pub use watch::{should_wake, spawn_watcher, Sighting, Wake, Wakeups};

use std::fs;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use btleplug::api::{
    BDAddr, Central, CentralState, Characteristic, Manager as _, Peripheral as _, RetrievePeripheralsOptions,
    ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{Stream, StreamExt};
use log::Level;
use tokio::sync::mpsc;
use uuid::Uuid;

use clawdmeter_core::wifi::{parse_device_line, DeviceLine};
pub use clawdmeter_core::wifi::{Battery, WifiStatus};
pub use clawdmeter_core::DEVICE_NAME;

/// Custom data service.
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x4c41555a_4465_7669_6365_000000000001);
/// Host writes JSON here.
pub const RX_UUID: Uuid = Uuid::from_u128(0x4c41555a_4465_7669_6365_000000000002);
/// Device ack/nack notifications (unused by the daemon).
pub const TX_UUID: Uuid = Uuid::from_u128(0x4c41555a_4465_7669_6365_000000000003);
/// Device-initiated refresh request (notify `0x01`).
pub const REQ_UUID: Uuid = Uuid::from_u128(0x4c41555a_4465_7669_6365_000000000004);
/// Standard HID service, which the device also exposes.
pub const HID_SERVICE_UUID: Uuid = Uuid::from_u128(0x00001812_0000_1000_8000_00805f9b34fb);

/// Largest payload written in one go. The firmware's RX buffer is 512 bytes;
/// writes use write-with-response, which the stacks split into a long write
/// when it exceeds the MTU.
pub const MAX_WRITE: usize = 500;

/// Link failures.
#[derive(Debug)]
pub enum LinkError {
    /// No Bluetooth adapter, or it is off.
    NoAdapter(String),
    /// No paired / OS-connected Clawdmeter.
    NotFound,
    /// The connect, discovery or a write failed.
    Ble(btleplug::Error),
    /// The connect or discovery took too long.
    Timeout(&'static str),
    /// The device lacks the data service's RX characteristic.
    MissingCharacteristic,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::NoAdapter(s) => write!(f, "no Bluetooth adapter: {s}"),
            LinkError::NotFound => write!(f, "no paired {DEVICE_NAME} found"),
            LinkError::Ble(e) => write!(f, "BLE error: {e}"),
            LinkError::Timeout(what) => write!(f, "timed out during {what}"),
            LinkError::MissingCharacteristic => write!(f, "device has no Clawdmeter RX characteristic"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<btleplug::Error> for LinkError {
    fn from(e: btleplug::Error) -> Self {
        LinkError::Ble(e)
    }
}

/// The one-line address cache.
#[derive(Debug, Clone)]
pub struct AddressCache {
    path: PathBuf,
}

impl AddressCache {
    /// Cache at `path` (normally `Paths::ble_address_file()`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AddressCache { path: path.into() }
    }

    /// The cached key (MAC on Linux/Windows, CoreBluetooth UUID on macOS).
    pub fn load(&self) -> Option<String> {
        let raw = fs::read_to_string(&self.path).ok()?;
        let key = raw.lines().next()?.trim().to_owned();
        if key.is_empty() {
            return None;
        }
        // Only MACs (or btleplug ids) are ever written; reject garbage.
        if key.contains(':') && !is_mac(&key) {
            let _ = fs::remove_file(&self.path);
            return None;
        }
        Some(key)
    }

    /// Remember a device key.
    pub fn save(&self, key: &str) -> io::Result<()> {
        if let Some(d) = self.path.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(&self.path, format!("{key}\n"))
    }

    /// Drop the cache, but only if it still holds `key` (another process may
    /// have re-cached a different device meanwhile).
    pub fn invalidate(&self, key: &str) {
        if self.load().is_some_and(|k| k.eq_ignore_ascii_case(key)) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6 && parts.iter().all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The stable key for a peripheral: its MAC when the platform exposes one,
/// else its platform id (macOS UUID).
pub fn peripheral_key(p: &Peripheral) -> String {
    let addr = p.address();
    if addr != BDAddr::default() {
        addr.to_string()
    } else {
        p.id().to_string()
    }
}

/// Connect / discovery knobs.
#[derive(Debug, Clone)]
pub struct LinkOptions {
    /// Bound on `connect()`; CoreBluetooth can otherwise wedge forever on a
    /// half-open HID link.
    pub connect_timeout: Duration,
    /// Bound on GATT discovery.
    pub discover_timeout: Duration,
    /// Bound on the REQ subscription (optional; polling works without it).
    pub subscribe_timeout: Duration,
    /// Fall back to a service-filtered scan when no known device is found
    /// (first-time setup only).
    pub allow_scan: bool,
    /// How long that scan runs.
    pub scan_time: Duration,
}

impl Default for LinkOptions {
    fn default() -> Self {
        LinkOptions {
            connect_timeout: Duration::from_secs(20),
            discover_timeout: Duration::from_secs(15),
            subscribe_timeout: Duration::from_secs(10),
            allow_scan: false,
            scan_time: Duration::from_secs(8),
        }
    }
}

/// The Bluetooth stack: one btleplug [`Manager`], made on first use and
/// shared by every clone. Its I/O task runs on the runtime of the
/// [`adapter`](Self::adapter) call that made it, so keep a handle to one
/// runtime and pass clones around.
///
/// On Linux a `Manager` is a D-Bus system-bus connection whose I/O task
/// btleplug spawns detached, so dropping the manager never closes it. A
/// manager per connect attempt leaked one connection per attempt until the
/// per-user limit (256) starved the whole session. The manager is replaced
/// only once a throwaway connection proves a new one would fare better and
/// a retry on its own connection still fails — meaning this one's
/// connection is dead and its socket already closed.
/// [`connection_losses`](Self::connection_losses) counts the replacements:
/// a stream opened on a lost connection must not be dropped, so the wakeup
/// watcher leaks it (see `spawn_watcher`).
#[derive(Clone)]
pub struct Bluetooth {
    manager: Arc<tokio::sync::Mutex<Option<Arc<Manager>>>>,
    lost_connections: Arc<tokio::sync::watch::Sender<u64>>,
    /// The last [`adapter`](Self::adapter) outcome (`None` before the first),
    /// so only changes are logged.
    adapter_seen: Arc<Mutex<Option<Result<(), String>>>>,
}

impl Default for Bluetooth {
    fn default() -> Self {
        Bluetooth {
            manager: Arc::default(),
            lost_connections: Arc::new(tokio::sync::watch::channel(0).0),
            adapter_seen: Arc::default(),
        }
    }
}

impl Bluetooth {
    /// The first powered adapter. Logs when it goes missing or off, and when
    /// it comes back.
    pub async fn adapter(&self) -> Result<Adapter, LinkError> {
        let r = self.find_adapter().await;
        let now = r.as_ref().map(|_| ()).map_err(LinkError::to_string);
        let mut seen = self.adapter_seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.as_ref() != Some(&now) {
            match (&*seen, &now) {
                (_, Err(why)) => log::warn!("ble: adapter unavailable: {why}"),
                (Some(Err(_)), Ok(())) => log::info!("ble: adapter available again"),
                (_, Ok(())) => log::info!("ble: adapter ready"),
            }
            *seen = Some(now);
        }
        r
    }

    async fn find_adapter(&self) -> Result<Adapter, LinkError> {
        let manager = self.manager().await?;
        let adapters = match manager.adapters().await {
            Ok(a) => a,
            Err(e) => {
                if !connection_lost().await {
                    return Err(LinkError::NoAdapter(e.to_string()));
                }
                // The probe can find bluetoothd answering again just after
                // this call hit it restarting, so ask this connection once
                // more: a dead one fails again at once, a live one answers.
                match manager.adapters().await {
                    Ok(a) => a,
                    Err(_) => {
                        // Whoever drops the dead manager counts the loss, so
                        // streams opened on its connection know they must
                        // not be dropped.
                        if self.manager.lock().await.take_if(|m| Arc::ptr_eq(m, &manager)).is_some() {
                            self.lost_connections.send_modify(|n| *n += 1);
                            log::warn!("ble: D-Bus connection to bluetoothd lost ({e}); replacing the manager");
                        }
                        return Err(LinkError::NoAdapter(e.to_string()));
                    }
                }
            }
        };
        let adapter = adapters.into_iter().next().ok_or_else(|| LinkError::NoAdapter("none present".into()))?;
        if let Ok(CentralState::PoweredOff) = adapter.adapter_state().await {
            return Err(LinkError::NoAdapter("adapter is off".into()));
        }
        Ok(adapter)
    }

    /// How often the manager was replaced after its D-Bus connection died.
    /// The wakeup watcher reopens its stream on every bump and reads the
    /// count to tell whether a finished stream is safe to drop.
    pub(crate) fn connection_losses(&self) -> tokio::sync::watch::Receiver<u64> {
        self.lost_connections.subscribe()
    }

    async fn manager(&self) -> Result<Arc<Manager>, LinkError> {
        let mut slot = self.manager.lock().await;
        if let Some(m) = &*slot {
            return Ok(m.clone());
        }
        let m = Arc::new(Manager::new().await.map_err(|e| LinkError::NoAdapter(e.to_string()))?);
        *slot = Some(m.clone());
        Ok(m)
    }
}

/// Is the manager's D-Bus connection gone — or would a fresh one hit the
/// same error? btleplug boxes bluez-async's error out of reach (its
/// `source()` chain skips the `dbus::Error` inside), so instead of reading
/// the error this asks the bus itself over a throwaway connection; dbus-rs
/// closes those on drop, so the probe leaks nothing. Only a bus or
/// bluetoothd that demonstrably answers proves the manager's connection
/// the dead one; anything else errs on the side of keeping it, since a
/// missed loss only delays recovery while a spurious one leaks a live
/// connection.
#[cfg(target_os = "linux")]
async fn connection_lost() -> bool {
    let reach = tokio::task::spawn_blocking(bluez::reach).await.unwrap_or(bluez::Reach::BluetoothUnreachable);
    lost_on(reach)
}

/// No system-bus connection to lose elsewhere.
#[cfg(not(target_os = "linux"))]
async fn connection_lost() -> bool {
    false
}

/// Which [`bluez::Reach`] results mean the manager's connection is dead.
#[cfg(target_os = "linux")]
fn lost_on(reach: bluez::Reach) -> bool {
    matches!(reach, bluez::Reach::Reached | bluez::Reach::BusUnreachable)
}

/// A candidate device the OS knows about.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The btleplug handle.
    pub peripheral: Peripheral,
    /// Cache key.
    pub key: String,
    /// Advertised / cached name.
    pub name: Option<String>,
    /// Exposes the custom data service (unambiguous).
    pub has_service: bool,
    /// How it was found, for the log: `os-known`, `bonded-address` or `scan`.
    pub found_by: &'static str,
}

pub(crate) async fn describe(p: Peripheral) -> Candidate {
    let props = p.properties().await.ok().flatten();
    let name = props.as_ref().and_then(|pr| pr.local_name.clone());
    let has_service = props.as_ref().is_some_and(|pr| pr.services.contains(&SERVICE_UUID))
        || p.services().iter().any(|s| s.uuid == SERVICE_UUID);
    Candidate { key: peripheral_key(&p), peripheral: p, name, has_service, found_by: "os-known" }
}

/// True when a candidate is ours: the custom service alone is enough; the
/// HID service (shared with every keyboard) needs the exact name.
pub fn accept(name: Option<&str>, has_service: bool) -> bool {
    has_service || name == Some(DEVICE_NAME)
}

/// Pick among accepted candidates: the cached key first, else the first.
pub fn choose<'a>(cands: &'a [Candidate], cached: Option<&str>) -> Option<&'a Candidate> {
    connect_order(cands, cached).into_iter().next()
}

/// The order to try candidates in: the cached key first, then the rest as
/// listed. Trying them all means a second paired unit that is switched off
/// (e.g. the board that was swapped out) can't block the one that's on.
pub fn connect_order<'a>(cands: &'a [Candidate], cached: Option<&str>) -> Vec<&'a Candidate> {
    let is_cached = |c: &Candidate| cached.is_some_and(|k| c.key.eq_ignore_ascii_case(k));
    cands.iter().filter(|c| is_cached(c)).chain(cands.iter().filter(|c| !is_cached(c))).collect()
}

/// List Clawdmeters the OS already knows, without scanning.
pub async fn known_devices(adapter: &Adapter) -> Vec<Candidate> {
    let opts = RetrievePeripheralsOptions { identifiers: None, services: Some(vec![SERVICE_UUID, HID_SERVICE_UUID]) };
    let mut list: Vec<Peripheral> = match adapter.retrieve_peripherals(opts).await {
        Ok(l) => l,
        Err(e) => {
            log_repeated(Level::Warn, &format!("ble: listing OS-connected devices failed: {e}"));
            Vec::new()
        }
    };
    if cfg!(target_os = "linux") {
        // BlueZ also lists paired-but-disconnected devices; connecting to
        // them reconnects without a scan.
        let known = adapter.peripherals().await.unwrap_or_else(|e| {
            log_repeated(Level::Warn, &format!("ble: listing BlueZ devices failed: {e}"));
            Vec::new()
        });
        for p in known {
            if !list.iter().any(|q| q.id() == p.id()) {
                list.push(p);
            }
        }
    }
    let mut out = Vec::new();
    for p in list {
        let c = describe(p).await;
        if !accept(c.name.as_deref(), c.has_service) {
            continue;
        }
        #[cfg(target_os = "linux")]
        {
            // Never pick an unpaired unit BlueZ merely saw advertising.
            let id = c.peripheral.id().to_string();
            let paired = tokio::task::spawn_blocking(move || bluez::is_paired(&id)).await.unwrap_or(Ok(false));
            if !paired.unwrap_or(false) {
                log_repeated(Level::Info, &format!("ble: ignoring {}: seen by BlueZ but not paired", c.key));
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Scan for advertisements of the data service (first-time setup).
pub async fn scan(adapter: &Adapter, time: Duration) -> Vec<Candidate> {
    if let Err(e) = adapter.start_scan(ScanFilter { services: vec![SERVICE_UUID] }).await {
        log_repeated(Level::Warn, &format!("ble: scan failed to start: {e}"));
        return Vec::new();
    }
    tokio::time::sleep(time).await;
    let _ = adapter.stop_scan().await;
    let mut out = Vec::new();
    for p in adapter.peripherals().await.unwrap_or_default() {
        let c = describe(p).await;
        if accept(c.name.as_deref(), c.has_service) {
            out.push(Candidate { found_by: "scan", ..c });
        }
    }
    out
}

/// Consecutive failed writes after which a link that still claims to be
/// connected is treated as dead. The Windows daemon broke after one; two
/// rides out a single transient error without stalling the 120 s budget.
pub const ZOMBIE_WRITE_FAILURES: u32 = 2;

/// Counts consecutive write failures (the zombie-link breaker).
#[derive(Debug, Default)]
pub struct WriteHealth {
    failures: AtomicU32,
}

impl WriteHealth {
    /// Record one write outcome.
    pub fn record(&self, ok: bool) {
        if ok {
            self.failures.store(0, Ordering::Relaxed);
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Too many failures in a row: abandon the link.
    pub fn is_zombie(&self) -> bool {
        self.failures.load(Ordering::Relaxed) >= ZOMBIE_WRITE_FAILURES
    }
}

/// How long identical log lines are held back after one is logged.
const REPEAT_WINDOW: Duration = Duration::from_secs(600);

/// Rate limit for identical log lines: the first is logged, repeats within
/// [`REPEAT_WINDOW`] are counted, and the first one after it carries the
/// count.
struct Repeats {
    /// (line, when it was last logged, repeats held back since).
    seen: Vec<(String, Instant, u32)>,
}

impl Repeats {
    /// `Some((held, since))` when `line` is to be logged now: `held`
    /// identical lines were held back since it was last logged, `since` ago.
    /// `None` holds it back.
    fn admit(&mut self, line: &str, now: Instant) -> Option<(u32, Duration)> {
        if let Some(e) = self.seen.iter_mut().find(|e| e.0 == line) {
            let since = now.duration_since(e.1);
            if since < REPEAT_WINDOW {
                e.2 += 1;
                return None;
            }
            e.1 = now;
            return Some((std::mem::take(&mut e.2), since));
        }
        // ponytail: forgets every count past 64 distinct lines; evict the
        // oldest instead if varied error texts ever churn through it.
        if self.seen.len() >= 64 {
            self.seen.clear();
        }
        self.seen.push((line.to_owned(), now, 0));
        Some((0, Duration::ZERO))
    }
}

static REPEATS: Mutex<Repeats> = Mutex::new(Repeats { seen: Vec::new() });

/// Log `line` at `level`, holding back identical repeats (see [`Repeats`]).
pub(crate) fn log_repeated(level: Level, line: &str) {
    let admitted = REPEATS.lock().unwrap_or_else(|e| e.into_inner()).admit(line, Instant::now());
    match admitted {
        Some((0, _)) => log::log!(level, "{line}"),
        Some((held, since)) => log::log!(level, "{line} ({held} more in the last {} min)", since.as_secs() / 60),
        None => {}
    }
}

/// A link that dies this soon after the OS connected it is short-lived.
const SHORT_LINK: Duration = Duration::from_secs(30);
/// Short-lived links within [`STALE_BOND_WINDOW`] that point at a stale bond.
const STALE_BOND_LINKS: usize = 3;
/// The window [`STALE_BOND_LINKS`] are counted in, and the least time
/// between two stale-bond warnings.
const STALE_BOND_WINDOW: Duration = Duration::from_secs(300);

/// Recent short-lived links: connect, then a drop or failed discovery within
/// [`SHORT_LINK`]. Repeated, that is the stale-bond pattern: the computer
/// keeps a pairing key the device lost, so encryption fails and the link
/// drops right after it comes up (seen on Windows).
struct ShortLinks {
    at: Vec<Instant>,
    warned: Option<Instant>,
}

impl ShortLinks {
    /// Record one. `Some(n)`: `n` within the window, time to warn (at most
    /// once per window).
    fn record(&mut self, now: Instant) -> Option<usize> {
        self.at.retain(|t| now.duration_since(*t) < STALE_BOND_WINDOW);
        self.at.push(now);
        let quiet = self.warned.is_none_or(|w| now.duration_since(w) >= STALE_BOND_WINDOW);
        if self.at.len() < STALE_BOND_LINKS || !quiet {
            return None;
        }
        self.warned = Some(now);
        Some(self.at.len())
    }
}

static SHORT_LINKS: Mutex<ShortLinks> = Mutex::new(ShortLinks { at: Vec::new(), warned: None });

/// A link to `key` died within [`SHORT_LINK`] of connecting.
fn note_short_link(key: &str) {
    let Some(n) = SHORT_LINKS.lock().unwrap_or_else(|e| e.into_inner()).record(Instant::now()) else { return };
    log::warn!(
        "ble: stale bond suspected: {n} links died within {}s of connecting in the last {} min (last: {key}). This \
         computer likely keeps a pairing key the device lost (reflash or bond clear on the device): remove \
         '{DEVICE_NAME}' in the OS Bluetooth settings and pair it again",
        SHORT_LINK.as_secs(),
        STALE_BOND_WINDOW.as_secs() / 60
    );
}

/// A canonical MAC from a Windows Bluetooth PnP instance id:
/// `BTHLE\DEV_98A316A5D706\7&B8081D1&0&98A316A5D706` → `98:A3:16:A5:D7:06`.
pub fn mac_from_pnp_instance_id(instance_id: &str) -> Option<String> {
    let up = instance_id.to_ascii_uppercase();
    let mut rest = up.as_str();
    while let Some(i) = rest.find("DEV_") {
        let tail = &rest[i + 4..];
        let hex: String = tail.chars().take_while(char::is_ascii_hexdigit).collect();
        if hex.len() == 12 {
            let pairs: Vec<&str> = (0..6).map(|k| &hex[k * 2..k * 2 + 2]).collect();
            return Some(pairs.join(":"));
        }
        rest = tail;
    }
    None
}

/// A MAC string (`AA:BB:CC:DD:EE:FF`, any case) as a [`BDAddr`].
pub fn parse_mac(s: &str) -> Option<BDAddr> {
    let s = s.trim();
    if !is_mac(s) {
        return None;
    }
    BDAddr::from_str_delim(s).ok()
}

/// True when a connect error is macOS's stale-bond failure (CBErrorDomain
/// code 15, "Failed to encrypt the connection").
pub fn is_encryption_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("code=15") || m.contains("encrypt")
}

/// The address of the paired device named `name` in `blueutil --paired`
/// output (`address: 28-84-85-55-5c-3d, ... name: "Clawdmeter", ...`).
pub fn blueutil_paired_address(out: &str, name: &str) -> Option<String> {
    let needle = format!("name: \"{name}\"");
    out.lines().filter(|l| l.contains(&needle)).find_map(|l| {
        let rest = &l[l.find("address:")? + "address:".len()..];
        let addr: String =
            rest.trim_start().chars().take_while(|c| c.is_ascii_hexdigit() || *c == '-' || *c == ':').collect();
        (!addr.is_empty()).then_some(addr)
    })
}

/// `CLAWDMETER_BLE_ADDRESS`: a MAC (or, on macOS, a CoreBluetooth UUID)
/// to try before any other known device.
pub fn pinned_address() -> Option<String> {
    std::env::var("CLAWDMETER_BLE_ADDRESS").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// What the device asks or tells the app through its notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// REQ (or `{"req":true}` over TCP): it has no data yet and wants a
    /// payload.
    Refresh,
    /// TX `{"dismiss":true}`: the alert ended on the device (a tap, or its
    /// own timeout).
    Dismissed,
    /// TX `{"wifi":{...}}`: its WiFi state (fork firmware only).
    Wifi(WifiStatus),
    /// `{"bat":{...}}`: the device's battery (fork firmware).
    Battery(Battery),
    /// A button for the app to type on this computer (USB/WiFi links only).
    Key {
        /// `space` | `shift_tab`.
        name: String,
        /// Pressed (true) or released.
        down: bool,
    },
}

/// The device's events, in order.
pub type DeviceEvents = mpsc::UnboundedReceiver<DeviceEvent>;

/// Read one notification. TX also carries the write acks
/// (`{"ack":true}` / `{"err":true}`), which mean nothing here.
pub fn device_event(uuid: Uuid, value: &[u8]) -> Option<DeviceEvent> {
    if uuid == REQ_UUID {
        return Some(DeviceEvent::Refresh);
    }
    if uuid != TX_UUID {
        return None;
    }
    line_event(std::str::from_utf8(value).ok()?)
}

/// Read one JSON line from the device, as TX carries it or the TCP link
/// receives it.
pub fn line_event(text: &str) -> Option<DeviceEvent> {
    match parse_device_line(text)? {
        DeviceLine::Refresh => Some(DeviceEvent::Refresh),
        DeviceLine::Dismissed => Some(DeviceEvent::Dismissed),
        DeviceLine::Wifi(s) => Some(DeviceEvent::Wifi(s)),
        DeviceLine::Key { name, down } => Some(DeviceEvent::Key { name, down }),
        DeviceLine::Battery(b) => Some(DeviceEvent::Battery(b)),
        DeviceLine::AuthOk | DeviceLine::Challenge { .. } => None,
    }
}

/// An open GATT link.
pub struct DeviceLink {
    peripheral: Peripheral,
    rx: Characteristic,
    key: String,
    name: Option<String>,
    health: WriteHealth,
    /// When the OS link came up (or was found up), for the drop log.
    up_at: Instant,
    /// The drop (or close) was logged already.
    down_logged: AtomicBool,
}

impl std::fmt::Debug for DeviceLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceLink").field("key", &self.key).field("name", &self.name).finish()
    }
}

impl DeviceLink {
    /// Find, connect, discover and subscribe. On a failed connect the address
    /// cache is dropped (if it pointed at that device).
    pub async fn open(
        adapter: &Adapter,
        cache: &AddressCache,
        opts: &LinkOptions,
    ) -> Result<(DeviceLink, DeviceEvents), LinkError> {
        // CLAWDMETER_BLE_ADDRESS pins the device to try first (the bash
        // daemon's DEVICE_MAC, the Windows daemon's override); else the cache.
        let cached = pinned_address().or_else(|| cache.load());
        let mut cands = known_devices(adapter).await;
        if cands.is_empty() {
            cands = bonded_by_address(adapter, cached.as_deref()).await;
        }
        if cands.is_empty() && opts.allow_scan {
            cands = scan(adapter, opts.scan_time).await;
        }
        let order = connect_order(&cands, cached.as_deref());
        if order.is_empty() {
            log_repeated(Level::Info, &format!("ble: no {DEVICE_NAME} known to the OS (scan: {})", opts.allow_scan));
        } else {
            let list: Vec<String> = order
                .iter()
                .map(|c| {
                    let how = if c.has_service { "data service" } else { "name match" };
                    format!("{} ({}, {how})", c.key, c.found_by)
                })
                .collect();
            let hint = cached.as_deref().unwrap_or("none");
            log_repeated(Level::Info, &format!("ble: trying {} (cached or pinned: {hint})", list.join(", ")));
        }
        let mut last_err = LinkError::NotFound;
        for cand in order {
            match Self::connect(cand.clone(), opts).await {
                Ok(ok) => {
                    if !cached.as_deref().is_some_and(|k| k.eq_ignore_ascii_case(&cand.key)) {
                        let _ = cache.save(&cand.key);
                    }
                    return Ok(ok);
                }
                Err(e) => {
                    if cfg!(target_os = "macos") && is_encryption_error(&e.to_string()) {
                        log::warn!("encryption failed: likely a stale macOS bond; trying to unpair it");
                        let _ = tokio::task::spawn_blocking(macos_unpair_stale_bond).await;
                    }
                    cache.invalidate(&cand.key);
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// Connect to a specific candidate. Logs how long each step took, or
    /// the step that failed.
    pub async fn connect(cand: Candidate, opts: &LinkOptions) -> Result<(DeviceLink, DeviceEvents), LinkError> {
        let (key, found_by) = (cand.key.clone(), cand.found_by);
        let started = Instant::now();
        let r = Self::establish(cand, opts, started).await;
        if let Err((step, e)) = &r {
            let secs = started.elapsed().as_secs();
            let line = format!("ble: connect to {key} ({found_by}) failed at {step} after {secs}s: {e}");
            log_repeated(Level::Warn, &line);
        }
        r.map_err(|(_, e)| e)
    }

    async fn establish(
        cand: Candidate,
        opts: &LinkOptions,
        started: Instant,
    ) -> Result<(DeviceLink, DeviceEvents), (&'static str, LinkError)> {
        let p = cand.peripheral;
        let reused = p.is_connected().await.unwrap_or(false);
        if !reused {
            let r = match tokio::time::timeout(opts.connect_timeout, p.connect()).await {
                Ok(r) => r.map_err(LinkError::Ble),
                Err(_) => Err(LinkError::Timeout("connect")),
            };
            if let Err(e) = r {
                // Cancel BlueZ's pending Connect, or the next attempt fails
                // with "Operation already in progress".
                let _ = p.disconnect().await;
                return Err(("connect", e));
            }
        }
        let up_at = Instant::now();
        let discovered = match tokio::time::timeout(opts.discover_timeout, p.discover_services()).await {
            Ok(r) => r.map_err(LinkError::Ble),
            Err(_) => Err(LinkError::Timeout("service discovery")),
        };
        if let Err(e) = discovered {
            // The link came up but GATT didn't answer: one sign of a stale
            // bond (encryption fails, the device drops the link).
            note_short_link(&cand.key);
            return Err(("service discovery", e));
        }
        let chars = p.characteristics();
        let Some(rx) = chars.iter().find(|c| c.uuid == RX_UUID && c.service_uuid == SERVICE_UUID).cloned() else {
            // Old firmware, another device under our name, or a stale GATT
            // cache (Windows keeps one per bond).
            let mut services: Vec<String> = p.services().iter().map(|s| s.uuid.to_string()).collect();
            services.sort();
            log::warn!("ble: {} has no RX characteristic; services: {}", cand.key, services.join(", "));
            return Err(("characteristic lookup", LinkError::MissingCharacteristic));
        };
        let (tx, events) = mpsc::unbounded_channel();
        let notifying: Vec<_> = chars
            .iter()
            .filter(|c| (c.uuid == REQ_UUID || c.uuid == TX_UUID) && c.service_uuid == SERVICE_UUID)
            .cloned()
            .collect();
        if !notifying.is_empty() {
            // Listen before subscribing: the device fires its REQ notify
            // inside the subscribe (it asks only when it has no data yet).
            match p.notifications().await {
                Ok(stream) => spawn_event_forwarder(stream, tx, cand.key.clone()),
                Err(e) => log::warn!("ble: no notification stream from {}: {e}", cand.key),
            }
        }
        let mut subscribed = Vec::new();
        for c in &notifying {
            let what = if c.uuid == REQ_UUID { "REQ" } else { "TX" };
            match tokio::time::timeout(opts.subscribe_timeout, p.subscribe(c)).await {
                Ok(Ok(())) => subscribed.push(what),
                Ok(Err(e)) => log_repeated(Level::Warn, &format!("ble: {what} subscription unavailable: {e}")),
                Err(_) => log_repeated(Level::Warn, &format!("ble: {what} subscription timed out; carrying on")),
            }
        }
        #[cfg(target_os = "linux")]
        {
            // `bluetoothctl trust`: BlueZ then
            // reconnects the device on its own after it drops.
            let id = p.id().to_string();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || bluez::trust(&id)).await {
                log::debug!("could not mark the device trusted: {e}");
            }
        }
        let connect_ms = (up_at - started).as_millis();
        let how = if reused { "OS link reused".to_owned() } else { format!("connect {connect_ms} ms") };
        let subs = if subscribed.is_empty() { "none".to_owned() } else { subscribed.join("+") };
        log::info!(
            "ble: link to {} ({}) ready in {} ms ({how}, discovery {} ms, subscribed: {subs})",
            cand.key,
            cand.found_by,
            started.elapsed().as_millis(),
            up_at.elapsed().as_millis()
        );
        let link = DeviceLink {
            key: cand.key,
            name: cand.name,
            peripheral: p,
            rx,
            health: WriteHealth::default(),
            up_at,
            down_logged: AtomicBool::new(false),
        };
        Ok((link, events))
    }

    /// Write one payload (already encoded to at most [`MAX_WRITE`] bytes).
    pub async fn write(&self, data: &[u8]) -> Result<(), LinkError> {
        let r = match tokio::time::timeout(
            Duration::from_secs(10),
            self.peripheral.write(&self.rx, data, WriteType::WithResponse),
        )
        .await
        {
            Ok(r) => r.map_err(LinkError::Ble),
            Err(_) => Err(LinkError::Timeout("write")),
        };
        let was_zombie = self.health.is_zombie();
        self.health.record(r.is_ok());
        if let (Err(e), false, true) = (&r, was_zombie, self.health.is_zombie()) {
            log::warn!(
                "ble: zombie link to {}: {ZOMBIE_WRITE_FAILURES} writes in a row failed on a live-looking link \
                 (last: {e}); dropping it",
                self.key
            );
        }
        r
    }

    /// Still connected? False once writes keep failing (a zombie link), so
    /// the caller drops it and reconnects. The first false logs why and how
    /// long the link lasted.
    pub async fn is_connected(&self) -> bool {
        let why = if self.health.is_zombie() {
            "writes kept failing (zombie link)".to_owned()
        } else {
            match self.peripheral.is_connected().await {
                Ok(true) => return true,
                Ok(false) => "the OS reports it disconnected".to_owned(),
                Err(e) => format!("connection state unreadable: {e}"),
            }
        };
        if !self.down_logged.swap(true, Ordering::Relaxed) {
            let lived = self.up_at.elapsed();
            log::info!("ble: link to {} down after {}s: {why}", self.key, lived.as_secs());
            if lived < SHORT_LINK {
                note_short_link(&self.key);
            }
        }
        false
    }

    /// Drop the GATT connection (the OS may keep the HID link).
    pub async fn close(&self) {
        if !self.down_logged.swap(true, Ordering::Relaxed) {
            log::info!("ble: closing link to {} after {}s", self.key, self.up_at.elapsed().as_secs());
        }
        if let Err(e) = self.peripheral.disconnect().await {
            log::debug!("ble: disconnect from {}: {e}", self.key);
        }
    }

    /// Cache key (address).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Device name, if known.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Platform id string (BlueZ: `hci0/dev_AA_BB_...`).
    pub fn id(&self) -> String {
        self.peripheral.id().to_string()
    }
}

/// Windows: a bonded device that Windows isn't connected to right now is
/// absent from `known_devices` (which lists connected peripherals only), but
/// WinRT can still connect to it by address. Addresses come from
/// `CLAWDMETER_BLE_ADDRESS`, then the cache, then the PnP table. Elsewhere
/// this finds nothing: BlueZ already lists paired devices, and macOS has no
/// way to reach a peripheral the OS doesn't hold.
#[cfg(not(windows))]
async fn bonded_by_address(_adapter: &Adapter, _cached: Option<&str>) -> Vec<Candidate> {
    Vec::new()
}

#[cfg(windows)]
async fn bonded_by_address(adapter: &Adapter, cached: Option<&str>) -> Vec<Candidate> {
    use btleplug::platform::PeripheralId;
    // `cached` already prefers CLAWDMETER_BLE_ADDRESS over the cache file.
    let mut addrs: Vec<String> = cached.map(str::to_owned).into_iter().collect();
    if addrs.iter().all(|a| parse_mac(a).is_none()) {
        match tokio::task::spawn_blocking(windows_pnp_address).await {
            Ok(Some(mac)) => {
                log_repeated(Level::Info, &format!("ble: Windows PnP table lists a bonded {DEVICE_NAME} at {mac}"));
                addrs.push(mac);
            }
            _ => log_repeated(Level::Info, &format!("ble: no bonded {DEVICE_NAME} in the Windows PnP table")),
        }
    }
    let mut out: Vec<Candidate> = Vec::new();
    for addr in addrs.iter().filter_map(|a| parse_mac(a)) {
        if out.iter().any(|c| c.key.eq_ignore_ascii_case(&addr.to_string())) {
            continue;
        }
        match adapter.add_peripheral(&PeripheralId::from(addr)).await {
            Ok(p) => out.push(Candidate {
                key: addr.to_string(),
                peripheral: p,
                name: Some(DEVICE_NAME.into()),
                has_service: false,
                found_by: "bonded-address",
            }),
            Err(e) => log_repeated(Level::Warn, &format!("ble: bonded address {addr} unreachable: {e}")),
        }
    }
    out
}

/// [`windows_pnp_lookup`], remembered for 10 minutes: each lookup starts a
/// PowerShell (about a second of CPU), and the table rarely changes.
#[cfg(windows)]
fn windows_pnp_address() -> Option<String> {
    static LAST: Mutex<Option<(Instant, Option<String>)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, mac)) = last.as_ref() {
        if at.elapsed() < Duration::from_secs(600) {
            return mac.clone();
        }
    }
    let mac = windows_pnp_lookup();
    *last = Some((Instant::now(), mac.clone()));
    mac
}

/// The bonded Clawdmeter's MAC from the Windows PnP table (the Windows
/// daemon's `Get-PnpDevice` lookup). `None` on any failure.
#[cfg(windows)]
fn windows_pnp_lookup() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "Get-PnpDevice -Class Bluetooth -ErrorAction SilentlyContinue | \
         Where-Object {{ $_.FriendlyName -eq '{DEVICE_NAME}' }} | Select-Object -ExpandProperty InstanceId"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .inspect_err(|e| log::warn!("ble: Windows PnP lookup could not run PowerShell: {e}"))
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(mac_from_pnp_instance_id)
}

/// macOS: forget a stale bond for the device with `blueutil` (CoreBluetooth
/// has no unpair API). The firmware pairs "just works", so the next connect
/// re-bonds with no prompt. Logs a manual fix when blueutil is missing or
/// lacks Bluetooth permission (it hangs then, hence the timeout).
fn macos_unpair_stale_bond() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let run = |args: &[&str]| -> Option<String> {
        let mut child = std::process::Command::new("blueutil")
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(st)) if st.success() => break,
                Ok(Some(_)) | Err(_) => return None,
                Ok(None) if start.elapsed() > Duration::from_secs(8) => {
                    let _ = child.kill();
                    log::warn!(
                        "blueutil timed out; it likely lacks Bluetooth permission (System Settings > \
                         Privacy & Security > Bluetooth)"
                    );
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        let mut s = String::new();
        io::Read::read_to_string(&mut child.stdout.take()?, &mut s).ok()?;
        Some(s)
    };
    let Some(paired) = run(&["--paired"]) else {
        log::warn!(
            "can't clear the stale bond automatically: install blueutil (`brew install blueutil`), or forget \
             '{DEVICE_NAME}' in System Settings > Bluetooth and connect again"
        );
        return;
    };
    match blueutil_paired_address(&paired, DEVICE_NAME) {
        Some(addr) if run(&["--unpair", &addr]).is_some() => {
            log::info!("unpaired the stale bond for '{DEVICE_NAME}' [{addr}]; it re-pairs on the next connect")
        }
        Some(addr) => log::warn!("blueutil could not unpair {addr}"),
        None => log::info!("no paired '{DEVICE_NAME}' to unpair"),
    }
}

fn spawn_event_forwarder(
    mut stream: Pin<Box<dyn Stream<Item = btleplug::api::ValueNotification> + Send>>,
    tx: mpsc::UnboundedSender<DeviceEvent>,
    key: String,
) {
    tokio::spawn(async move {
        while let Some(n) = stream.next().await {
            if let Some(ev) = device_event(n.uuid, &n.value) {
                if tx.send(ev).is_err() {
                    return;
                }
            }
        }
        // The stack closed it: a timestamp for the drop.
        log::info!("ble: notification stream from {key} ended");
    });
}

/// Forget a device in BlueZ (unpair). Not called by the daemon loop: doing it
/// on connect failures would strand a device that is only ever found among
/// paired devices. Exposed for an explicit "Forget device" action.
#[cfg(target_os = "linux")]
pub async fn forget_device(link_id: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || bluez::remove_device(&link_id)).await.map_err(|e| e.to_string())?
}

#[cfg(target_os = "linux")]
mod bluez {
    //! Two BlueZ D-Bus calls btleplug doesn't expose.
    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
    use dbus::blocking::Connection;
    use std::time::Duration;

    fn object_path(id: &str) -> String {
        if id.starts_with('/') {
            id.to_owned()
        } else {
            format!("/org/bluez/{id}")
        }
    }

    /// `org.bluez.Device1.Paired` for a btleplug id (`hci0/dev_...`).
    pub fn is_paired(id: &str) -> Result<bool, String> {
        let conn = Connection::new_system().map_err(|e| e.to_string())?;
        let path = object_path(id);
        let proxy = conn.with_proxy("org.bluez", path, Duration::from_secs(5));
        proxy.get::<bool>("org.bluez.Device1", "Paired").map_err(|e| e.to_string())
    }

    /// `org.bluez.Device1.Trusted = true` (`bluetoothctl trust`).
    pub fn trust(id: &str) -> Result<(), String> {
        use dbus::blocking::stdintf::org_freedesktop_dbus::Properties as _;
        let conn = Connection::new_system().map_err(|e| e.to_string())?;
        let proxy = conn.with_proxy("org.bluez", object_path(id), Duration::from_secs(5));
        if proxy.get::<bool>("org.bluez.Device1", "Trusted").unwrap_or(false) {
            return Ok(());
        }
        proxy.set("org.bluez.Device1", "Trusted", true).map_err(|e| e.to_string())
    }

    /// `org.bluez.Adapter1.RemoveDevice` on the device's adapter.
    pub fn remove_device(id: &str) -> Result<(), String> {
        let conn = Connection::new_system().map_err(|e| e.to_string())?;
        let path = object_path(id);
        let adapter = path.rsplit_once('/').map(|(a, _)| a.to_owned()).ok_or("bad device path")?;
        let proxy = conn.with_proxy("org.bluez", adapter, Duration::from_secs(5));
        let dev = dbus::Path::new(path).map_err(|e| e.to_string())?;
        proxy.method_call::<(), _, _, _>("org.bluez.Adapter1", "RemoveDevice", (dev,)).map_err(|e| e.to_string())
    }

    /// What a throwaway connection finds when it asks `org.bluez` for its
    /// object tree.
    pub enum Reach {
        /// `GetManagedObjects` answered: bus and bluetoothd are up, so a
        /// call that failed on the manager's connection lost that
        /// connection.
        Reached,
        /// A fresh connection opened but `org.bluez` failed the call:
        /// bluetoothd stopped or hung, on a live bus — a new connection
        /// to the same bus would not help.
        BluetoothUnreachable,
        /// Even the connection failed: the system bus is gone, and every
        /// connection on it with it.
        BusUnreachable,
    }

    /// `org.bluez`'s object tree over a fresh system-bus connection — the
    /// same call `Manager::adapters` makes, so it answers whether a new
    /// connection would fare better.
    pub fn reach() -> Reach {
        use dbus::blocking::stdintf::org_freedesktop_dbus::ObjectManager;
        let Ok(conn) = Connection::new_system() else {
            return Reach::BusUnreachable;
        };
        let proxy = conn.with_proxy("org.bluez", "/", Duration::from_secs(5));
        if proxy.get_managed_objects().is_ok() {
            Reach::Reached
        } else {
            Reach::BluetoothUnreachable
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn paths() {
            assert_eq!(super::object_path("hci0/dev_AA_BB"), "/org/bluez/hci0/dev_AA_BB");
            assert_eq!(super::object_path("/org/bluez/hci1/dev_X"), "/org/bluez/hci1/dev_X");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn reach_results() {
        use bluez::Reach::*;
        assert!(lost_on(Reached));
        assert!(lost_on(BusUnreachable));
        assert!(!lost_on(BluetoothUnreachable));
    }

    #[test]
    fn device_events() {
        assert_eq!(device_event(REQ_UUID, &[1]), Some(DeviceEvent::Refresh));
        assert_eq!(device_event(TX_UUID, br#"{"dismiss":true}"#), Some(DeviceEvent::Dismissed));
        assert_eq!(device_event(TX_UUID, br#"{ "dismiss": true }"#), Some(DeviceEvent::Dismissed));
        assert_eq!(device_event(TX_UUID, br#"{"ack":true}"#), None);
        assert_eq!(device_event(TX_UUID, br#"{"err":true}"#), None);
        assert_eq!(device_event(TX_UUID, &[0xff, 0xfe]), None);
        assert_eq!(device_event(RX_UUID, br#"{"dismiss":true}"#), None);
        assert_eq!(
            device_event(TX_UUID, br#"{"wifi":{"st":"connecting","ssid":"Mi casa"}}"#),
            Some(DeviceEvent::Wifi(WifiStatus { st: "connecting".into(), ip: None, ssid: Some("Mi casa".into()) }))
        );
        assert_eq!(line_event(r#"{"req":true}"#), Some(DeviceEvent::Refresh));
        assert_eq!(line_event(r#"{"auth":"ok"}"#), None);
        // The firmware's reply, as written.
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/src/ble.cpp"),
        )
        .unwrap();
        assert!(src.contains(r#"tx_char->setValue("{\"dismiss\":true}");"#));
    }

    #[test]
    fn uuids_match_firmware() {
        assert_eq!(SERVICE_UUID.to_string(), "4c41555a-4465-7669-6365-000000000001");
        assert_eq!(RX_UUID.to_string(), "4c41555a-4465-7669-6365-000000000002");
        assert_eq!(TX_UUID.to_string(), "4c41555a-4465-7669-6365-000000000003");
        assert_eq!(REQ_UUID.to_string(), "4c41555a-4465-7669-6365-000000000004");
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/src/ble.cpp"),
        );
        if let Ok(src) = src {
            for u in [SERVICE_UUID, RX_UUID, TX_UUID, REQ_UUID] {
                assert!(src.contains(&u.to_string()), "{u} not in firmware/src/ble.cpp");
            }
            assert!(src.contains("BLE_BUF_SIZE 512"), "RX buffer size changed; revisit MAX_WRITE");
        }
    }

    #[test]
    fn address_cache() {
        let dir = std::env::temp_dir().join(format!("cm-ble-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let c = AddressCache::new(dir.join("ble-address"));
        assert_eq!(c.load(), None);
        c.save("AA:BB:CC:DD:EE:FF").unwrap();
        assert_eq!(c.load().as_deref(), Some("AA:BB:CC:DD:EE:FF"));
        c.invalidate("11:22:33:44:55:66");
        assert!(c.load().is_some(), "only drops the matching key");
        c.invalidate("aa:bb:cc:dd:ee:ff");
        assert_eq!(c.load(), None);
        fs::write(dir.join("ble-address"), "AA:BB:zz\n").unwrap();
        assert_eq!(c.load(), None, "malformed MAC discarded");
        assert!(!dir.join("ble-address").exists());
        c.save("6F1C2E3A-0000-4000-8000-123456789ABC").unwrap();
        assert_eq!(c.load().as_deref(), Some("6F1C2E3A-0000-4000-8000-123456789ABC"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pnp_instance_ids() {
        assert_eq!(
            mac_from_pnp_instance_id(r"BTHLE\DEV_98A316A5D706\7&B8081D1&0&98A316A5D706").as_deref(),
            Some("98:A3:16:A5:D7:06")
        );
        assert_eq!(mac_from_pnp_instance_id(r"BTHLE\dev_98a316a5d706\x").as_deref(), Some("98:A3:16:A5:D7:06"));
        assert_eq!(mac_from_pnp_instance_id(r"BTHLE\SOMETHING\7&B8081D1"), None);
        assert_eq!(mac_from_pnp_instance_id(""), None);
        // A short hex run after DEV_ is not a MAC; neither is a 13-digit one.
        assert_eq!(mac_from_pnp_instance_id(r"BTHENUM\DEV_1234\x"), None);
        assert_eq!(mac_from_pnp_instance_id(r"BTHLE\DEV_98A316A5D7061\x"), None);
        assert_eq!(parse_mac("98:a3:16:a5:d7:06").map(|a| a.to_string()).as_deref(), Some("98:A3:16:A5:D7:06"));
        assert_eq!(parse_mac("6F1C2E3A-0000-4000-8000-123456789ABC"), None);
    }

    // Ported from test_windows_reconnect.py's zombie-link tests.
    #[test]
    fn zombie_breaker() {
        let h = WriteHealth::default();
        assert!(!h.is_zombie());
        h.record(false);
        assert!(!h.is_zombie(), "one transient failure is ridden out");
        h.record(true);
        h.record(false);
        assert!(!h.is_zombie(), "a success resets the count");
        h.record(false);
        assert!(h.is_zombie());
    }

    #[test]
    fn repeated_lines_are_held_back() {
        let mut r = Repeats { seen: Vec::new() };
        let t = Instant::now();
        let min = |m: u64| t + Duration::from_secs(m * 60);
        assert_eq!(r.admit("a", t), Some((0, Duration::ZERO)), "the first is logged");
        assert_eq!(r.admit("a", min(1)), None);
        assert_eq!(r.admit("a", min(9)), None);
        assert_eq!(r.admit("b", min(9)), Some((0, Duration::ZERO)), "another line is logged");
        assert_eq!(r.admit("a", min(10)), Some((2, Duration::from_secs(600))), "then the count");
        assert_eq!(r.admit("a", min(11)), None, "and the window starts over");
        assert_eq!(r.admit("a", min(25)), Some((1, Duration::from_secs(900))));
    }

    #[test]
    fn stale_bond_pattern() {
        let mut s = ShortLinks { at: Vec::new(), warned: None };
        let t = Instant::now();
        let sec = |n: u64| t + Duration::from_secs(n);
        assert_eq!(s.record(sec(0)), None);
        assert_eq!(s.record(sec(20)), None);
        assert_eq!(s.record(sec(40)), Some(3), "three short links in 5 min");
        assert_eq!(s.record(sec(60)), None, "one warning per window");
        // Spread out, they don't add up.
        let mut s = ShortLinks { at: Vec::new(), warned: None };
        assert_eq!(s.record(sec(0)), None);
        assert_eq!(s.record(sec(200)), None);
        assert_eq!(s.record(sec(400)), None);
        assert_eq!(s.record(sec(450)), Some(3));
    }

    // The macOS daemon's stale-bond self-heal (unpair_macos).
    #[test]
    fn stale_bond_helpers() {
        assert!(is_encryption_error("Error Domain=CBErrorDomain Code=15 \"Failed to encrypt the connection\""));
        assert!(is_encryption_error("Failed to encrypt"));
        assert!(!is_encryption_error("timed out during connect"));
        let out = "address: 11-22-33-44-55-66, connected (master, -50 dBm), not favourite, paired, name: \"Keyboard\", recent access date: 2026\n\
                   address: 28-84-85-55-5c-3d, not connected, not favourite, paired, name: \"Clawdmeter\", recent access date: 2026\n";
        assert_eq!(blueutil_paired_address(out, "Clawdmeter").as_deref(), Some("28-84-85-55-5c-3d"));
        assert_eq!(blueutil_paired_address(out, "Other"), None);
    }

    #[test]
    fn acceptance_rules() {
        assert!(accept(None, true));
        assert!(accept(Some("Clawdmeter"), false));
        assert!(!accept(Some("Keychron K2"), false));
        assert!(!accept(Some("clawdmeter"), false));
        assert!(!accept(None, false));
    }
}
