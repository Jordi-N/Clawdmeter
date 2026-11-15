#pragma once

// Device settings from the app, kept in NVS so they apply from boot, before
// the app connects. The app sends them in every data payload.
//   liveliness       `lv`  1 No-nonsense .. 5 All-nonsense (clawd_policy.h)
//   attention anim   `aa`  seconds he points and waves for an alert; 0 = off
//   pace marker      `pm`  1 = pace marker on the usage bars (pace.h)
//   chime level      `vol` 0-100 % of the board's tuned ding (sound_hal_set_level)
//   battery care     `bc`  0 full, 1 4.1 V, 2 stop at 95 % (power_hal_set_battery_care)

void clawd_prefs_init(void);          // after director_init(): load and apply the saved values
void liveliness_set(int level);       // apply; saved only when it changes
int  liveliness_get(void);
void attn_anim_set(int seconds);      // apply; saved only when it changes
int  attn_anim_get(void);
void pace_marker_set(bool on);      // apply; saved only when it changes
bool pace_marker_get(void);
void chime_level_set(int pct);      // apply; saved only when it changes
void battery_care_set(int mode);    // `bc` 0-2 (power_hal.h); saved only when it changes
int  chime_level_get(void);

// Boot statistics in NVS: boots so far (this one included) and how many of
// them followed a panic, watchdog or brownout. Written once per boot.
void boot_stats_init(const char* reset_reason);   // early in setup(), once
unsigned long boot_count(void);
unsigned long crash_count(void);
