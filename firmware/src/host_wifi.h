#pragma once
#include <stddef.h>
#include "wifi_proto.h"

// WiFi (LAN) transport: the same JSON lines as BLE, over TCP, for when the
// computer is out of Bluetooth range. Provisioned with the `wifi` key on the
// BLE channel; station mode, mDNS `_clawdmeter._tcp`, one authenticated TCP
// client (docs/protocol.md, "WiFi transport"). Compiled in only where the
// env sets -DBOARD_HAS_WIFI=1 (docs/porting/capability-flags.md); elsewhere
// every call is a no-op stub and the status is empty.
#ifndef BOARD_HAS_WIFI
#define BOARD_HAS_WIFI 0
#endif

void host_wifi_init(void);   // loads the stored creds; connects if enabled
// Reconnects with backoff and serves TCP; each authenticated line goes to
// on_line (main.cpp's shared dispatch). Never blocks the loop.
void host_wifi_tick(void (*on_line)(const char* line));
// A parsed `wifi` command. Stores it now; radio changes land on the next tick.
void host_wifi_apply(const WifiCmd* cmd);
bool host_wifi_client(void);           // an authenticated TCP host is attached
void host_wifi_send(const char* line); // JSON line to that host, if any
WifiSt host_wifi_state(void);
// The status object {"st":...,"ip":...,"ssid":...}; 0 when compiled out.
size_t host_wifi_status(char* buf, size_t cap);
bool host_wifi_take_changed(void);     // status changed since the last call

// Radio layer under host_wifi.cpp: wifi_radio_esp.cpp on hardware,
// boards/sim/wifi_radio_sim.cpp in the simulator.
void radio_init(void);
void radio_begin(const char* ssid, const char* pass);
int  radio_poll(void);                  // 1 connected, 0 connecting, -1 failed
void radio_ip(char* buf, size_t cap);
void radio_off(void);
void radio_mdns_start(const char* instance, const char* id, unsigned port);
void radio_mdns_stop(void);
