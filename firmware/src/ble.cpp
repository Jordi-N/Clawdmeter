#include "ble.h"
#include <Arduino.h>
#include <NimBLEDevice.h>
#include <NimBLEHIDDevice.h>
#include <Preferences.h>

#define DEVICE_NAME "Clawdmeter"

// Custom GATT UUIDs for data channel
#define SERVICE_UUID        "4c41555a-4465-7669-6365-000000000001"
#define RX_CHAR_UUID        "4c41555a-4465-7669-6365-000000000002"  // host writes here
#define TX_CHAR_UUID        "4c41555a-4465-7669-6365-000000000003"  // device ack/nack notifies
#define REQ_CHAR_UUID       "4c41555a-4465-7669-6365-000000000004"  // device-initiated refresh request

#define BLE_BUF_SIZE 512

// HID keyboard report descriptor (standard 6-KRO boot-protocol-compatible).
// Includes the LED output report (Num/Caps/Scroll Lock indicators) — without
// it macOS's Keyboard Setup Assistant flags the device as "unidentifiable"
// because the descriptor doesn't look like a complete keyboard.
static const uint8_t HID_REPORT_MAP[] = {
    0x05, 0x01,  // Usage Page (Generic Desktop)
    0x09, 0x06,  // Usage (Keyboard)
    0xA1, 0x01,  // Collection (Application)
    0x85, 0x01,  //   Report ID (1)
    0x05, 0x07,  //   Usage Page (Key Codes)
    0x19, 0xE0,  //   Usage Minimum (224) - Left Control
    0x29, 0xE7,  //   Usage Maximum (231) - Right GUI
    0x15, 0x00,  //   Logical Minimum (0)
    0x25, 0x01,  //   Logical Maximum (1)
    0x75, 0x01,  //   Report Size (1)
    0x95, 0x08,  //   Report Count (8)
    0x81, 0x02,  //   Input (Data, Variable, Absolute) - Modifier byte
    0x95, 0x01,  //   Report Count (1)
    0x75, 0x08,  //   Report Size (8)
    0x81, 0x01,  //   Input (Constant) - Reserved byte
    // LED output report — required for macOS to treat this as a full keyboard.
    0x95, 0x05,  //   Report Count (5)
    0x75, 0x01,  //   Report Size (1)
    0x05, 0x08,  //   Usage Page (LEDs)
    0x19, 0x01,  //   Usage Minimum (Num Lock)
    0x29, 0x05,  //   Usage Maximum (Kana)
    0x91, 0x02,  //   Output (Data, Variable, Absolute) - LED report
    0x95, 0x01,  //   Report Count (1)
    0x75, 0x03,  //   Report Size (3)
    0x91, 0x01,  //   Output (Constant) - LED report padding
    0x95, 0x06,  //   Report Count (6)
    0x75, 0x08,  //   Report Size (8)
    0x15, 0x00,  //   Logical Minimum (0)
    0x25, 0x65,  //   Logical Maximum (101)
    0x05, 0x07,  //   Usage Page (Key Codes)
    0x19, 0x00,  //   Usage Minimum (0)
    0x29, 0x65,  //   Usage Maximum (101)
    0x81, 0x00,  //   Input (Data, Array) - Key array (6 keys)
    0xC0,        // End Collection
};

static NimBLEServer* server = nullptr;
static NimBLEHIDDevice* hid_dev = nullptr;
static NimBLECharacteristic* input_kbd = nullptr;
static NimBLECharacteristic* tx_char = nullptr;
static NimBLECharacteristic* rx_char = nullptr;
static NimBLECharacteristic* req_char = nullptr;

static ble_state_t state = BLE_STATE_INIT;
static bool need_advertise = false;
static bool link_secure = false;   // ble_link_secure()
// Off while the app talks over USB or WiFi (ble_set_radio).
static bool radio_on = true;
static uint32_t radio_kick_ms = 0;   // last disconnect sent while off

// One-shot supervision-timeout pushback (see onConnParamsUpdate). Written by
// NimBLE host-task callbacks, consumed by ble_tick() on the loop task.
static const uint16_t CONN_HANDLE_NONE  = 0xFFFF;
static const uint16_t DESIRED_TIMEOUT   = 600;   // ×10ms = 6s, matches PPCP
static volatile uint16_t param_fix_handle = CONN_HANDLE_NONE;  // pending retry
static volatile uint32_t param_fix_at_ms  = 0;                 // when to send it
static volatile uint16_t param_fix_spent  = CONN_HANDLE_NONE;  // one per connection
// RX writes land in rx_buf on the NimBLE host task (core 0); the loop parses
// on core 1. Both sides copy under rx_lock, and the loop parses its own copy
// (parse_buf), so a write never changes the text mid-parse.
// A few slots, so two writes landing in one loop pass (during a long
// redraw) don't overwrite each other.
#define RX_SLOTS 3
static char rx_buf[RX_SLOTS][BLE_BUF_SIZE];
static uint8_t rx_head = 0, rx_count = 0;   // under rx_lock
static char parse_buf[BLE_BUF_SIZE];
static portMUX_TYPE rx_lock = portMUX_INITIALIZER_UNLOCKED;
static volatile bool data_ready = false;
static volatile bool has_received_data = false;
static char mac_str[18];

// --- Link diagnostics (ble_diag_json, serial `ble`) -------------------------
//
// Counters are written by NimBLE host-task callbacks and read on the loop
// task; they're diagnostics, so plain 32-bit stores are enough.
struct LinkSlot {
    uint16_t handle;    // CONN_HANDLE_NONE: free
    uint32_t at_ms;     // connect time, for the duration on disconnect
    uint32_t rx_ms;     // last accepted RX write (0: none yet)
    bool secure;        // encrypted
    bool reported;      // "encrypted, rssi=..." line printed
    bool silent;        // "no RX write" line printed for this silence
};
static LinkSlot links[CONFIG_BT_NIMBLE_MAX_CONNECTIONS];
static uint32_t n_connects = 0, n_drops = 0;
static uint32_t n_unsecured_drops = 0;   // left before encryption (not by us)
static uint32_t n_enc_fail = 0;          // auth completed without encryption
static uint32_t n_adv = 0, n_adv_fail = 0;
static uint32_t n_rx = 0, n_rx_rejected = 0, n_rx_truncated = 0, n_rx_overrun = 0;
static uint16_t max_write = 0;
static int last_reason = 0;
static uint32_t last_rx_ms = 0;          // any link; 0: none since boot
static uint8_t stale_streak = 0;         // links in a row dropped before encryption
#define RX_SILENCE_MS 45000              // the app writes at least every 15 s
#define NEAR_LIMIT_B  450                // a new longest write this long is logged

static LinkSlot* link_find(uint16_t h) {
    for (LinkSlot& l : links) if (l.handle == h) return &l;
    return nullptr;
}

// link_secure follows the links: the owner's encrypted link leaving while an
// unencrypted one stays must not keep the UI on "connected".
static void update_link_secure() {
    bool any = false;
    for (const LinkSlot& l : links) if (l.handle != CONN_HANDLE_NONE && l.secure) any = true;
    link_secure = any;
}

// True at most once per gap_ms for one `last` stamp: for lines that could
// otherwise repeat on every write.
static bool log_due(uint32_t* last, uint32_t gap_ms) {
    const uint32_t now = millis();
    if (*last && now - *last < gap_ms) return false;
    *last = now ? now : 1;
    return true;
}

// NimBLE reports HCI disconnect reasons as 0x200 + the HCI error code.
static const char* reason_name(int reason) {
    if (reason < 0x200 || reason >= 0x300) return "host";
    switch (reason - 0x200) {
    case 0x05: return "auth failure";
    case 0x06: return "key missing";
    case 0x08: return "supervision timeout";
    case 0x13: return "closed by peer";
    case 0x16: return "closed by us";
    case 0x1a: return "unsupported feature";
    case 0x22: return "LL response timeout";
    case 0x28: return "instant passed";
    case 0x3b: return "unacceptable params";
    case 0x3d: return "MIC failure";
    case 0x3e: return "failed to establish";
    default:   return "other";
    }
}
static const int REASON_LOCAL = 0x216;   // we called disconnect()

// --- Single-owner lock -----------------------------------------------------
//
// The board is a BLE peripheral that any central in range could connect to and
// write usage data to. To stop the display rotating to another machine's
// account, it locks to ONE owner: the identity address of the machine it is
// bonded to, persisted in NVS. Only that owner (over a bonded+encrypted link)
// may write usage data; a second machine that pairs is rejected so the board
// stays paired to a single machine. The hold-power bond-clear gesture resets
// the owner so the board can be handed to a different machine.
static Preferences prefs;
static char owner_addr[18] = {0};   // owner identity address, e.g. "aa:bb:cc:dd:ee:ff"
static bool owner_set = false;
static const char* ZERO_ADDR = "00:00:00:00:00:00";

static void save_owner() {
    prefs.begin("clawd", false);
    prefs.putString("owner", owner_addr);
    prefs.end();
}

static void clear_owner() {
    owner_set = false;
    owner_addr[0] = '\0';
    prefs.begin("clawd", false);
    prefs.remove("owner");
    prefs.end();
}

static void load_owner() {
    prefs.begin("clawd", true);
    String o = prefs.getString("owner", "");
    prefs.end();
    if (o.length() == 17) {  // "aa:bb:cc:dd:ee:ff"
        strncpy(owner_addr, o.c_str(), sizeof(owner_addr) - 1);
        owner_addr[sizeof(owner_addr) - 1] = '\0';
        owner_set = true;
        Serial.printf("BLE: owner loaded = %s\n", owner_addr);
    }
}

// Delete every stored bond that isn't the owner, so the board stays paired to
// exactly one machine. Removing a bond shifts the indices, so restart from 0.
static void prune_foreign_bonds() {
    if (!owner_set) return;
    bool removed;
    do {
        removed = false;
        int n = NimBLEDevice::getNumBonds();
        for (int i = 0; i < n; i++) {
            NimBLEAddress a = NimBLEDevice::getBondedAddress(i);
            if (strcmp(a.toString().c_str(), owner_addr) != 0) {
                Serial.printf("BLE: pruning non-owner bond %s\n", a.toString().c_str());
                NimBLEDevice::deleteBond(a);
                removed = true;
                break;
            }
        }
    } while (removed);
}

static void claim_owner(const std::string& id) {
    strncpy(owner_addr, id.c_str(), sizeof(owner_addr) - 1);
    owner_addr[sizeof(owner_addr) - 1] = '\0';
    owner_set = true;
    save_owner();
    Serial.printf("BLE: owner claimed = %s\n", owner_addr);
    prune_foreign_bonds();
}

static void start_advertising() {
    NimBLEAdvertising* adv = NimBLEDevice::getAdvertising();
    adv->reset();
    // Primary advertising packet (≤31 bytes):
    //   flags (3) + appearance (4) + HID service 0x1812 (4) + name "Clawdmeter" (12)
    //   = 23 bytes. macOS Bluetooth Settings only surfaces BLE-only devices
    //   that explicitly advertise the standard HID service UUID (0x1812) —
    //   without it the device is recognized internally but hidden from the
    //   GUI nearby-devices list.
    adv->setAppearance(HID_KEYBOARD);
    adv->addServiceUUID(NimBLEUUID((uint16_t)0x1812));  // BLE HID Service
    adv->setName(DEVICE_NAME);
    // Scan response carries the 128-bit custom data-service UUID for active
    // scanners (the host app scans actively).
    NimBLEAdvertisementData scanResp;
    scanResp.setCompleteServices(NimBLEUUID(SERVICE_UUID));
    adv->setScanResponseData(scanResp);
    adv->enableScanResponse(true);
    bool ok = adv->start();
    n_adv++;
    if (!ok) n_adv_fail++;
    // Only reflect ADVERTISING in the UI state when no client is connected.
    // With MAX_CONNECTIONS=2, onConnect re-advertises to fill the second slot;
    // without this guard the UI would flip CONNECTED → ADVERTISING on every
    // first connect and never come back until a second client arrived.
    if (!server || server->getConnectedCount() == 0) {
        state = BLE_STATE_ADVERTISING;
    }
    Serial.printf("BLE: advertising start=%s (connected=%u)\n",
        ok ? "OK" : "FAILED",
        server ? (unsigned)server->getConnectedCount() : 0);
}

class ServerCallbacks : public NimBLEServerCallbacks {
    void onConnect(NimBLEServer* s, NimBLEConnInfo& info) override {
        state = BLE_STATE_CONNECTED;
        n_connects++;
        LinkSlot* l = link_find(CONN_HANDLE_NONE);
        if (l) *l = { info.getConnHandle(), millis(), 0, info.isEncrypted(), false, false };
        update_link_secure();
        Serial.printf("BLE: connected from %s (active=%u, connect #%lu)\n",
            info.getAddress().toString().c_str(),
            (unsigned)s->getConnectedCount(), (unsigned long)n_connects);
        // Log negotiated link timing — the difference between guessing and
        // knowing when debugging disconnects (e.g. reason=520 supervision
        // timeouts are only explainable next to the negotiated timeout).
        // Units: interval ×1.25ms, timeout ×10ms, latency = skippable events.
        Serial.printf("BLE: connparams itvl=%u(%.2fms) lat=%u timeout=%u(%ums)\n",
            info.getConnInterval(), info.getConnInterval() * 1.25f,
            info.getConnLatency(), info.getConnTimeout(), info.getConnTimeout() * 10);
        // Keep advertising while a connection slot is still free so a second
        // central (e.g. the host app alongside an OS-held HID link) can
        // discover and connect. NimBLE auto-stops advertising on each accept.
        if (s->getConnectedCount() < CONFIG_BT_NIMBLE_MAX_CONNECTIONS) {
            need_advertise = true;
        }
        // Start pairing/encryption ourselves. RX writes are only accepted over
        // an encrypted link, but nothing in the GATT table forces the central
        // to pair; macOS can keep a cached GATT table after a bond clear and
        // never read a protected HID characteristic, leaving it stuck on
        // "Connecting" with an unencrypted link.
        if (!info.isEncrypted()) {
            int rc = 0;
            bool ok = NimBLEDevice::startSecurity(info.getConnHandle(), &rc);
            Serial.printf("BLE: startSecurity %s (rc=%d)\n", ok ? "OK" : "FAILED", rc);
        }
    }

    void onDisconnect(NimBLEServer* s, NimBLEConnInfo& info, int reason) override {
        n_drops++;
        last_reason = reason;
        LinkSlot* l = link_find(info.getConnHandle());
        const bool was_secure = l && l->secure;
        const uint32_t secs = l ? (millis() - l->at_ms) / 1000 : 0;
        if (l) l->handle = CONN_HANDLE_NONE;
        update_link_secure();
        // Only flip the UI state to DISCONNECTED when the last client leaves.
        if (s->getConnectedCount() == 0) {
            state = BLE_STATE_DISCONNECTED;
            link_secure = false;
        }
        need_advertise = true;
        // Drop any pending/spent param pushback for this handle — NimBLE
        // reuses conn handles, so stale state would leak onto the next link.
        if (param_fix_handle == info.getConnHandle()) param_fix_handle = CONN_HANDLE_NONE;
        if (param_fix_spent  == info.getConnHandle()) param_fix_spent  = CONN_HANDLE_NONE;
        Serial.printf("BLE: disconnected (reason=%d %s, after %lus, %s, remaining=%u)\n",
            reason, reason_name(reason), (unsigned long)secs,
            was_secure ? "encrypted" : "never encrypted", (unsigned)s->getConnectedCount());
        // Stale bond: the computer keeps keys the board dropped, so it
        // connects, fails encryption and leaves, over and over. Our own
        // disconnects (radio off, non-owner) don't count.
        if (was_secure) {
            stale_streak = 0;
        } else if (reason != REASON_LOCAL) {
            n_unsecured_drops++;
            if (stale_streak < 255) stale_streak++;
            if (stale_streak == 3 || stale_streak % 10 == 0)
                Serial.printf("BLE: %u links in a row dropped before encryption: "
                    "stale pairing on the computer? (remove it there, then pair again)\n",
                    (unsigned)stale_streak);
        }
    }

    void onMTUChange(uint16_t mtu, NimBLEConnInfo& info) override {
        // Writes longer than MTU-3 need the long-write procedure; a small
        // MTU next to failed or cut writes explains them.
        (void)info;
        Serial.printf("BLE: MTU %u (single write up to %u B)\n",
            (unsigned)mtu, (unsigned)(mtu > 3 ? mtu - 3 : 0));
    }

    // Centrals re-negotiate parameters mid-connection. Windows in particular
    // clamps the supervision timeout to 2s once an app GATT session goes
    // active (captured on hardware; it honors 9.6s while only its HID driver
    // holds the link) — and a 2s window is tight enough that ordinary radio
    // gaps kill the link (HCI 0x208 → reason=520), which caused constant
    // reconnect churn. Push back ONCE per connection: schedule a
    // deferred LL connection-parameter request for the same interval range but
    // a 6s timeout (the mechanism Microsoft's accessory guidelines prescribe).
    // Deferred ~2s so it can't race the central's own in-flight update
    // transaction (Windows has a documented late-instant bug there), and
    // one-shot so a central that re-clamps doesn't trigger an update war.
    void onConnParamsUpdate(NimBLEConnInfo& info) override {
        Serial.printf("BLE: connparams update itvl=%u(%.2fms) lat=%u timeout=%u(%ums)\n",
            info.getConnInterval(), info.getConnInterval() * 1.25f,
            info.getConnLatency(), info.getConnTimeout(), info.getConnTimeout() * 10);
        if (info.getConnTimeout() < DESIRED_TIMEOUT &&
            info.getConnHandle() != param_fix_spent) {
            param_fix_handle = info.getConnHandle();
            param_fix_at_ms  = millis() + 2000;
        }
    }

    // Lock the board to a single owner machine. The first machine to bond
    // becomes the owner; any other machine that pairs is un-bonded and dropped
    // so the board never shows (or rotates to) a second machine's account.
    void onAuthenticationComplete(NimBLEConnInfo& info) override {
        std::string id = info.getIdAddress().toString();
        Serial.printf("BLE: auth complete peer=%s bonded=%d enc=%d\n",
            id.c_str(), info.isBonded() ? 1 : 0, info.isEncrypted() ? 1 : 0);
        // Bonded reconnects START at the central's clamped parameters (no
        // later update event fires), so the supervision-timeout pushback must
        // also arm here, not just in onConnParamsUpdate.
        if (info.getConnTimeout() < DESIRED_TIMEOUT &&
            info.getConnHandle() != param_fix_spent) {
            param_fix_handle = info.getConnHandle();
            param_fix_at_ms  = millis() + 2000;
        }
        if (id == ZERO_ADDR) return;
        // Only an encrypted link may claim the owner lock or be judged by it:
        // a failed pairing leaves no bond, and its address can be a random
        // one that, saved as the owner, would lock out the real machine.
        if (!info.isEncrypted()) {
            n_enc_fail++;
            Serial.printf("BLE: encryption failed for %s (#%lu): the computer keeps an old pairing (pair again)\n",
                id.c_str(), (unsigned long)n_enc_fail);
            return;
        }
        if (!owner_set) {
            claim_owner(id);
        } else if (strcmp(id.c_str(), owner_addr) != 0) {
            Serial.printf("BLE: rejecting non-owner %s (owner=%s)\n", id.c_str(), owner_addr);
            NimBLEDevice::deleteBond(info.getIdAddress());
            server->disconnect(info);
            return;
        }
        LinkSlot* l = link_find(info.getConnHandle());
        if (l) l->secure = true;
        update_link_secure();
    }

};

class RxCallbacks : public NimBLECharacteristicCallbacks {
    void onWrite(NimBLECharacteristic* chr, NimBLEConnInfo& info) override {
        // Only accept usage data over a bonded+encrypted link, and only from the
        // owner machine. Another machine's app in range is ignored so the
        // display never rotates to a foreign account. The first encrypted writer
        // claims ownership when none is set yet (e.g. a fresh pairing).
        static uint32_t rej_log = 0, cut_log = 0, ovr_log = 0;
        std::string id = info.getIdAddress().toString();
        if (!info.isEncrypted()) {
            n_rx_rejected++;
            if (log_due(&rej_log, 30000))
                Serial.printf("BLE: dropping RX write from unencrypted link (%lu so far)\n",
                    (unsigned long)n_rx_rejected);
            return;
        }
        if (!owner_set && id != ZERO_ADDR) {
            claim_owner(id);
        }
        if (owner_set && strcmp(id.c_str(), owner_addr) != 0) {
            n_rx_rejected++;
            if (log_due(&rej_log, 30000))
                Serial.printf("BLE: dropping RX write from non-owner %s (%lu so far)\n",
                    id.c_str(), (unsigned long)n_rx_rejected);
            return;
        }
        std::string val = chr->getValue();
        const size_t raw = val.length();
        size_t len = std::min(raw, (size_t)(BLE_BUF_SIZE - 1));
        n_rx++;
        const uint32_t now = millis();
        last_rx_ms = now ? now : 1;
        LinkSlot* l = link_find(info.getConnHandle());
        if (l) {
            if (l->silent)
                Serial.printf("BLE: RX writes resumed after %lus\n",
                    (unsigned long)((now - l->rx_ms) / 1000));
            l->rx_ms = last_rx_ms;
            l->silent = false;
        }
        if (raw > len) {
            n_rx_truncated++;
            if (log_due(&cut_log, 30000))
                Serial.printf("BLE: RX write %u B cut to %u B (%lu so far)\n",
                    (unsigned)raw, (unsigned)len, (unsigned long)n_rx_truncated);
        }
        if (raw > max_write) {
            max_write = (uint16_t)std::min(raw, (size_t)0xFFFF);
            if (raw >= NEAR_LIMIT_B)
                Serial.printf("BLE: RX write %u B, near the %u B buffer\n",
                    (unsigned)raw, (unsigned)(BLE_BUF_SIZE - 1));
        }
        bool overrun = false;
        portENTER_CRITICAL(&rx_lock);
        // Full: the oldest line gives way (the app resends every few seconds).
        if (rx_count == RX_SLOTS) { rx_head = (rx_head + 1) % RX_SLOTS; rx_count--; overrun = true; }
        char* slot = rx_buf[(rx_head + rx_count) % RX_SLOTS];
        memcpy(slot, val.c_str(), len);
        slot[len] = '\0';
        rx_count++;
        data_ready = true;
        portEXIT_CRITICAL(&rx_lock);
        has_received_data = true;
        if (overrun) {
            // The loop didn't drain the queue in time (a long redraw or a stall).
            n_rx_overrun++;
            if (log_due(&ovr_log, 30000))
                Serial.printf("BLE: RX queue full, oldest line dropped (%lu so far)\n",
                    (unsigned long)n_rx_overrun);
        }
    }
};

// When the app enables notifications on the refresh char, ask for data
// if we have none yet. Firing on subscribe (not on connect) ensures the
// notification isn't dropped before the app's CCCD write completes.
class ReqCallbacks : public NimBLECharacteristicCallbacks {
    void onSubscribe(NimBLECharacteristic* chr, NimBLEConnInfo& info, uint16_t subValue) override {
        Serial.printf("BLE: req_char onSubscribe subValue=%u has_data=%d\n", subValue, has_received_data ? 1 : 0);
        if (subValue != 0 && !has_received_data) {
            ble_request_refresh();
        }
    }
};

void ble_init(void) {
    for (LinkSlot& l : links) l.handle = CONN_HANDLE_NONE;
    NimBLEDevice::init(DEVICE_NAME);
    NimBLEDevice::setSecurityAuth(true, false, true);  // bonding, no MITM, SC

    // Restore the locked owner (if any) and drop any stale non-owner bonds so
    // the board stays paired to a single machine across reboots.
    load_owner();
    prune_foreign_bonds();

    // Format MAC address
    NimBLEAddress addr = NimBLEDevice::getAddress();
    snprintf(mac_str, sizeof(mac_str), "%s", addr.toString().c_str());
    for (int i = 0; mac_str[i]; i++) {
        if (mac_str[i] >= 'a' && mac_str[i] <= 'f') mac_str[i] -= 32;
    }

    server = NimBLEDevice::createServer();
    static ServerCallbacks serverCb;
    server->setCallbacks(&serverCb);
    // Advertising is ours to restart (need_advertise): NimBLE doing it on every
    // disconnect would bring a bonded host straight back while the radio is off.
    server->advertiseOnDisconnect(false);

    // --- HID keyboard service ---
    hid_dev = new NimBLEHIDDevice(server);
    hid_dev->setReportMap((uint8_t*)HID_REPORT_MAP, sizeof(HID_REPORT_MAP));
    hid_dev->setManufacturer("Clawdmeter");
    // PnP ID: (vendorIdSource, vendorId, productId, version).
    // Source 1 = Bluetooth SIG, vendor 0x02E5 = Espressif. Don't claim
    // Apple's IDs (USB vendor 0x05AC): macOS validates Apple-claimed HIDs
    // against known devices and silently refuses to surface a Connect button
    // for spoofers.
    hid_dev->setPnp(0x01, 0x02E5, 0x0001, 0x0100);
    // country=33 (US ANSI). Setting this to 0 ("not supported") causes macOS
    // to launch the Keyboard Setup Assistant on first pair asking the user
    // to identify the layout — we only ever send Space / Shift+Tab so the
    // physical layout is irrelevant; advertise a known one to skip the wizard.
    hid_dev->setHidInfo(33, 0x02);
    hid_dev->setBatteryLevel(100);
    input_kbd = hid_dev->getInputReport(1);  // report ID 1

    // --- Custom data service ---
    NimBLEService* svc = server->createService(SERVICE_UUID);

    rx_char = svc->createCharacteristic(
        RX_CHAR_UUID,
        NIMBLE_PROPERTY::WRITE | NIMBLE_PROPERTY::WRITE_NR
    );
    static RxCallbacks rxCb;
    rx_char->setCallbacks(&rxCb);

    tx_char = svc->createCharacteristic(
        TX_CHAR_UUID,
        NIMBLE_PROPERTY::READ | NIMBLE_PROPERTY::NOTIFY
    );

    req_char = svc->createCharacteristic(
        REQ_CHAR_UUID,
        NIMBLE_PROPERTY::NOTIFY
    );
    static ReqCallbacks reqCb;
    req_char->setCallbacks(&reqCb);

    svc->start();
    server->start();
    start_advertising();

    Serial.printf("BLE: init complete, MAC=%s\n", mac_str);
}

void ble_tick(void) {
    if (!radio_on) {
        // A central that got in before advertising stopped is let go.
        if (server && server->getConnectedCount() > 0 && millis() - radio_kick_ms >= 1000) {
            radio_kick_ms = millis();
            ble_disconnect_all();
        }
        return;
    }
    if (need_advertise) {
        need_advertise = false;
        start_advertising();
    }
    // Deferred one-shot supervision-timeout pushback (see onConnParamsUpdate).
    if (param_fix_handle != CONN_HANDLE_NONE &&
        (int32_t)(millis() - param_fix_at_ms) >= 0) {
        uint16_t h = param_fix_handle;
        param_fix_handle = CONN_HANDLE_NONE;
        param_fix_spent  = h;
        if (server && server->getConnectedCount() > 0) {
            Serial.println("BLE: requesting 6s supervision timeout");
            server->updateConnParams(h, 12, 24, 0, DESIRED_TIMEOUT);
        }
    }
    // Once per encrypted link: signal strength and MTU, the first suspects
    // when a link keeps dropping. Then one line when the app goes quiet on it.
    if (!server) return;
    for (LinkSlot& l : links) {
        if (l.handle == CONN_HANDLE_NONE || !l.secure) continue;
        if (!l.reported) {
            l.reported = true;
            int8_t rssi = 0;
            const int rc = ble_gap_conn_rssi(l.handle, &rssi);
            NimBLEConnInfo ci = server->getPeerInfoByHandle(l.handle);
            if (rc == 0)
                Serial.printf("BLE: link %s encrypted, rssi=%d dBm, mtu=%u\n",
                    ci.getIdAddress().toString().c_str(), (int)rssi, (unsigned)ci.getMTU());
            else
                Serial.printf("BLE: link %s encrypted, rssi n/a (rc=%d), mtu=%u\n",
                    ci.getIdAddress().toString().c_str(), rc, (unsigned)ci.getMTU());
        }
        if (l.rx_ms && !l.silent && millis() - l.rx_ms >= RX_SILENCE_MS) {
            l.silent = true;
            Serial.printf("BLE: encrypted link but no RX write for %us (app stalled?)\n",
                (unsigned)(RX_SILENCE_MS / 1000));
        }
    }
}

size_t ble_diag_json(char* buf, size_t cap) {
    const long rx_age = last_rx_ms ? (long)((millis() - last_rx_ms) / 1000) : -1;
    int n = snprintf(buf, cap,
        "{\"con\":%lu,\"drop\":%lu,\"noenc\":%lu,\"encf\":%lu,\"adv\":%lu,\"advf\":%lu,"
        "\"rx\":%lu,\"rxrej\":%lu,\"cut\":%lu,\"ovr\":%lu,\"maxw\":%u,\"rsn\":%d,\"rxage\":%ld",
        (unsigned long)n_connects, (unsigned long)n_drops, (unsigned long)n_unsecured_drops,
        (unsigned long)n_enc_fail, (unsigned long)n_adv, (unsigned long)n_adv_fail,
        (unsigned long)n_rx, (unsigned long)n_rx_rejected, (unsigned long)n_rx_truncated,
        (unsigned long)n_rx_overrun, (unsigned)max_write, last_reason, rx_age);
    if (n <= 0 || (size_t)n + 1 >= cap) return 0;
    // Signal and MTU of the first encrypted link, when there is one.
    for (const LinkSlot& l : links) {
        if (!server || l.handle == CONN_HANDLE_NONE || !l.secure) continue;
        int8_t rssi = 0;
        if (ble_gap_conn_rssi(l.handle, &rssi) != 0) break;
        const int m = snprintf(buf + n, cap - n, ",\"rssi\":%d,\"mtu\":%u", (int)rssi,
            (unsigned)server->getPeerInfoByHandle(l.handle).getMTU());
        if (m > 0 && (size_t)(n + m) + 1 < cap) n += m;
        else buf[n] = '\0';
        break;
    }
    buf[n++] = '}';
    buf[n] = '\0';
    return (size_t)n;
}

ble_state_t ble_get_state(void) {
    return state;
}

bool ble_link_secure(void) { return state == BLE_STATE_CONNECTED && link_secure; }
bool ble_has_bond(void) { return NimBLEDevice::getNumBonds() > 0; }

const char* ble_get_device_name(void) {
    return DEVICE_NAME;
}

const char* ble_get_mac_address(void) {
    return mac_str;
}

void ble_clear_bonds(void) {
    // Delete bonds one at a time (always index 0: each delete shifts the list)
    // and report failures: deleteAllBonds() bails silently on the first error,
    // leaving a stale bond in NVS that breaks re-pairing.
    for (int guard = NimBLEDevice::getNumBonds(); guard > 0 && NimBLEDevice::getNumBonds() > 0; guard--) {
        NimBLEAddress a = NimBLEDevice::getBondedAddress(0);
        bool ok = NimBLEDevice::deleteBond(a);
        if (!ok) {
            // ble_gap_unpair() stops at the first missing record (e.g. our_sec
            // already gone) and leaves the peer's security entry behind, so
            // delete each record directly and ignore "not found".
            struct ble_store_key_sec key;
            memset(&key, 0, sizeof(key));
            key.peer_addr = *a.getBase();
            int r1 = ble_store_delete_peer_sec(&key);
            int r2 = ble_store_delete_our_sec(&key);
            Serial.printf("BLE: unpair failed; direct delete peer_sec rc=%d our_sec rc=%d\n", r1, r2);
        }
        Serial.printf("BLE: delete bond %s -> %s\n", a.toString().c_str(), ok ? "ok" : "forced");
    }
    clear_owner();  // release ownership so the board can be handed to another machine
    Serial.printf("BLE: bonds cleared (remaining=%d)\n", NimBLEDevice::getNumBonds());
    // Every link: with two centrals, dropping only peer 0 kept the other.
    ble_disconnect_all();
    need_advertise = true;
}

void ble_print_status(void) {
    static const char* names[] = {"INIT", "ADVERTISING", "CONNECTED", "DISCONNECTED"};
    Serial.printf("BLE status: state=%s bonds=%d owner=%s\n",
        names[state], NimBLEDevice::getNumBonds(), owner_set ? owner_addr : "none");
    if (!server) return;
    for (uint8_t i = 0; i < server->getConnectedCount(); i++) {
        NimBLEConnInfo ci = server->getPeerInfo(i);
        Serial.printf("BLE status: peer=%s id=%s enc=%d bonded=%d\n",
            ci.getAddress().toString().c_str(), ci.getIdAddress().toString().c_str(),
            ci.isEncrypted() ? 1 : 0, ci.isBonded() ? 1 : 0);
    }
    char d[256];
    if (ble_diag_json(d, sizeof(d))) Serial.printf("BLE diag: %s\n", d);
}

void ble_disconnect_all(void) {
    if (!server) return;
    // Collect the handles first: each disconnect shrinks the peer list.
    uint16_t handles[CONFIG_BT_NIMBLE_MAX_CONNECTIONS];
    uint8_t n = 0;
    for (uint8_t i = 0; i < server->getConnectedCount() && n < CONFIG_BT_NIMBLE_MAX_CONNECTIONS; i++) {
        handles[n++] = server->getPeerInfo(i).getConnHandle();
    }
    for (uint8_t i = 0; i < n; i++) server->disconnect(handles[i]);
}

void ble_set_radio(bool on) {
    if (on == radio_on) return;
    radio_on = on;
    if (on) {
        need_advertise = true;   // ble_tick starts it
        Serial.println("BLE: on (no USB/WiFi host)");
        return;
    }
    NimBLEDevice::getAdvertising()->stop();
    radio_kick_ms = millis();
    ble_disconnect_all();
    state = BLE_STATE_DISCONNECTED;
    Serial.println("BLE: off (host on USB/WiFi)");
}

bool ble_has_data(void) {
    return data_ready;
}

const char* ble_get_data(void) {
    portENTER_CRITICAL(&rx_lock);
    if (rx_count) {
        memcpy(parse_buf, rx_buf[rx_head], sizeof(parse_buf));
        rx_head = (rx_head + 1) % RX_SLOTS;
        rx_count--;
    } else {
        parse_buf[0] = '\0';
    }
    data_ready = rx_count > 0;
    portEXIT_CRITICAL(&rx_lock);
    return parse_buf;
}

// Acks ({"ack":true} / {"err":true}) and the WiFi status go through here.
void ble_send_line(const char* line) {
    if (state == BLE_STATE_CONNECTED && tx_char) {
        tx_char->setValue(line);
        tx_char->notify();
    }
}

void ble_send_dismiss(void) {
    if (state == BLE_STATE_CONNECTED && tx_char) {
        tx_char->setValue("{\"dismiss\":true}");
        tx_char->notify();
        Serial.println("BLE: alert dismissed on the device");
    }
}

void ble_set_battery_level(int pct) {
    if (!hid_dev || pct < 0) return;
    if (pct > 100) pct = 100;
    hid_dev->setBatteryLevel((uint8_t)pct, state == BLE_STATE_CONNECTED);
}

void ble_request_refresh(void) {
    if (state == BLE_STATE_CONNECTED && req_char) {
        uint8_t v = 0x01;
        req_char->setValue(&v, 1);
        req_char->notify();
        Serial.println("BLE: refresh requested");
    }
}

void ble_keyboard_press(uint8_t key, uint8_t modifier) {
    if (state != BLE_STATE_CONNECTED || !input_kbd) return;
    // HID report: [modifier, reserved, key1, key2, key3, key4, key5, key6]
    uint8_t report[8] = {modifier, 0, key, 0, 0, 0, 0, 0};
    input_kbd->setValue(report, sizeof(report));
    input_kbd->notify();
}

void ble_keyboard_release(void) {
    if (state != BLE_STATE_CONNECTED || !input_kbd) return;
    uint8_t report[8] = {0};
    input_kbd->setValue(report, sizeof(report));
    input_kbd->notify();
}
