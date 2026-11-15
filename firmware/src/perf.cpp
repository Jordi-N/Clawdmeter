#include "perf.h"
#include "clawd_prefs.h"
#include "hal/board_info_hal.h"
#include "hal/power_hal.h"
#include <Arduino.h>
#include <stdio.h>

static const char* const NAMES[PERF_WINDOW_COUNT] = { "face-rise", "face-settle", "face-drop", "perform" };

struct PerfStats {
    bool     done;          // a complete (or cut-short) run is recorded
    bool     cut;           // another window started before this one ended
    uint32_t start_ms, dur_ms;
    uint32_t frames;
    uint64_t refr_us, flush_us, px;
    uint32_t refr_max_us;
    uint32_t gap_max_ms;    // longest wait between two frames (or from the start)
};

static PerfStats stats[PERF_WINDOW_COUNT];
static int       open_w = -1;
static uint32_t  last_frame_ms = 0;
static uint32_t  refr_t0 = 0, flush_t0 = 0;
static uint32_t  frame_px = 0, frame_flush_us = 0;
static uint32_t  panel_px = 1;
static uint32_t  period_ms = 0;

static void on_event(lv_event_t* e) {
    if (open_w < 0) return;
    const lv_event_code_t code = lv_event_get_code(e);
    if (code == LV_EVENT_REFR_START) {
        refr_t0 = micros();
        frame_px = 0;
        frame_flush_us = 0;
    } else if (code == LV_EVENT_FLUSH_START) {
        const lv_area_t* a = (const lv_area_t*)lv_event_get_param(e);
        frame_px += lv_area_get_size(a);
        flush_t0 = micros();
    } else if (code == LV_EVENT_FLUSH_FINISH) {
        frame_flush_us += micros() - flush_t0;
    } else if (code == LV_EVENT_REFR_READY) {
        if (!frame_px) return;                  // nothing was invalid
        PerfStats& s = stats[open_w];
        const uint32_t us = micros() - refr_t0;
        const uint32_t now = millis();
        s.frames++;
        s.refr_us += us;
        s.flush_us += frame_flush_us;
        s.px += frame_px;
        if (us > s.refr_max_us) s.refr_max_us = us;
        if (now - last_frame_ms > s.gap_max_ms) s.gap_max_ms = now - last_frame_ms;
        last_frame_ms = now;
    }
}

void perf_attach(lv_display_t* disp, uint32_t refr_period_ms) {
    period_ms = refr_period_ms;
    panel_px = (uint32_t)lv_display_get_horizontal_resolution(disp) *
               (uint32_t)lv_display_get_vertical_resolution(disp);
    if (!panel_px) panel_px = 1;
    lv_display_add_event_cb(disp, on_event, LV_EVENT_REFR_START, NULL);
    lv_display_add_event_cb(disp, on_event, LV_EVENT_FLUSH_START, NULL);
    lv_display_add_event_cb(disp, on_event, LV_EVENT_FLUSH_FINISH, NULL);
    lv_display_add_event_cb(disp, on_event, LV_EVENT_REFR_READY, NULL);
}

static void close_open(bool cut) {
    if (open_w < 0) return;
    PerfStats& s = stats[open_w];
    s.dur_ms = millis() - s.start_ms;
    s.done = true;
    s.cut = cut;
    open_w = -1;
}

void perf_begin(PerfWindow w) {
    close_open(true);
    PerfStats& s = stats[w];
    s = PerfStats();
    s.start_ms = millis();
    last_frame_ms = s.start_ms;
    open_w = w;
}

void perf_end(PerfWindow w) {
    if (open_w == (int)w) close_open(false);
}

void perf_print(void) {
    Serial.printf("perf: refresh period %lu ms\n", (unsigned long)period_ms);
    for (int i = 0; i < PERF_WINDOW_COUNT; i++) {
        const PerfStats& s = stats[i];
        if (!s.done) { Serial.printf("perf %s: no run yet\n", NAMES[i]); continue; }
        const uint32_t f = s.frames ? s.frames : 1;
        Serial.printf("perf %s: %lu ms%s, %lu frames (%.1f fps), longest gap %lu ms; "
                      "refresh avg %.1f ms (max %.1f), flush avg %.1f ms; "
                      "%lu px/frame (%.1f%% of the panel)\n",
                      NAMES[i], (unsigned long)s.dur_ms, s.cut ? " (cut short)" : "",
                      (unsigned long)s.frames,
                      s.dur_ms ? s.frames * 1000.0f / s.dur_ms : 0.0f,
                      (unsigned long)s.gap_max_ms,
                      s.refr_us / 1000.0f / f, s.refr_max_us / 1000.0f,
                      s.flush_us / 1000.0f / f,
                      (unsigned long)(s.px / f), 100.0f * (float)(s.px / f) / panel_px);
    }
}

// ---- System health ----

#define HEALTH_PERIOD_MS   600000   // the periodic health line: every 10 min
#define STALL_MS           500      // a loop this long froze input and the screen
#define STALL_LOG_EVERY_MS 60000    // at most one stall line a minute (all are counted)
#define HEAP_CHECK_MS      10000

static uint32_t loop_max, lv_max;           // since the last health line
static uint32_t loop_max_boot;              // since boot, for `info`
static uint32_t stalls, stall_worst, stall_logged_ms;
static uint32_t health_ms, heap_check_ms, uptime_noted_ms;

// Internal RAM low-water marks (BLE, WiFi and the task stacks live there) and
// a PSRAM one. Each threshold logs once per boot; the values are first
// guesses, to tune against the baseline the boot health line shows.
static const unsigned long HEAP_ALARMS[] = { 65536, 32768, 16384 };
#define HEAP_ALARM_COUNT (sizeof(HEAP_ALARMS) / sizeof(HEAP_ALARMS[0]))
#define BLOCK_ALARM      16384UL     // largest free internal block: fragmentation
#define PSRAM_ALARM      1048576UL
static unsigned heap_alarmed;        // thresholds already logged
static bool     block_alarmed, psram_alarmed;

static void heap_check(void) {
    BoardHeap h;
    if (!board_hal_heap(&h)) return;
    while (heap_alarmed < HEAP_ALARM_COUNT && h.min_free < HEAP_ALARMS[heap_alarmed]) {
        Serial.printf("Heap: internal RAM fell below %lu KB (min %lu, now %lu free, largest block %lu)\n",
                      HEAP_ALARMS[heap_alarmed] / 1024, h.min_free, h.free, h.largest);
        heap_alarmed++;
    }
    // Fragmentation: memory free but no big block. Re-armed once it recovers.
    if (!block_alarmed && h.largest < BLOCK_ALARM) {
        block_alarmed = true;
        Serial.printf("Heap: largest internal block %lu B (%lu free): fragmented\n", h.largest, h.free);
    } else if (block_alarmed && h.largest >= 2 * BLOCK_ALARM) {
        block_alarmed = false;
    }
    if (!psram_alarmed && h.psram_free && h.psram_min < PSRAM_ALARM) {
        psram_alarmed = true;
        Serial.printf("Heap: PSRAM fell below %lu KB (min %lu, now %lu free)\n", PSRAM_ALARM / 1024,
                      h.psram_min, h.psram_free);
    }
}

void health_print(const char* why) {
    BoardHeap h;
    const bool heap = board_hal_heap(&h);
    char pwr[112];
    const size_t pn = power_hal_health(pwr, sizeof(pwr));
    Serial.printf("Health (%s): up %lu s", why, (unsigned long)(millis() / 1000));
    if (heap)
        Serial.printf(", heap %lu free (min %lu, block %lu), psram %lu (min %lu)", h.free, h.min_free,
                      h.largest, h.psram_free, h.psram_min);
    Serial.printf(", loop max %lu ms (LVGL %lu), stalls %lu (worst %lu ms)%s%s\n", (unsigned long)loop_max,
                  (unsigned long)lv_max, (unsigned long)stalls, (unsigned long)stall_worst, pn ? "; " : "",
                  pn ? pwr : "");
}

void health_loop(uint32_t loop_ms, uint32_t lv_ms) {
    const uint32_t now = millis();
    if (loop_ms > loop_max) loop_max = loop_ms;
    if (loop_ms > loop_max_boot) loop_max_boot = loop_ms;
    if (lv_ms > lv_max) lv_max = lv_ms;
    if (loop_ms >= STALL_MS) {
        stalls++;
        if (loop_ms > stall_worst) stall_worst = loop_ms;
        if (stalls == 1 || now - stall_logged_ms >= STALL_LOG_EVERY_MS) {
            stall_logged_ms = now;
            Serial.printf("Stall: a loop took %lu ms (LVGL %lu ms); %lu since boot, worst %lu ms\n",
                          (unsigned long)loop_ms, (unsigned long)lv_ms, (unsigned long)stalls,
                          (unsigned long)stall_worst);
        }
    }
    if (now - uptime_noted_ms >= 1000) {
        uptime_noted_ms = now;
        board_hal_note_uptime(now / 1000);
    }
    if (now - heap_check_ms >= HEAP_CHECK_MS) {
        heap_check_ms = now;
        heap_check();
    }
    if (now - health_ms >= HEALTH_PERIOD_MS) {
        health_ms = now;
        health_print("periodic");
        loop_max = 0;
        lv_max = 0;
    }
}

size_t health_info(char* buf, size_t cap) {
    BoardHeap h;
    board_hal_heap(&h);   // all zero when the board can't tell
    const int n = snprintf(buf, cap,
                           ",\"boots\":%lu,\"crash\":%lu,\"pup\":%ld,\"hmin\":%lu,\"hblk\":%lu,\"psr\":%lu,"
                           "\"lmax\":%lu,\"stall\":%lu,\"mv\":%d",
                           boot_count(), crash_count(), board_hal_prev_uptime(), h.min_free, h.largest,
                           h.psram_free, (unsigned long)loop_max_boot, (unsigned long)stalls,
                           power_hal_battery_mv());
    return n > 0 && (size_t)n < cap ? (size_t)n : 0;
}
