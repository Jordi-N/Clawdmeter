// ESP32 radio layer for host_wifi.cpp: station mode and mDNS. Every call is
// non-blocking except the first WiFi.mode(), which brings the driver up once.
#include "host_wifi.h"
#if BOARD_HAS_WIFI
#include <WiFi.h>
#include <ESPmDNS.h>
#include <string.h>

static bool mdns_up = false;

void radio_init(void) {
    WiFi.persistent(false);   // creds live in our own "wifi" NVS namespace
}

void radio_begin(const char* ssid, const char* pass) {
    if (WiFi.getMode() & WIFI_MODE_STA) WiFi.disconnect();
    WiFi.setHostname("clawdmeter");
    WiFi.mode(WIFI_STA);
    WiFi.setAutoReconnect(false);   // host_wifi.cpp retries with its own backoff
    // BLE runs alongside: the coexistence arbiter needs modem sleep on.
    WiFi.setSleep(true);
    WiFi.begin(ssid, pass[0] ? pass : nullptr);
}

int radio_poll(void) {
    switch (WiFi.status()) {
    case WL_CONNECTED:       return 1;
    case WL_CONNECT_FAILED:
    case WL_NO_SSID_AVAIL:
    case WL_CONNECTION_LOST: return -1;
    default:                 return 0;
    }
}

void radio_ip(char* buf, size_t cap) {
    snprintf(buf, cap, "%s", WiFi.localIP().toString().c_str());
}

void radio_off(void) {
    radio_mdns_stop();
    WiFi.disconnect(true);
    WiFi.mode(WIFI_OFF);
}

void radio_mdns_start(const char* instance, const char* id, unsigned port) {
    if (mdns_up) return;
    // Unique host name from the MAC tail ("44:BD:8D:77:AF:11" -> clawdmeter-af11).
    char host[24] = "clawdmeter";
    const size_t n = strlen(id);
    if (n == 17) {
        snprintf(host, sizeof(host), "clawdmeter-%c%c%c%c", id[12], id[13], id[15], id[16]);
        for (char* c = host; *c; c++) if (*c >= 'A' && *c <= 'F') *c += 32;
    }
    if (!MDNS.begin(host)) {
        Serial.println("WiFi: mDNS start failed");
        return;
    }
    MDNS.setInstanceName(instance);
    MDNS.addService("clawdmeter", "tcp", port);
    MDNS.addServiceTxt("clawdmeter", "tcp", "id", id);
    mdns_up = true;
    Serial.printf("WiFi: mDNS %s.local, _clawdmeter._tcp %u id=%s\n", host, port, id);
}

void radio_mdns_stop(void) {
    if (!mdns_up) return;
    MDNS.end();
    mdns_up = false;
}
#endif
