#pragma once

// Optional board identity hooks. Shared code provides weak defaults
// (board_info_default.cpp); a board overrides only what it knows.

// Hardware revision of the board, as a short stable token for the `info`
// serial command and the desktop flasher ("" when the board has a single
// revision or can't tell). Valid after board_init(). Must be a static string.
//   waveshare_amoled_18: "sh8601_ft3168" | "co5300_cst816"
const char* board_hal_rev(void);

// Free heap in bytes for the `info` line (0 when the board can't tell).
unsigned long board_hal_free_heap(void);

// Why the chip last started, as a short token for `info` and the boot log
// ("power", "sw", "panic", "wdt", "brownout", "usb", "deepsleep"; "" when
// the board can't tell). Must be a static string.
const char* board_hal_reset_reason(void);

// Heap figures for the health log and `info` (bytes). Internal RAM: free,
// lowest free since boot, largest free block (fragmentation); PSRAM: free and
// lowest free (0 without PSRAM). Returns false, all zero, when the board can't
// tell (weak default).
struct BoardHeap {
    unsigned long free, min_free, largest, psram_free, psram_min;
};
bool board_hal_heap(BoardHeap* out);

// Uptime that survives a reset which keeps the chip powered (panic, watchdog,
// software, sometimes brownout): main.cpp notes it every second, and the next
// boot reads how long the previous run lasted. -1 when unknown (power-on, or
// a board without retained RAM; weak default).
void board_hal_note_uptime(unsigned long seconds);
long board_hal_prev_uptime(void);
