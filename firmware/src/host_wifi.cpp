#include "host_wifi.h"

#if !BOARD_HAS_WIFI
void host_wifi_init(void) {}
void host_wifi_tick(void (*)(const char*)) {}
void host_wifi_apply(const WifiCmd*) {}
bool host_wifi_client(void) { return false; }
bool host_wifi_has_creds(void) { return false; }
void host_wifi_send(const char*) {}
WifiSt host_wifi_state(void) { return WIFI_ST_OFF; }
size_t host_wifi_status(char*, size_t) { return 0; }
size_t host_wifi_diag(char*, size_t) { return 0; }
bool host_wifi_take_changed(void) { return false; }
#else
#include "host_tcp.h"
#include "ble.h"
#include <Arduino.h>
#include <Preferences.h>
#include <string.h>

#define CONNECT_TIMEOUT_MS 20000

static char ssid[33], pass[64], key[WIFI_KEY_LEN + 1];
static bool enabled = false;
static bool restart = false;    // config changed: drop the link, start over
static WifiSt st = WIFI_ST_OFF;
static bool changed = true;     // first tick reports the boot state
static unsigned attempt = 0;
static uint32_t since_ms = 0;   // attempt start, or when to retry after a fail
static char ip[16];
// Diagnostics since boot (host_wifi_diag).
static uint32_t n_tries = 0, n_fail = 0, n_lost = 0;
static uint32_t ok_since_ms = 0;   // when the link last came up

static void set_st(WifiSt s) {
    if (s == st) return;   // e.g. the ack's CONNECTING, then the tick's
    changed = true;
    st = s;
    Serial.printf("WiFi: %s\n", wifi_st_name(s));
}

static void load(void) {
    Preferences p;
    p.begin("wifi", true);
    ssid[0] = pass[0] = key[0] = '\0';
    p.getString("ssid", ssid, sizeof(ssid));
    p.getString("pass", pass, sizeof(pass));
    p.getString("key", key, sizeof(key));
    enabled = p.getBool("en", false);
    p.end();
}

static void save(void) {
    Preferences p;
    p.begin("wifi", false);
    if (ssid[0]) {
        p.putString("ssid", ssid);
        p.putString("pass", pass);
        p.putString("key", key);
    } else {
        p.remove("ssid");
        p.remove("pass");
        p.remove("key");
    }
    p.putBool("en", enabled);
    p.end();
}

void host_wifi_init(void) {
    radio_init();
    load();
    Serial.printf("WiFi: %s, creds %s\n", enabled ? "enabled" : "disabled", ssid[0] ? "stored" : "none");
}

void host_wifi_apply(const WifiCmd* c) {
    switch (c->kind) {
    case WIFI_CMD_SET:
        // The app may resend the same creds: only a real change drops the link.
        if (strcmp(ssid, c->ssid) || strcmp(pass, c->pass) || strcmp(key, c->key)) restart = true;
        strcpy(ssid, c->ssid);
        strcpy(pass, c->pass);
        strcpy(key, c->key);
        enabled = c->en;
        break;
    case WIFI_CMD_ENABLE:
        enabled = c->en;
        break;
    case WIFI_CMD_FORGET:
        ssid[0] = pass[0] = key[0] = '\0';
        enabled = false;
        break;
    }
    save();
    // The ack goes out before the next tick: report the attempt that is about
    // to start, not the previous result (a stale "fail" read as a failure).
    // restart makes the tick begin it from OFF, so the radio is started.
    if (enabled && ssid[0] && (restart || st == WIFI_ST_OFF)) {
        restart = true;
        set_st(WIFI_ST_CONNECTING);
    }
}

static void go_down(void) {
    tcp_host_stop();
    radio_mdns_stop();
    ip[0] = '\0';
}

static void begin_attempt(void) {
    n_tries++;
    Serial.printf("WiFi: joining \"%s\" (attempt %u)\n", ssid, attempt + 1);
    radio_begin(ssid, pass);
    since_ms = millis();
    set_st(WIFI_ST_CONNECTING);
}

static void fail(const char* why) {
    go_down();
    n_fail++;
    const uint32_t wait = wifi_backoff_ms(attempt++);
    since_ms = millis() + wait;
    Serial.printf("WiFi: %s, retry in %lus\n", why, (unsigned long)(wait / 1000));
    set_st(WIFI_ST_FAIL);
}

void host_wifi_tick(void (*on_line)(const char* line)) {
    if (restart && st != WIFI_ST_OFF) {
        go_down();
        radio_off();
        set_st(WIFI_ST_OFF);
    }
    restart = false;
    if (!enabled || !ssid[0]) {
        if (st != WIFI_ST_OFF) {
            go_down();
            radio_off();
            set_st(WIFI_ST_OFF);
        }
        return;
    }
    switch (st) {
    case WIFI_ST_OFF:
        attempt = 0;
        begin_attempt();
        break;
    case WIFI_ST_CONNECTING: {
        const int r = radio_poll();
        if (r > 0) {
            attempt = 0;
            radio_ip(ip, sizeof(ip));
            radio_mdns_start(ble_get_device_name(), ble_get_mac_address(), WIFI_TCP_PORT);
            tcp_host_start(WIFI_TCP_PORT);
            ok_since_ms = millis();
            Serial.printf("WiFi: up, ip %s, after %lums\n", ip, (unsigned long)(ok_since_ms - since_ms));
            set_st(WIFI_ST_OK);
        } else if (r < 0) {
            fail("join refused (wrong password, network not found or lost)");
        } else if (millis() - since_ms >= CONNECT_TIMEOUT_MS) {
            fail("no connection within 20 s");
        }
        break;
    }
    case WIFI_ST_FAIL:
        if ((int32_t)(millis() - since_ms) >= 0) begin_attempt();
        break;
    case WIFI_ST_OK:
        if (radio_poll() <= 0) {
            n_lost++;
            Serial.printf("WiFi: link lost after %lus (#%lu)\n",
                (unsigned long)((millis() - ok_since_ms) / 1000), (unsigned long)n_lost);
            fail("link lost");
            break;
        }
        tcp_host_tick(key, on_line);
        break;
    }
}

bool host_wifi_client(void) { return st == WIFI_ST_OK && tcp_host_authed(); }
bool host_wifi_has_creds(void) { return ssid[0] != 0; }

void host_wifi_send(const char* line) {
    if (st == WIFI_ST_OK) tcp_host_send(line);
}

WifiSt host_wifi_state(void) { return st; }

size_t host_wifi_status(char* buf, size_t cap) {
    return wifi_status_json(buf, cap, st, ip, ssid);
}

size_t host_wifi_diag(char* buf, size_t cap) {
    const TcpStats* t = tcp_host_stats();
    const long up = st == WIFI_ST_OK ? (long)((millis() - ok_since_ms) / 1000) : -1;
    const long rx_age = t->last_line_ms ? (long)((millis() - t->last_line_ms) / 1000) : -1;
    const int n = snprintf(buf, cap,
        "{\"try\":%lu,\"fail\":%lu,\"lost\":%lu,\"up\":%ld,\"tcp\":%lu,\"auth\":%lu,"
        "\"authf\":%lu,\"autht\":%lu,\"closed\":%lu,\"idle\":%lu,\"wrf\":%lu,\"rxage\":%ld}",
        (unsigned long)n_tries, (unsigned long)n_fail, (unsigned long)n_lost, up,
        (unsigned long)t->accepts, (unsigned long)t->authed, (unsigned long)t->auth_fail,
        (unsigned long)t->auth_timeout, (unsigned long)t->closed, (unsigned long)t->idle,
        (unsigned long)t->write_fail, rx_age);
    return n > 0 && (size_t)n < cap ? (size_t)n : 0;
}

bool host_wifi_take_changed(void) {
    const bool c = changed;
    changed = false;
    return c;
}
#endif
