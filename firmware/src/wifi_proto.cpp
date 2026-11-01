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

bool wifi_auth_ok(const char* line, const char* key) {
    if (!line || !key || strlen(key) != WIFI_KEY_LEN) return false;
    JsonDocument doc;
    if (deserializeJson(doc, line)) return false;
    char got[WIFI_KEY_LEN + 1];
    if (!copy_key(got, doc["auth"] | (const char*)nullptr)) return false;
    char want[WIFI_KEY_LEN + 1];
    if (!copy_key(want, key)) return false;
    unsigned diff = 0;   // constant time: no early exit on the first mismatch
    for (int i = 0; i < WIFI_KEY_LEN; i++) diff |= (unsigned)(got[i] ^ want[i]);
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
