#include "clawd_prefs.h"
#include "clawd_director.h"
#include "clawd_policy.h"
#include "ui.h"
#include "hal/sound_hal.h"
#include <Preferences.h>
#include <Arduino.h>

void clawd_prefs_init(void) {
    Preferences prefs;
    prefs.begin("clawdmeter", true);
    const int lively = prefs.getUChar("lively", CLAWD_LIVELINESS_DEFAULT);
    const int attn = prefs.getUChar("attn_anim", CLAWD_ATTN_ANIM_DEFAULT_S);
    const bool pace = prefs.getUChar("pace_mark", 0) != 0;
    const int level = prefs.getUChar("chime_lvl", 100);
    prefs.end();
    chime_level_set(level);
    director_set_liveliness(clawd_liveliness_clamp(lively));
    director_set_attn_anim(clawd_attn_anim_clamp(attn));
    pace_marker_set(pace);
    Serial.printf("Device prefs init: liveliness %d, attention animation %d s, pace marker %s\n",
                  clawd_liveliness(), clawd_attn_anim(), pace ? "on" : "off");
}

static void save(const char* key, uint8_t v) {
    Preferences prefs;
    prefs.begin("clawdmeter", false);
    prefs.putUChar(key, v);
    prefs.end();
}

void liveliness_set(int level) {
    level = clawd_liveliness_clamp(level);
    if (level == clawd_liveliness()) return;
    save("lively", (uint8_t)level);
    director_set_liveliness(level);
    Serial.printf("Liveliness: %d\n", level);
}

int liveliness_get(void) { return clawd_liveliness(); }

void attn_anim_set(int seconds) {
    seconds = clawd_attn_anim_clamp(seconds);
    if (seconds == clawd_attn_anim()) return;
    save("attn_anim", (uint8_t)seconds);
    director_set_attn_anim(seconds);
    Serial.printf("Attention animation: %d s\n", seconds);
}

int attn_anim_get(void) { return clawd_attn_anim(); }

static bool pace_marker = false;

void pace_marker_set(bool on) {
    ui_set_pace_marker(on);
    if (on == pace_marker) return;
    pace_marker = on;
    save("pace_mark", on ? 1 : 0);
    Serial.printf("Pace marker: %s\n", on ? "on" : "off");
}

bool pace_marker_get(void) { return pace_marker; }

static int chime_level = -1;   // -1 until the saved level is applied

void chime_level_set(int pct) {
    pct = pct < 0 ? 0 : pct > 100 ? 100 : pct;
    if (pct == chime_level) return;
    const bool first = chime_level < 0;
    chime_level = pct;
    sound_hal_set_level(pct);
    if (first) return;   // loaded from NVS: nothing to save
    save("chime_lvl", (uint8_t)pct);
    Serial.printf("Chime level: %d %%\n", pct);
}

int chime_level_get(void) { return chime_level; }
