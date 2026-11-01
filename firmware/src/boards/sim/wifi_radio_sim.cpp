// Simulator radio layer for host_wifi.cpp: no WiFi, the "network" is the
// host's loopback and the link comes up at once, so the real TCP server
// (host_tcp.cpp, POSIX sockets) can be driven end to end on 127.0.0.1:7363.
// SIM_WIFI_KEY=<32 hex> provisions it at boot (ssid "sim"); otherwise a
// {"wifi":{...}} line does, as on hardware.
#include "../../host_wifi.h"
#include <Arduino.h>
#include <Preferences.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>

static bool up = false;

void radio_init(void) {
    signal(SIGPIPE, SIG_IGN);   // a host closing mid-write must not kill the sim
    const char* k = getenv("SIM_WIFI_KEY");
    if (!k || !*k) return;
    Preferences p;
    p.begin("wifi", false);
    p.putString("ssid", "sim");
    p.putString("pass", "");
    p.putString("key", k);
    p.putBool("en", true);
    p.end();
}

void radio_begin(const char* ssid, const char*) {
    printf("[sim] WiFi join '%s' (instant)\n", ssid);
    up = true;
}
int radio_poll(void) { return up ? 1 : 0; }
void radio_ip(char* buf, size_t cap) { snprintf(buf, cap, "127.0.0.1"); }
void radio_off(void) { up = false; }
void radio_mdns_start(const char* instance, const char* id, unsigned port) {
    printf("[sim] mDNS (not advertised) _clawdmeter._tcp '%s' port %u id=%s\n", instance, port, id);
}
void radio_mdns_stop(void) {}
