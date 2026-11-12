#include "hal/board_info_hal.h"
#include "hal/power_hal.h"
#include "hal/sound_hal.h"

// Weak default: boards with one hardware revision don't need to implement it.
__attribute__((weak)) const char* board_hal_rev(void) { return ""; }

// Weak defaults: the sim and boards that don't say.
__attribute__((weak)) unsigned long board_hal_free_heap(void) { return 0; }
__attribute__((weak)) void sound_hal_set_level(int) {}

// Weak default: boards without a PMU that can cut the power.
__attribute__((weak)) bool power_hal_shutdown(void) { return false; }
