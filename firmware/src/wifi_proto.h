#pragma once
#include <stddef.h>
#include <stdint.h>

// WiFi transport, pure parts (no Arduino, no sockets; runs in the native
// tests): the `wifi` provisioning key, the TCP auth line, newline framing
// and the status object. See docs/protocol.md, "WiFi transport".

#define WIFI_TCP_PORT    7363
#define WIFI_KEY_LEN     32      // hex chars
#define WIFI_LINE_MAX    512     // same as the BLE receive buffer

enum WifiCmdKind {
    WIFI_CMD_SET,      // {"wifi":{"ssid","pass","key","en"}}: store creds, then en
    WIFI_CMD_ENABLE,   // {"wifi":{"en":bool}}: radio on/off, creds kept
    WIFI_CMD_FORGET,   // {"wifi":null}: forget creds, radio off
};

struct WifiCmd {
    WifiCmdKind kind;
    bool en;
    char ssid[33];     // 802.11: up to 32 bytes
    char pass[64];     // WPA2: up to 63 chars (empty = open network)
    char key[WIFI_KEY_LEN + 1];   // lowercase hex
};

// Returns true when the line carries a valid `wifi` key; `out` is then
// filled. Malformed values (ssid empty/too long, pass > 63, key not 32 hex
// chars, `en` missing on a bare object) return false and *wifi_only stays
// as computed. `*wifi_only` (if non-null) is true when the line has a
// `wifi` key (valid or not) and no `ok`: it must not reach the usage path.
bool wifi_cmd_parse(const char* json, WifiCmd* out, bool* wifi_only);

// True when `line` is exactly {"auth":"<key>"} for this 32-hex key
// (case-insensitive hex, compared in constant time). False for an empty key.
bool wifi_auth_ok(const char* line, const char* key);

// Newline framing for a byte stream. Lines longer than WIFI_LINE_MAX-1 are
// dropped whole; '\r' is stripped; empty lines are skipped.
struct LineBuf {
    char buf[WIFI_LINE_MAX];
    size_t len;
    bool overflow;
};
void linebuf_reset(LineBuf* lb);
// Feeds n bytes, calling on_line(line, ctx) for each complete line. Stops
// early (returns the bytes consumed) when on_line returns false, so the
// caller can stop reading a client it just dropped.
size_t linebuf_feed(LineBuf* lb, const char* data, size_t n,
                    bool (*on_line)(const char* line, void* ctx), void* ctx);

enum WifiSt { WIFI_ST_OFF, WIFI_ST_CONNECTING, WIFI_ST_OK, WIFI_ST_FAIL };
const char* wifi_st_name(WifiSt st);   // "off" | "connecting" | "ok" | "fail"

// {"st":"ok","ip":"192.168.1.5","ssid":"Casa"} (ip/ssid omitted when empty;
// never the password). Returns the length, 0 if it doesn't fit.
size_t wifi_status_json(char* buf, size_t cap, WifiSt st, const char* ip, const char* ssid);

// Reconnect backoff: 2 s, 4 s, 8 s ... capped at 60 s.
uint32_t wifi_backoff_ms(unsigned attempt);
