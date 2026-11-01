#include "host_wifi.h"

#if !BOARD_HAS_WIFI
void host_wifi_init(void) {}
void host_wifi_tick(void (*)(const char*)) {}
void host_wifi_apply(const WifiCmd*) {}
bool host_wifi_client(void) { return false; }
void host_wifi_send(const char*) {}
WifiSt host_wifi_state(void) { return WIFI_ST_OFF; }
size_t host_wifi_status(char*, size_t) { return 0; }
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

static void set_st(WifiSt s) {
    if (s != st) changed = true;
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
}

static void go_down(void) {
    tcp_host_stop();
    radio_mdns_stop();
    ip[0] = '\0';
}

static void begin_attempt(void) {
    radio_begin(ssid, pass);
    since_ms = millis();
    set_st(WIFI_ST_CONNECTING);
}

static void fail(void) {
    go_down();
    since_ms = millis() + wifi_backoff_ms(attempt++);
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
            set_st(WIFI_ST_OK);
        } else if (r < 0 || millis() - since_ms >= CONNECT_TIMEOUT_MS) {
            fail();
        }
        break;
    }
    case WIFI_ST_FAIL:
        if ((int32_t)(millis() - since_ms) >= 0) begin_attempt();
        break;
    case WIFI_ST_OK:
        if (radio_poll() <= 0) {
            Serial.println("WiFi: link lost");
            fail();
            break;
        }
        tcp_host_tick(key, on_line);
        break;
    }
}

bool host_wifi_client(void) { return st == WIFI_ST_OK && tcp_host_authed(); }

void host_wifi_send(const char* line) {
    if (st == WIFI_ST_OK) tcp_host_send(line);
}

WifiSt host_wifi_state(void) { return st; }

size_t host_wifi_status(char* buf, size_t cap) {
    return wifi_status_json(buf, cap, st, ip, ssid);
}

bool host_wifi_take_changed(void) {
    const bool c = changed;
    changed = false;
    return c;
}
#endif
