//! The links the daemon talks to the device over: BLE ([`DeviceLink`]), TCP
//! on the local network ([`TcpLink`]) or the USB serial port ([`UsbLink`]);
//! the last two need the fork firmware. All carry the same JSON lines and
//! deliver the same [`DeviceEvent`]s, so the daemon's link logic doesn't care
//! which one it holds.
//!
//! Finding the device on the network: `wifi_host` when set, else the last
//! address that worked (`wifi-address` in the state dir), else mDNS
//! (`_clawdmeter._tcp`, TXT `id` = the BLE device key).

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clawdmeter_ble::{line_event, AddressCache, DeviceEvents, DeviceLink, WriteHealth};
use clawdmeter_core::wifi::{
    auth_line, auth_mac, hello_line, is_key, parse_device_line, same_secret, DeviceLine, Secret, Transport, APP_LABEL,
    DEVICE_LABEL, HEARTBEAT_LINE, MDNS_SERVICE, PORT,
};
use clawdmeter_flash::device::LinkPort;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};

/// Bound on the TCP connect and on the device's auth reply (the device
/// itself gives the client 3 s to authenticate).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// Bound on one write.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an mDNS browse waits for the device to answer.
const BROWSE_TIME: Duration = Duration::from_secs(3);

/// An open link of either kind.
#[derive(Debug)]
pub enum Link {
    /// Bluetooth LE.
    Ble(DeviceLink),
    /// TCP over the local network.
    Wifi(TcpLink),
    /// The USB serial port.
    Usb(UsbLink),
}

impl Link {
    /// Which kind.
    pub fn transport(&self) -> Transport {
        match self {
            Link::Ble(_) => Transport::Ble,
            Link::Wifi(_) => Transport::Wifi,
            Link::Usb(_) => Transport::Usb,
        }
    }

    /// Write one JSON line (at most `MAX_WRITE` bytes, no newline).
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        match self {
            Link::Ble(l) => l.write(data).await.map_err(|e| e.to_string()),
            Link::Wifi(l) => l.write(data).await.map_err(|e| e.to_string()),
            Link::Usb(l) => l.write(data).map_err(|e| e.to_string()),
        }
    }

    /// Still up?
    pub async fn is_connected(&self) -> bool {
        match self {
            Link::Ble(l) => l.is_connected().await,
            Link::Wifi(l) => l.is_connected(),
            Link::Usb(l) => l.is_connected(),
        }
    }

    /// Close it.
    pub async fn close(&self) {
        match self {
            Link::Ble(l) => l.close().await,
            Link::Wifi(l) => l.close().await,
            Link::Usb(l) => l.close(),
        }
    }
}

/// An authenticated TCP connection to the device.
pub struct TcpLink {
    writer: Mutex<OwnedWriteHalf>,
    addr: SocketAddr,
    alive: Arc<AtomicBool>,
    health: WriteHealth,
}

impl std::fmt::Debug for TcpLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpLink").field("addr", &self.addr).finish()
    }
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, format!("timed out during {what}"))
}

impl TcpLink {
    /// Connect and run the mutual handshake with `key`: the device proves it
    /// knows the key before the app answers, and the key never goes on the
    /// wire. The device's lines after `{"auth":"ok"}` arrive as events until
    /// it closes the connection.
    pub async fn open(addr: SocketAddr, key: &Secret) -> io::Result<(TcpLink, DeviceEvents)> {
        let denied = |why: &str| io::Error::new(io::ErrorKind::PermissionDenied, why.to_owned());
        let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| timed_out("connect"))??;
        stream.set_nodelay(true)?;
        let (read, mut write) = stream.into_split();
        let app_nonce = new_key()?.0;
        write.write_all(format!("{}\n", hello_line(&app_nonce)).as_bytes()).await?;
        let mut lines = BufReader::new(read).lines();
        let reply =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line()).await.map_err(|_| timed_out("hello"))??;
        let Some(DeviceLine::Challenge { nonce, proof }) = reply.as_deref().and_then(parse_device_line) else {
            return Err(denied("the device didn't answer the handshake (update its firmware)"));
        };
        let want = auth_mac(key, DEVICE_LABEL, &app_nonce, &nonce);
        if !is_key(&nonce) || !same_secret(&proof.to_ascii_lowercase(), &want) {
            return Err(denied("the device doesn't know the key: not trusting it"));
        }
        let mac = auth_mac(key, APP_LABEL, &app_nonce, &nonce);
        write.write_all(format!("{}\n", auth_line(&mac)).as_bytes()).await?;
        let reply =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, lines.next_line()).await.map_err(|_| timed_out("auth"))??;
        if reply.as_deref().and_then(parse_device_line) != Some(DeviceLine::AuthOk) {
            return Err(denied("the device refused the key"));
        }
        let (tx, events) = mpsc::unbounded_channel();
        let alive = Arc::new(AtomicBool::new(true));
        let up = alive.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ev) = line_event(&line) {
                    if tx.send(ev).is_err() {
                        break;
                    }
                }
            }
            up.store(false, Ordering::Relaxed);
        });
        Ok((TcpLink { writer: Mutex::new(write), addr, alive, health: WriteHealth::default() }, events))
    }

    /// Write one line (the newline is added here).
    pub async fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut line = Vec::with_capacity(data.len() + 1);
        line.extend_from_slice(data);
        line.push(b'\n');
        let r = match tokio::time::timeout(WRITE_TIMEOUT, async { self.writer.lock().await.write_all(&line).await })
            .await
        {
            Ok(r) => r,
            Err(_) => Err(timed_out("write")),
        };
        self.health.record(r.is_ok());
        r
    }

    /// The device hasn't closed the connection and writes go through.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && !self.health.is_zombie()
    }

    /// Close the connection.
    pub async fn close(&self) {
        let _ = self.writer.lock().await.shutdown().await;
    }

    /// The device's address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// The board on the USB serial port (fork firmware): JSON lines both ways,
/// no auth (the cable is the trust). A blocking thread reads the port; the
/// firmware's own log lines are skipped like any unreadable line.
pub struct UsbLink {
    writer: std::sync::Mutex<LinkPort>,
    port: String,
    alive: Arc<AtomicBool>,
    health: WriteHealth,
}

impl std::fmt::Debug for UsbLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsbLink").field("port", &self.port).finish()
    }
}

/// The firmware's reply to a line it took.
fn is_ack(line: &str) -> bool {
    line.trim() == r#"{"ack":true}"#
}

/// Complete lines from a byte stream; a partial line waits in `pending`.
fn take_lines(pending: &mut Vec<u8>, data: &[u8]) -> Vec<String> {
    pending.extend_from_slice(data);
    let mut out = Vec::new();
    while let Some(i) = pending.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = pending.drain(..=i).collect();
        out.push(String::from_utf8_lossy(&line).trim().to_owned());
    }
    // A runaway log line with no newline: drop it rather than grow forever.
    if pending.len() > 4096 {
        pending.clear();
    }
    out
}

impl UsbLink {
    /// Open `port` and check the firmware speaks the USB link: it must ack a
    /// keepalive within the handshake time (older firmware ignores the line).
    pub async fn open(port: String) -> io::Result<(UsbLink, DeviceEvents)> {
        tokio::task::spawn_blocking(move || UsbLink::open_blocking(port)).await.map_err(io::Error::other)?
    }

    fn open_blocking(port: String) -> io::Result<(UsbLink, DeviceEvents)> {
        let (mut reader, mut writer) =
            clawdmeter_flash::device::open_link(&port).map_err(|e| io::Error::other(e.to_string()))?;
        // The leading newline ends whatever the port held before.
        writer.write_all(format!("\n{HEARTBEAT_LINE}\n").as_bytes())?;
        writer.flush()?;
        let (tx, events) = mpsc::unbounded_channel();
        let mut pending = Vec::new();
        let mut buf = [0u8; 512];
        let end = Instant::now() + HANDSHAKE_TIMEOUT;
        let mut acked = false;
        while !acked && Instant::now() < end {
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
                Err(e) => return Err(e),
            };
            for line in take_lines(&mut pending, &buf[..n]) {
                // What a new host gets before the ack (WiFi status, a refresh
                // request) is kept for the daemon.
                if is_ack(&line) {
                    acked = true;
                } else if let Some(ev) = line_event(&line) {
                    let _ = tx.send(ev);
                }
            }
        }
        if !acked {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the board's firmware doesn't answer on USB (update it)",
            ));
        }
        let alive = Arc::new(AtomicBool::new(true));
        let up = alive.clone();
        std::thread::Builder::new().name("clawdmeter-usb".into()).spawn(move || {
            while up.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        for line in take_lines(&mut pending, &buf[..n]) {
                            if let Some(ev) = line_event(&line) {
                                if tx.send(ev).is_err() {
                                    up.store(false, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                    Err(_) => break, // unplugged
                }
            }
            up.store(false, Ordering::Relaxed);
        })?;
        Ok((UsbLink { writer: std::sync::Mutex::new(writer), port, alive, health: WriteHealth::default() }, events))
    }

    /// Write one line (the newline is added here). Short and bounded by the
    /// port's timeout, so it runs inline.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut line = Vec::with_capacity(data.len() + 1);
        line.extend_from_slice(data);
        line.push(b'\n');
        let r = match self.writer.lock() {
            Ok(mut w) => w.write_all(&line).and_then(|()| w.flush()),
            Err(_) => Err(io::Error::other("USB writer poisoned")),
        };
        self.health.record(r.is_ok());
        r
    }

    /// The port is still there and writes go through.
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && !self.health.is_zombie()
    }

    /// Let go of the port: the reader stops within one read timeout (100 ms)
    /// and the port closes once the link is dropped.
    pub fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
    }

    /// The serial port's name.
    pub fn port(&self) -> &str {
        &self.port
    }
}

/// `wifi_host` as a socket address: `host` or `host:port` (default port
/// 7363), resolved.
async fn host_addr(host: &str) -> io::Result<SocketAddr> {
    let target = if host.contains(':') { host.to_owned() } else { format!("{host}:{PORT}") };
    tokio::net::lookup_host(target)
        .await?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "wifi_host doesn't resolve"))
}

/// Open the WiFi link: `host` (the `wifi_host` override) when set, else the
/// cached address of the device `device_key`, else an mDNS browse for it.
/// An address found by mDNS is cached for the next time.
pub async fn connect_wifi(
    host: Option<String>,
    device_key: Option<String>,
    cache: AddressCache,
    key: Secret,
) -> io::Result<(TcpLink, DeviceEvents)> {
    if let Some(h) = host {
        return TcpLink::open(host_addr(&h).await?, &key).await;
    }
    let id = device_key.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no device paired yet"))?;
    if let Some(ip) = cache.load().and_then(|s| s.parse::<IpAddr>().ok()) {
        if let Ok(ok) = TcpLink::open(SocketAddr::new(ip, PORT), &key).await {
            return Ok(ok);
        }
    }
    let found = tokio::task::spawn_blocking(move || browse(&id, BROWSE_TIME)).await.map_err(io::Error::other)?;
    let addr = found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "device not found on the network"))?;
    let ok = TcpLink::open(addr, &key).await?;
    let _ = cache.save(&addr.ip().to_string());
    Ok(ok)
}

/// Browse `_clawdmeter._tcp` for up to `wait` and return the IPv4 address
/// of the instance whose TXT `id` is `device_key`; any other unit on the
/// network is ignored.
fn browse(device_key: &str, wait: Duration) -> Option<SocketAddr> {
    let mdns = mdns_sd::ServiceDaemon::new().ok()?;
    let rx = mdns.browse(MDNS_SERVICE).ok()?;
    let end = Instant::now() + wait;
    let mut found = None;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(s)) => {
                if s.get_property_val_str("id").is_some_and(|id| id.trim().eq_ignore_ascii_case(device_key)) {
                    found = s.get_addresses_v4().into_iter().next().map(|ip| SocketAddr::new(ip.into(), s.port));
                    if found.is_some() {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = mdns.shutdown();
    found
}

/// A fresh random key for the device (32 hex characters).
pub fn new_key() -> io::Result<Secret> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(Secret(b.iter().map(|x| format!("{x:02x}")).collect()))
}

/// The WiFi network this computer is on, from `netsh wlan show interfaces`
/// (Windows only; `None` elsewhere or when not on WiFi).
pub fn current_ssid() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let out = std::process::Command::new("netsh")
            .args(["wlan", "show", "interfaces"])
            .stdin(std::process::Stdio::null())
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .ok()?;
        ssid_from_netsh(&String::from_utf8_lossy(&out.stdout))
    }
    #[cfg(not(windows))]
    None
}

/// The `SSID : name` line of `netsh wlan show interfaces` (not `BSSID`).
pub fn ssid_from_netsh(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        let v = v.trim();
        (k.trim() == "SSID" && !v.is_empty()).then(|| v.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdmeter_ble::{DeviceEvent, WifiStatus};
    use tokio::net::TcpListener;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    /// The firmware's side of the handshake with `key`; false when the app
    /// fails it (the firmware then just closes).
    async fn device_handshake(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
        w: &mut OwnedWriteHalf,
        key: &str,
    ) -> bool {
        let Ok(Some(hello)) = lines.next_line().await else { return false };
        let v: serde_json::Value = serde_json::from_str(&hello).unwrap();
        let cn = v["hello"].as_str().unwrap().to_owned();
        let dn = "ffeeddccbbaa99887766554433221100";
        let key = Secret(key.into());
        let proof = auth_mac(&key, DEVICE_LABEL, &cn, dn);
        w.write_all(format!("{{\"nonce\":\"{dn}\",\"proof\":\"{proof}\"}}\n").as_bytes()).await.unwrap();
        let Ok(Some(auth)) = lines.next_line().await else { return false };
        auth == auth_line(&auth_mac(&key, APP_LABEL, &cn, dn))
    }

    /// A device on 127.0.0.1: runs the handshake, asks for data, reports its
    /// WiFi, then hands every line it gets to the test.
    async fn fake_device() -> (SocketAddr, mpsc::UnboundedReceiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let (r, mut w) = sock.into_split();
                    let mut lines = BufReader::new(r).lines();
                    if !device_handshake(&mut lines, &mut w, KEY).await {
                        return;
                    }
                    w.write_all(b"{\"auth\":\"ok\"}\n{\"req\":true}\n").await.unwrap();
                    w.write_all(b"{\"wifi\":{\"st\":\"ok\",\"ip\":\"127.0.0.1\",\"ssid\":\"Casa\"}}\n").await.unwrap();
                    while let Ok(Some(l)) = lines.next_line().await {
                        w.write_all(b"{\"ack\":true}\n").await.unwrap();
                        let _ = tx.send(l);
                    }
                });
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn handshake_and_framing() {
        let (addr, mut got) = fake_device().await;
        let (link, mut events) = TcpLink::open(addr, &Secret(KEY.to_uppercase())).await.unwrap();
        assert_eq!(events.recv().await, Some(DeviceEvent::Refresh));
        assert_eq!(
            events.recv().await,
            Some(DeviceEvent::Wifi(WifiStatus {
                st: "ok".into(),
                ip: Some("127.0.0.1".into()),
                ssid: Some("Casa".into())
            }))
        );
        let link = Link::Wifi(link);
        assert_eq!(link.transport(), Transport::Wifi);
        link.write(br#"{"s":3,"ok":true}"#).await.unwrap();
        link.write(br#"{"ag":[]}"#).await.unwrap();
        assert_eq!(got.recv().await.as_deref(), Some(r#"{"s":3,"ok":true}"#), "one line per write");
        assert_eq!(got.recv().await.as_deref(), Some(r#"{"ag":[]}"#));
        assert!(link.is_connected().await);
        link.close().await;
    }

    #[tokio::test]
    async fn wrong_key_is_refused() {
        let (addr, _got) = fake_device().await;
        let err = TcpLink::open(addr, &Secret("f".repeat(32))).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn an_impostor_is_not_trusted() {
        // Something on the network that just says yes, without the key.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 128];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let hello = String::from_utf8_lossy(&buf);
            assert!(!hello.contains(KEY), "the key never goes on the wire");
            sock.write_all(b"{\"auth\":\"ok\"}\n{\"key\":\"space\",\"down\":true}\n").await.unwrap();
            let fake = format!("{{\"nonce\":\"{}\",\"proof\":\"{}\"}}\n", "a".repeat(32), "b".repeat(64));
            let _ = sock.write_all(fake.as_bytes()).await;
        });
        let err = TcpLink::open(addr, &Secret(KEY.into())).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn closed_by_the_device() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            assert!(device_handshake(&mut lines, &mut w, KEY).await);
            w.write_all(b"{\"auth\":\"ok\"}\n").await.unwrap();
            // and hangs up
        });
        let (link, mut events) = TcpLink::open(addr, &Secret(KEY.into())).await.unwrap();
        assert_eq!(events.recv().await, None, "the event stream ends with the connection");
        assert!(!link.is_connected());
    }

    #[tokio::test]
    async fn wifi_host_override_and_cache() {
        let (addr, _got) = fake_device().await;
        let dir = std::env::temp_dir().join(format!("cm-wifi-{}", std::process::id()));
        let cache = AddressCache::new(dir.join("wifi-address"));
        // The override wins, with or without a port.
        let (l, _) = connect_wifi(Some(addr.to_string()), None, cache.clone(), Secret(KEY.into())).await.unwrap();
        assert_eq!(l.addr(), addr);
        // No override and no paired device: nothing to look for.
        let err = connect_wifi(None, None, cache, Secret(KEY.into())).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn usb_lines_split_across_reads() {
        let mut pending = Vec::new();
        assert!(take_lines(&mut pending, b"BLE: adverti").is_empty());
        assert_eq!(
            take_lines(&mut pending, b"sing\r\n{\"ack\":true}\n{\"req\""),
            ["BLE: advertising", r#"{"ack":true}"#]
        );
        assert_eq!(take_lines(&mut pending, b":true}\n"), [r#"{"req":true}"#]);
        assert!(pending.is_empty());
        assert!(is_ack(r#"{"ack":true}"#) && !is_ack(r#"{"err":true}"#));
        assert_eq!(
            line_event(r#"{"key":"space","down":true}"#),
            Some(DeviceEvent::Key { name: "space".into(), down: true })
        );
    }

    #[test]
    fn keys_and_ssids() {
        let a = new_key().unwrap();
        assert!(clawdmeter_core::wifi::is_key(&a.0));
        assert_ne!(a, new_key().unwrap());
        let netsh = "There is 1 interface on the system:\r\n\r\n    Name                   : Wi-Fi\r\n    \
                     State                  : connected\r\n    SSID                   : Mi casa 5G\r\n    \
                     BSSID                  : aa:bb:cc:dd:ee:ff\r\n";
        assert_eq!(ssid_from_netsh(netsh).as_deref(), Some("Mi casa 5G"));
        assert_eq!(ssid_from_netsh("    State : disconnected\r\n"), None);
    }
}
