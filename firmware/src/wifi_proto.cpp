#include "wifi_proto.h"
#include <ArduinoJson.h>
#include <stdio.h>
#include <string.h>

static bool is_hex(char c) {
    return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

// Copies a 32-hex key lowercased; false if it isn't exactly that.
static bool copy_key(char* dst, const char* src) {
    if (!src || strlen(src) != WIFI_KEY_LEN) return false;
    for (int i = 0; i < WIFI_KEY_LEN; i++) {
        if (!is_hex(src[i])) return false;
        dst[i] = (src[i] >= 'A' && src[i] <= 'F') ? (char)(src[i] + 32) : src[i];
    }
    dst[WIFI_KEY_LEN] = '\0';
    return true;
}

bool wifi_cmd_parse(const char* json, WifiCmd* out, bool* wifi_only) {
    if (wifi_only) *wifi_only = false;
    // Most lines are usage-only: skip the second deserialize for them.
    if (!json || !strstr(json, "\"wifi\"")) return false;
    JsonDocument doc;
    if (deserializeJson(doc, json)) return false;
    JsonObject root = doc.as<JsonObject>();
    if (root.isNull() || !root["wifi"].is<JsonVariantConst>()) return false;
    if (wifi_only) *wifi_only = !root["ok"].is<JsonVariantConst>();

    JsonVariant w = root["wifi"];
    WifiCmd c;
    memset(&c, 0, sizeof(c));
    if (w.isNull()) {
        c.kind = WIFI_CMD_FORGET;
        *out = c;
        return true;
    }
    if (!w.is<JsonObject>()) return false;
    const bool has_en = w["en"].is<bool>();
    if (w["ssid"].is<const char*>()) {
        const char* ssid = w["ssid"];
        const char* pass = w["pass"] | "";
        size_t sl = strlen(ssid), pl = strlen(pass);
        if (sl == 0 || sl >= sizeof(c.ssid) || pl >= sizeof(c.pass)) return false;
        if (!copy_key(c.key, w["key"] | (const char*)nullptr)) return false;
        memcpy(c.ssid, ssid, sl + 1);
        memcpy(c.pass, pass, pl + 1);
        c.kind = WIFI_CMD_SET;
        c.en = has_en ? w["en"].as<bool>() : true;
    } else if (has_en) {
        c.kind = WIFI_CMD_ENABLE;
        c.en = w["en"].as<bool>();
    } else {
        return false;
    }
    *out = c;
    return true;
}

// ---- SHA-256 (FIPS 180-4), small and portable: the sim and the native
// tests have no mbedTLS. Only short handshake messages go through it. ----

static const uint32_t K256[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static void sha256_block(uint32_t h[8], const uint8_t* p) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
        w[i] = (uint32_t)p[4 * i] << 24 | (uint32_t)p[4 * i + 1] << 16 | (uint32_t)p[4 * i + 2] << 8 | p[4 * i + 3];
    for (int i = 16; i < 64; i++) {
        const uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        const uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
    for (int i = 0; i < 64; i++) {
        const uint32_t t1 = hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K256[i] + w[i];
        const uint32_t t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        hh = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e; h[5] += f; h[6] += g; h[7] += hh;
}

void wifi_sha256(const uint8_t* data, size_t n, uint8_t out[32]) {
    uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                     0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    size_t i = 0;
    for (; i + 64 <= n; i += 64) sha256_block(h, data + i);
    // The rest, 0x80, zeros and the bit length: one block, or two when the
    // length doesn't fit after the rest.
    uint8_t tail[128];
    memset(tail, 0, sizeof(tail));
    const size_t rest = n - i;
    memcpy(tail, data + i, rest);
    tail[rest] = 0x80;
    const size_t tail_n = rest + 9 <= 64 ? 64 : 128;
    const uint64_t bits = (uint64_t)n * 8;
    for (int k = 0; k < 8; k++) tail[tail_n - 1 - k] = (uint8_t)(bits >> (8 * k));
    sha256_block(h, tail);
    if (tail_n == 128) sha256_block(h, tail + 64);
    for (int k = 0; k < 8; k++) {
        out[4 * k] = (uint8_t)(h[k] >> 24);
        out[4 * k + 1] = (uint8_t)(h[k] >> 16);
        out[4 * k + 2] = (uint8_t)(h[k] >> 8);
        out[4 * k + 3] = (uint8_t)h[k];
    }
}

// RFC 2104. msg_n is at most 128 here (handshake strings and tests).
void wifi_hmac_sha256(const uint8_t* key, size_t key_n, const uint8_t* msg, size_t msg_n, uint8_t out[32]) {
    uint8_t k[64];
    memset(k, 0, sizeof(k));
    if (key_n > 64) wifi_sha256(key, key_n, k);
    else memcpy(k, key, key_n);
    uint8_t buf[64 + 128];
    if (msg_n > 128) msg_n = 128;
    for (int i = 0; i < 64; i++) buf[i] = k[i] ^ 0x36;
    memcpy(buf + 64, msg, msg_n);
    uint8_t inner[32];
    wifi_sha256(buf, 64 + msg_n, inner);
    for (int i = 0; i < 64; i++) buf[i] = k[i] ^ 0x5c;
    memcpy(buf + 64, inner, 32);
    wifi_sha256(buf, 64 + 32, out);
}

static bool copy_hex(char* dst, const char* src, size_t len) {
    if (!src || strlen(src) != len) return false;
    for (size_t i = 0; i < len; i++) {
        if (!is_hex(src[i])) return false;
        dst[i] = (src[i] >= 'A' && src[i] <= 'F') ? (char)(src[i] + 32) : src[i];
    }
    dst[len] = '\0';
    return true;
}

bool wifi_auth_mac(const char* key, const char* label, const char* cn, const char* dn,
                   char out[WIFI_MAC_LEN + 1]) {
    char k[WIFI_KEY_LEN + 1];
    if (!copy_key(k, key)) return false;
    char msg[128];
    const int n = snprintf(msg, sizeof(msg), "%s:%s:%s", label, cn, dn);
    if (n <= 0 || n >= (int)sizeof(msg)) return false;
    uint8_t mac[32];
    wifi_hmac_sha256((const uint8_t*)k, WIFI_KEY_LEN, (const uint8_t*)msg, (size_t)n, mac);
    static const char HEX_DIGITS[] = "0123456789abcdef";
    for (int i = 0; i < 32; i++) {
        out[2 * i] = HEX_DIGITS[mac[i] >> 4];
        out[2 * i + 1] = HEX_DIGITS[mac[i] & 15];
    }
    out[WIFI_MAC_LEN] = '\0';
    return true;
}

bool wifi_hello_parse(const char* line, char cn[WIFI_NONCE_LEN + 1]) {
    if (!line) return false;
    JsonDocument doc;
    if (deserializeJson(doc, line)) return false;
    return copy_hex(cn, doc["hello"] | (const char*)nullptr, WIFI_NONCE_LEN);
}

bool wifi_auth_check(const char* line, const char* want) {
    if (!line || !want || strlen(want) != WIFI_MAC_LEN) return false;
    JsonDocument doc;
    if (deserializeJson(doc, line)) return false;
    char got[WIFI_MAC_LEN + 1];
    if (!copy_hex(got, doc["auth"] | (const char*)nullptr, WIFI_MAC_LEN)) return false;
    unsigned diff = 0;   // constant time: no early exit on the first mismatch
    for (int i = 0; i < WIFI_MAC_LEN; i++) diff |= (unsigned)(got[i] ^ want[i]);
    return diff == 0;
}

void linebuf_reset(LineBuf* lb) {
    lb->len = 0;
    lb->overflow = false;
}

size_t linebuf_feed(LineBuf* lb, const char* data, size_t n,
                    bool (*on_line)(const char* line, void* ctx), void* ctx) {
    for (size_t i = 0; i < n; i++) {
        const char ch = data[i];
        if (ch == '\n') {
            const bool emit = !lb->overflow && lb->len > 0;
            lb->buf[lb->len] = '\0';
            lb->len = 0;
            lb->overflow = false;
            if (emit && !on_line(lb->buf, ctx)) return i + 1;
        } else if (ch == '\r') {
            // stripped
        } else if (lb->len < WIFI_LINE_MAX - 1) {
            lb->buf[lb->len++] = ch;
        } else {
            lb->overflow = true;
        }
    }
    return n;
}

const char* wifi_st_name(WifiSt st) {
    switch (st) {
    case WIFI_ST_CONNECTING: return "connecting";
    case WIFI_ST_OK:         return "ok";
    case WIFI_ST_FAIL:       return "fail";
    default:                 return "off";
    }
}

size_t wifi_status_json(char* buf, size_t cap, WifiSt st, const char* ip, const char* ssid) {
    JsonDocument doc;
    doc["st"] = wifi_st_name(st);
    if (ip && ip[0]) doc["ip"] = ip;
    if (ssid && ssid[0]) doc["ssid"] = ssid;
    if (measureJson(doc) + 1 > cap) return 0;
    return serializeJson(doc, buf, cap);
}

uint32_t wifi_backoff_ms(unsigned attempt) {
    if (attempt > 5) return 60000;
    const uint32_t ms = 2000u << attempt;
    return ms > 60000 ? 60000 : ms;
}
