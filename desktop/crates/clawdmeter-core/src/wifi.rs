//! The WiFi/LAN link's I/O-free side (fork only, see docs/protocol.md
//! "WiFi transport"): the lines the app sends to provision and switch the
//! device's radio, the lines the device sends back, and which transport to
//! try next.
//!
//! The device is provisioned over BLE with a shared 32-hex key; it then
//! advertises `_clawdmeter._tcp` over mDNS and takes the same JSON lines
//! over TCP after a `{"auth":"<key>"}` handshake.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The device's TCP port.
pub const PORT: u16 = 7363;
/// The mDNS service type the device advertises.
pub const MDNS_SERVICE: &str = "_clawdmeter._tcp.local.";
/// The keys these lines add to the firmware's JSON parser, on top of the
/// usage payload's (`auth` from the TCP handshake, `wifi` from provisioning).
pub const TRANSPORT_KEYS: [&str; 2] = ["auth", "wifi"];

/// A password or key: kept out of `Debug` output, so it never reaches a log.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// `connection`: which link the app uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Connection {
    /// Bluetooth only, and the device's WiFi radio off (default).
    #[default]
    Ble,
    /// WiFi when the device is reachable on the network, else Bluetooth.
    Wifi,
}

impl Connection {
    /// Parse a config value; anything unknown is Bluetooth.
    pub fn parse(v: &str) -> Self {
        if v.trim().eq_ignore_ascii_case("wifi") {
            Connection::Wifi
        } else {
            Connection::Ble
        }
    }

    /// The config-file spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Connection::Ble => "ble",
            Connection::Wifi => "wifi",
        }
    }
}

/// A link the app talks to the device over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Bluetooth LE.
    Ble,
    /// TCP over the local network.
    Wifi,
    /// JSON lines over the USB serial port (fork firmware).
    Usb,
}

impl Transport {
    /// `app-status.json` spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Ble => "ble",
            Transport::Wifi => "wifi",
            Transport::Usb => "usb",
        }
    }
}

/// The device's WiFi state, as it reports it (`{"wifi":{...}}`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WifiStatus {
    /// `off` | `connecting` | `ok` | `fail`.
    pub st: String,
    /// Address while `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    /// Network name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,
}

/// True for a 32-hex-character key.
pub fn is_key(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Store the network and key on the device and join it (sent over BLE).
pub fn provision_line(ssid: &str, pass: &Secret, key: &Secret) -> String {
    json!({"wifi": {"ssid": ssid, "pass": pass.0, "key": key.0, "en": true}}).to_string()
}

/// Turn the device's radio on or off, keeping its credentials.
pub fn enable_line(on: bool) -> String {
    json!({"wifi": {"en": on}}).to_string()
}

/// Power actions the Device page sends (fork firmware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    /// Rest: the panel goes dark until a press or [`Power::Wake`].
    Sleep,
    /// Leave rest.
    Wake,
    /// Cut the power, like an 8 s PWR hold; only PWR turns it back on.
    Off,
}

impl Power {
    /// The command-file and log spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Power::Sleep => "sleep",
            Power::Wake => "wake",
            Power::Off => "off",
        }
    }

    /// Parse [`Power::as_str`].
    pub fn parse(s: &str) -> Option<Power> {
        [Power::Sleep, Power::Wake, Power::Off].into_iter().find(|p| p.as_str() == s)
    }

    /// The line for the device. The firmware matches these exactly.
    pub fn line(self) -> &'static str {
        match self {
            Power::Sleep => r#"{"power":"sleep"}"#,
            Power::Wake => r#"{"power":"wake"}"#,
            Power::Off => r#"{"power":"off"}"#,
        }
    }
}

/// The TCP handshake's first line.
pub fn auth_line(key: &Secret) -> String {
    json!({"auth": key.0}).to_string()
}

/// What one line from the device (a BLE TX notification or a TCP line) means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceLine {
    /// `{"req":true}`: it has no data yet and wants a payload.
    Refresh,
    /// `{"dismiss":true}`: the alert ended on the device.
    Dismissed,
    /// `{"auth":"ok"}`: the TCP handshake passed.
    AuthOk,
    /// Its WiFi state (alone, or in the ack of a `wifi` line).
    Wifi(WifiStatus),
    /// `{"key":"space","down":true}`: a button the app types on this
    /// computer while the link is USB or WiFi (over BLE the device is the
    /// keyboard).
    Key {
        /// `space` | `shift_tab`.
        name: String,
        /// Pressed (true) or released.
        down: bool,
    },
}

/// The app's keepalive on USB, when it has nothing else to send: the
/// firmware counts a USB host as attached while lines keep coming.
pub const HEARTBEAT_LINE: &str = r#"{"hb":1}"#;
/// How often the app writes [`HEARTBEAT_LINE`] on an idle USB link (s). The
/// firmware lets go of a silent USB host after 45 s.
pub const USB_HEARTBEAT_SECS: i64 = 15;

/// Read one device line. Plain acks (`{"ack":true}` / `{"err":true}`) and
/// anything unreadable are `None`.
pub fn parse_device_line(text: &str) -> Option<DeviceLine> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    if v.get("req") == Some(&Value::Bool(true)) {
        return Some(DeviceLine::Refresh);
    }
    if v.get("dismiss") == Some(&Value::Bool(true)) {
        return Some(DeviceLine::Dismissed);
    }
    if v.get("auth").and_then(Value::as_str) == Some("ok") {
        return Some(DeviceLine::AuthOk);
    }
    if let Some(name) = v.get("key").and_then(Value::as_str) {
        let down = v.get("down").and_then(Value::as_bool)?;
        return Some(DeviceLine::Key { name: name.to_owned(), down });
    }
    serde_json::from_value(v.get("wifi")?.clone()).ok().map(DeviceLine::Wifi)
}

/// Which transport to try next, if any: USB, then WiFi, then Bluetooth.
/// `usb`: a board is on a USB port and its retry is due; `wifi_ready`: the
/// user chose WiFi and the device has been provisioned; `current`: the open
/// link.
///
/// USB is tried whenever it's due and the link isn't USB already, so the app
/// moves up to the cable from WiFi or Bluetooth. With no link, WiFi goes
/// next when its retry is due, else Bluetooth when its own is. On Bluetooth,
/// WiFi is probed whenever its retry is due. On WiFi only USB is tried.
pub fn next_attempt(
    usb: bool,
    wifi_ready: bool,
    current: Option<Transport>,
    wifi_due: bool,
    ble_due: bool,
) -> Option<Transport> {
    let wifi = wifi_ready && wifi_due;
    match current {
        Some(Transport::Usb) => None,
        _ if usb => Some(Transport::Usb),
        Some(Transport::Wifi) => None,
        Some(Transport::Ble) => wifi.then_some(Transport::Wifi),
        None if wifi => Some(Transport::Wifi),
        None => ble_due.then_some(Transport::Ble),
    }
}

/// A WiFi link is dropped once WiFi stops being the user's choice.
pub fn keep(wifi_ready: bool, current: Transport) -> bool {
    current != Transport::Wifi || wifi_ready
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_match_the_protocol() {
        let key = Secret("0123456789abcdef0123456789ABCDEF".into());
        assert_eq!(
            provision_line("Casa \"2\"", &Secret("p@ss".into()), &key),
            r#"{"wifi":{"ssid":"Casa \"2\"","pass":"p@ss","key":"0123456789abcdef0123456789ABCDEF","en":true}}"#
        );
        assert_eq!(enable_line(false), r#"{"wifi":{"en":false}}"#);
        assert_eq!(enable_line(true), r#"{"wifi":{"en":true}}"#);
        assert_eq!(auth_line(&key), r#"{"auth":"0123456789abcdef0123456789ABCDEF"}"#);
        assert!(is_key(&key.0));
        assert!(!is_key("0123"));
        assert!(!is_key("g123456789abcdef0123456789abcdef"));
        assert_eq!(format!("{:?}", key), "Secret(***)");
    }

    #[test]
    fn device_lines() {
        assert_eq!(parse_device_line(r#"{"req":true}"#), Some(DeviceLine::Refresh));
        assert_eq!(parse_device_line(r#"{"dismiss":true}"#), Some(DeviceLine::Dismissed));
        assert_eq!(parse_device_line("{\"auth\":\"ok\"}\r\n"), Some(DeviceLine::AuthOk));
        assert_eq!(parse_device_line(r#"{"ack":true}"#), None);
        assert_eq!(parse_device_line(r#"{"err":true}"#), None);
        assert_eq!(parse_device_line("garbage"), None);
        assert_eq!(
            parse_device_line(r#"{"key":"space","down":true}"#),
            Some(DeviceLine::Key { name: "space".into(), down: true })
        );
        assert_eq!(
            parse_device_line(r#"{"key":"shift_tab","down":false}"#),
            Some(DeviceLine::Key { name: "shift_tab".into(), down: false })
        );
        assert_eq!(parse_device_line(r#"{"key":"space"}"#), None, "no direction, no key");
        let ok = WifiStatus { st: "ok".into(), ip: Some("192.168.1.5".into()), ssid: Some("Casa".into()) };
        assert_eq!(
            parse_device_line(r#"{"wifi":{"st":"ok","ip":"192.168.1.5","ssid":"Casa"}}"#),
            Some(DeviceLine::Wifi(ok.clone()))
        );
        assert_eq!(
            parse_device_line(r#"{"ack":true,"wifi":{"st":"ok","ip":"192.168.1.5","ssid":"Casa"}}"#),
            Some(DeviceLine::Wifi(ok))
        );
        assert_eq!(
            parse_device_line(r#"{"wifi":{"st":"off"}}"#),
            Some(DeviceLine::Wifi(WifiStatus { st: "off".into(), ..Default::default() }))
        );
    }

    #[test]
    fn power_lines() {
        for p in [Power::Sleep, Power::Wake, Power::Off] {
            assert_eq!(Power::parse(p.as_str()), Some(p));
            let v: Value = serde_json::from_str(p.line()).unwrap();
            assert_eq!(v["power"], p.as_str(), "the line names the action");
        }
        assert_eq!(Power::parse("reboot"), None);
    }

    #[test]
    fn connection_values() {
        assert_eq!(Connection::parse("WiFi"), Connection::Wifi);
        assert_eq!(Connection::parse("ble"), Connection::Ble);
        assert_eq!(Connection::parse("weird"), Connection::Ble);
        for c in [Connection::Ble, Connection::Wifi] {
            assert_eq!(Connection::parse(c.as_str()), c);
        }
    }

    #[test]
    fn transport_selection() {
        use Transport::*;
        // Bluetooth only: never WiFi.
        assert_eq!(next_attempt(false, false, None, true, true), Some(Ble));
        assert_eq!(next_attempt(false, false, None, true, false), None);
        assert_eq!(next_attempt(false, false, Some(Ble), true, true), None);
        // WiFi first, Bluetooth while WiFi waits out its retry.
        assert_eq!(next_attempt(false, true, None, true, true), Some(Wifi));
        assert_eq!(next_attempt(false, true, None, false, true), Some(Ble));
        assert_eq!(next_attempt(false, true, None, false, false), None);
        // On Bluetooth as a fallback, WiFi is probed when due.
        assert_eq!(next_attempt(false, true, Some(Ble), true, false), Some(Wifi));
        assert_eq!(next_attempt(false, true, Some(Ble), false, true), None);
        // On WiFi nothing else is tried but the cable.
        assert_eq!(next_attempt(false, true, Some(Wifi), true, true), None);
        // The cable beats everything, from any link, whatever the choice.
        assert_eq!(next_attempt(true, false, None, true, true), Some(Usb));
        assert_eq!(next_attempt(true, true, None, true, true), Some(Usb));
        assert_eq!(next_attempt(true, true, Some(Wifi), true, true), Some(Usb));
        assert_eq!(next_attempt(true, false, Some(Ble), true, true), Some(Usb));
        assert_eq!(next_attempt(true, true, Some(Usb), true, true), None);
        assert!(keep(true, Wifi) && keep(false, Ble) && keep(true, Ble) && keep(false, Usb));
        assert!(!keep(false, Wifi), "switching to Bluetooth only drops WiFi");
    }
}
