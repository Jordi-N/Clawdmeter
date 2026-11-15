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
