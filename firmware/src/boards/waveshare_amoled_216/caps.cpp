#include "../../hal/board_caps.h"
#include "board.h"

static const BoardCaps caps = {
    .name = BOARD_NAME,
    .width = LCD_WIDTH,
    .height = LCD_HEIGHT,
    .button_count = 2,
    .has_rotation = true,
    .has_battery = true,
    .has_imu = true,
    .minimal_ui   = false,   // full animated UI; true for e-ink / slow panels
    .has_pwr_button = true,  // AXP2101 PKEY
    .has_touch = true,
};

const BoardCaps& board_caps(void) { return caps; }

#include <Arduino.h>
unsigned long board_hal_free_heap(void) { return ESP.getFreeHeap(); }

#include <esp_system.h>
const char* board_hal_reset_reason(void) {
    switch (esp_reset_reason()) {
    case ESP_RST_POWERON:   return "power";
    case ESP_RST_SW:        return "sw";
    case ESP_RST_PANIC:     return "panic";
    case ESP_RST_INT_WDT:
    case ESP_RST_TASK_WDT:
    case ESP_RST_WDT:       return "wdt";
    case ESP_RST_BROWNOUT:  return "brownout";
    case ESP_RST_DEEPSLEEP: return "deepsleep";
    case ESP_RST_USB:       return "usb";
    case ESP_RST_EXT:       return "ext";
    default:                return "other";
    }
}

#include "../../hal/board_info_hal.h"
#include <esp_heap_caps.h>
bool board_hal_heap(BoardHeap* h) {
    h->free       = heap_caps_get_free_size(MALLOC_CAP_INTERNAL);
    h->min_free   = heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL);
    h->largest    = heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL);
    h->psram_free = heap_caps_get_free_size(MALLOC_CAP_SPIRAM);
    h->psram_min  = heap_caps_get_minimum_free_size(MALLOC_CAP_SPIRAM);
    return true;
}

// RTC slow memory isn't cleared by a panic, watchdog or software reset; the
// magic tells a kept value from power-on garbage. Read before setup() runs.
#include <esp_attr.h>
#define UPTIME_MAGIC 0xC1A3D0B7u
static RTC_NOINIT_ATTR uint32_t rtc_magic;
static RTC_NOINIT_ATTR uint32_t rtc_uptime_s;
static const long prev_uptime = rtc_magic == UPTIME_MAGIC ? (long)rtc_uptime_s : -1;

void board_hal_note_uptime(unsigned long seconds) {
    rtc_uptime_s = (uint32_t)seconds;
    rtc_magic = UPTIME_MAGIC;
}

long board_hal_prev_uptime(void) { return prev_uptime; }
