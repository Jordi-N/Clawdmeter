#pragma once
// In-memory stand-in for ESP32 NVS Preferences (brightness, prefs, WiFi).
// State lasts for the process lifetime only — the sim boots with defaults.
#include <stdint.h>
#include <stddef.h>

class Preferences {
public:
    bool begin(const char* name, bool read_only = false) {
        (void)name; (void)read_only;
        return true;
    }
    void end(void) {}
    uint8_t getUChar(const char* key, uint8_t def = 0);
    size_t  putUChar(const char* key, uint8_t value);
    // Strings and bools for the WiFi creds (host_wifi.cpp). Namespaces are
    // ignored here, as for UChar: keys must stay unique across the firmware.
    size_t  getString(const char* key, char* value, size_t max_len);
    size_t  putString(const char* key, const char* value);
    bool    getBool(const char* key, bool def = false) { return getUChar(key, def ? 1 : 0) != 0; }
    size_t  putBool(const char* key, bool value) { return putUChar(key, value ? 1 : 0); }
    bool    remove(const char* key);
};
