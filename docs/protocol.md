# BLE protocol

Usage readings and hook events reach the app independently. The app selects
an account, builds a JSON payload, and writes it to the device over Bluetooth
LE. The firmware updates the meters and passes activity to Clawd's director.
Keyboard button reports use the separate HID service.

## Service and characteristics

The device advertises a custom GATT service next to the standard HID keyboard service:

|                                | UUID                                   |
| ------------------------------ | -------------------------------------- |
| **Data service**               | `4c41555a-4465-7669-6365-000000000001` |
| RX characteristic (write)      | `4c41555a-4465-7669-6365-000000000002` |
| TX characteristic (notify)     | `4c41555a-4465-7669-6365-000000000003` |
| REQ characteristic (notify)    | `4c41555a-4465-7669-6365-000000000004` |
| **HID service**                | `00001812-0000-1000-8000-00805f9b34fb` |

The host writes one complete JSON object to RX per write. The app limits
payloads to 500 bytes; the firmware has a 512-byte receive buffer, including
the terminating null byte. The device notifies `0x01` on REQ when it has no
data yet, asking for a poll.
It sends `{"dismiss":true}` on TX when a tap or the device's 2-minute timeout
ends an alert, so the app clears its corresponding alert too. The app expires
an alert nothing cleared after the same 2 minutes (a test alert after 30
seconds) and sends `x` if the device still shows it.

```json
{"s":45,"sr":120,"w":28,"wr":7200,"st":"allowed","acct":"pro","ok":true,"a":2,"e":"p","es":14,"n":1}
```

| Field | Meaning |
| --- | --- |
| `s`, `w` | Session and weekly % used. |
| `sr`, `wr` | Minutes until each resets; `-1` = window not started / unknown. |
| `st` | `allowed`, `limited` or `overage` (stored, not shown). |
| `acct` | `pro`, or `ent` for a spend-capped account: `s` is then the spend %, and `tp` (% of the billing period elapsed), `pd` (period length in days) and `rd` (reset date, e.g. `"Nov 1"`) fill the second card. |
| `t`, `tf` | Local wall-clock epoch and 12/24-hour format, when the clock is on. |
| `c` | `1` = the reset chime may sound. |
| `ok` | `false` alone = no usable account data; do not treat this as a zero reading. |
| `b`, `m`, `x` | Attention: alert chime, message (ASCII), end the alert. The app sends a fixed line per alert kind in `m`: `Question for you`, `Permission needed!` (also for other notification types), `Waiting for you!` (idle) or `MCP request!`; the test alert sends `Clawdmeter test alert`. The firmware shows whatever `m` it gets. |
| `a`, `e`, `es`, `n` | Fast clock: activity 0–3, latest hook event (`p` prompt, `s` stop, `b` back after a gap) and its sequence number, active sessions. |
| `rp` | `1` = the usage numbers are a replay, not a fresh poll (heartbeats, and writes between polls for an event, `a`/`n` change, alert or setting); the rate math and reset detection skip them. Fast-clock fields in the same write still count: a new `es` fires. |
| `lv` | How lively Clawd is, `1` (No-nonsense) to `5` (All-nonsense); `3` is the default. Sent with every data payload and right away when the setting changes. The device stores the last value in NVS, so it applies from boot before the app connects; absent = keep the stored value. |
| `aa` | How long Clawd points and waves for an alert, in seconds: `120` (the default, the whole alert), `30`, `10`, or `0` = no animation. Only the animation: the alert's message, tap, `x`, timeout and dismiss notify are unchanged. Sent like `lv` and stored the same way (clamped to 0–120); absent = keep the stored value. |
| `pm` | Pace marker: `1` draws a line on the session and weekly bars at the elapsed share of each window (5 hours, 7 days) and colours the fill by pace (green at or behind it, amber ahead, red at 100%); `0` keeps the plain 50/80% colours. A bar with no reset time (`sr`/`wr` = -1) and spend-capped accounts keep the plain colours. Sent like `lv` and stored the same way; absent = keep the stored value. |
| `ag` | Fork only (the official app never sends it): subagents per Claude Code session for the device's Agentes page. `[[name, state, [[type, task, secs, running], ...]], ...]`: project folder name, state `0` paused / `1` working / `2` waiting on agents; agent type, short task, seconds (elapsed while running, total once done) and `1` running / `0` done. Up to 4 sessions and 10 agents. A write with `ag` and no `ok` is agents-only and leaves the usage readings alone. The app sends it that way, on its own write: when the list changes (at most once a second), every 60 s while sessions are listed, and after a reconnect; `[]` once to clear a list. It keeps the write within 480 bytes by dropping done agents (oldest first), then paused sessions, then the newest running agents. Sessions come from `agents.log` (written by the hook): a session goes after 5 min without events unless an agent runs, or on `SessionEnd`; done agents after 10 min; running agents with no stop after 2 h. |
| `wifi` | Fork only: WiFi provisioning, see [WiFi transport](#wifi-transport). A write with `wifi` and no `ok` is WiFi-only and leaves the usage readings alone. |

## WiFi transport

Fork only, and only on boards built with `-DBOARD_HAS_WIFI=1` (the
AMOLED-2.16 and the simulator). The app still produces every line; WiFi
only replaces the radio link when Bluetooth doesn't reach. BLE keeps
working unchanged alongside it.

### Provisioning (over BLE, on RX)

| Line | Effect |
| --- | --- |
| `{"wifi":{"ssid":"Casa","pass":"…","key":"<32 hex>","en":true}}` | Stores ssid, password and key in NVS (namespace `wifi`) and joins the network as a station. `en` defaults to `true`. `pass` may be empty (open network). |
| `{"wifi":{"en":false}}` | Keeps the creds, turns the radio off (saves battery). `{"wifi":{"en":true}}` turns it back on. |
| `{"wifi":null}` | Forgets the creds and turns the radio off. |

Limits: ssid 1–32 bytes, password up to 63, key exactly 32 hex characters
(compared case-insensitively). An invalid `wifi` value gets `{"err":true}`.
A valid one is acked with the current status: `{"ack":true,"wifi":{...}}`.
Resending the same creds doesn't drop the link; a new ssid, password or key
reconnects and drops the TCP client. The same lines work over TCP once
authenticated.

### Status

`{"st":"off|connecting|ok|fail","ip":"192.168.1.5","ssid":"Casa"}`, never
the password; `ip` only while `ok`. The device sends `{"wifi":{...}}` on TX
(and to the TCP host) whenever the state changes, includes it in the ack of
a `wifi` line, and appends `"wifi":{...}` to the serial `info` line. After
a failure it retries with backoff (2 s, 4 s … 60 s); a join that hasn't
finished in 20 s counts as a failure.

### Discovery and TCP

While joined, the device advertises the mDNS service `_clawdmeter._tcp` on
TCP port `7363`: instance name = BLE name (`Clawdmeter`), host
`clawdmeter-<last 4 MAC hex>.local`, TXT `id=<BLE MAC>` in the app's
device-key form (uppercase, colons: `44:BD:8D:77:AF:11`).

Framing is newline-delimited UTF-8 JSON in both directions, one object per
line, at most 511 bytes (longer lines are dropped).

1. The client's first line must be `{"auth":"<key>"}` within 3 s, or the
   device closes the socket. The device answers `{"auth":"ok"}`.
2. After that every client line is handled exactly like a BLE RX write
   (usage payloads, `ag`, `wifi`): the device answers `{"ack":true}` or
   `{"err":true}` per line.
3. The device writes to the client what it would notify over BLE:
   `{"dismiss":true}`, `{"wifi":{...}}` on a status change, and
   `{"req":true}` (the REQ characteristic's refresh request) right after
   authentication when it has no data yet.

One client at a time: a newly authenticated client replaces the previous
one; a connection still authenticating doesn't disturb it. The device
counts as connected to a host when BLE is connected or a TCP client is
authenticated; the status line, pairing hint and stale logic treat both
the same.

## USB transport

Fork only. The app writes the same JSON lines over the USB serial port
(115200, the port the `info` command uses), one object per line; the
firmware tells them from serial commands by the leading `{`. No auth: the
cable is the trust. Every line is handled like a BLE RX write and answered
with `{"ack":true}` or `{"err":true}` on the same port, mixed with the
firmware's log lines (the app skips anything it can't read).

- `{"hb":1}` is a keepalive: acked, nothing else. The app sends it to open
  the link (no ack within 3 s means firmware without the USB link) and
  every 15 s when it has nothing else to write.
- The device counts a USB host as attached while lines keep coming (45 s).
  A new one gets the WiFi status and, without data yet, `{"req":true}`.
  `{"dismiss":true}` and WiFi status changes go to it too.

## Link priority and the buttons

The app uses USB, then WiFi, then Bluetooth, and moves up as soon as a
better link answers. While the app is on USB or WiFi for 3 s, the device
stops advertising and drops its Bluetooth links (Windows otherwise keeps
the HID keyboard connected); Bluetooth comes back as soon as neither is
there, and stays on for 2 minutes after a pair gesture.

The buttons (Space for Claude Code's hold-to-talk, Shift+Tab) are BLE HID
keys only on Bluetooth. On USB or WiFi the device sends
`{"key":"space"|"shift_tab","down":true|false}` and the app types the key
on the computer (Windows), repeating a held Space like a keyboard does.

The window and the daemon share the serial port through `<state>/usb-claim`
(unix seconds): while it's in the future the daemon leaves the port alone,
so flashing and the Device page's `info` probe can open it.

## Power

Fork only, on any link, matched exactly and acked:

| Line | Effect |
| --- | --- |
| `{"power":"sleep"}` | Rest: the panel fades to black and stays dark (USB power, payloads and alerts don't wake it). A button press, a touch or `wake` brings it back. |
| `{"power":"wake"}` | Leaves rest. |
| `{"power":"off"}` | Acked, then the PMU cuts the power like an 8 s PWR hold. Only PWR turns it back on; plugging USB in with it off doesn't. Boards without a PMU that can do it (`power_hal_shutdown()` weak default) answer `{"err":true}` after the ack. |
