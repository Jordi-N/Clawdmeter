#pragma once
#include <stddef.h>
#include <stdint.h>

enum ble_state_t {
    BLE_STATE_INIT,
    BLE_STATE_ADVERTISING,
    BLE_STATE_CONNECTED,
    BLE_STATE_DISCONNECTED,
};

void ble_init(void);
void ble_tick(void);
ble_state_t ble_get_state(void);
// An encrypted link to the owner is up. A central with a stale bond (it
// still has keys the board dropped) connects, fails encryption and leaves
// every few seconds: the UI waits for this, so it doesn't flicker.
bool ble_link_secure(void);
bool ble_has_bond(void);   // some machine is bonded (BLE can come back by itself)
const char* ble_get_device_name(void);
const char* ble_get_mac_address(void);
void ble_clear_bonds(void);
void ble_print_status(void);   // serial `ble`: state, bonds, owner, peers
void ble_disconnect_all(void); // serial `disconnect`: drop every link
// Link counters since boot, as one JSON object for `info` and `ble`:
// con/drop connects and disconnects, noenc links that left before encryption
// (not closed by us; the stale-bond pattern), encf auth completed without
// encryption, adv/advf advertising starts and failures, rx accepted writes,
// rxrej writes refused (unencrypted or non-owner), cut writes longer than the
// buffer, ovr lines lost to a full queue, maxw longest write (B), rsn last
// disconnect reason, rxage seconds since the last accepted write (-1 none),
// and rssi (dBm) / mtu of the encrypted link when there is one.
// Returns the length, 0 if it doesn't fit (or in the simulator).
size_t ble_diag_json(char* buf, size_t cap);
// false: stop advertising and drop every link (the app is on USB or WiFi);
// true: advertise again. Idempotent; the pair gesture still clears bonds.
void ble_set_radio(bool on);
bool ble_has_data(void);
// The latest payload, copied for the caller; valid until the next call.
const char* ble_get_data(void);
// Any JSON line on TX (notify), e.g. the WiFi status; dropped when not connected.
void ble_send_line(const char* line);
void ble_send_dismiss(void);   // TX {"dismiss":true}: the user ended the alert on the device
void ble_request_refresh(void);

void ble_set_battery_level(int pct);

// BLE HID keyboard
void ble_keyboard_press(uint8_t key, uint8_t modifier);
void ble_keyboard_release(void);
