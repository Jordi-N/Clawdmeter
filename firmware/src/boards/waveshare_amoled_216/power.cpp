#include "../../hal/power_hal.h"
#include "board.h"
#include <Arduino.h>
#include <Wire.h>
#include <XPowersLib.h>

// PWR button comes from AXP2101 PKEY IRQs:
//   SHORT    — quick tap (cycle brightness)
//   LONG     — ~1.5s mark, starts the hold-to-pair countdown
//   POSITIVE — release edge, completes/cancels the gesture

#define BATTERY_POLL_MS  2000
#define CHARGING_POLL_MS 500
#define PWR_POLL_MS      50

static XPowersPMU pmu;
static bool       pmu_ok = false;   // begin() succeeded

static int      cached_pct        = -1;
static bool     cached_charging   = false;
static bool     cached_vbus       = false;
static bool     pwr_pressed_flag  = false;
static bool     pwr_long_flag     = false;
static bool     pwr_released_flag = false;
static uint32_t last_battery_ms   = 0;
static uint32_t last_charging_ms  = 0;
static uint32_t last_pwr_ms       = 0;
static int      care_mode         = 0;      // power_hal_set_battery_care
static bool     charge_on         = true;   // the charger is enabled

void power_hal_init(void) {
    if (!pmu.begin(Wire, AXP2101_ADDR, IIC_SDA, IIC_SCL)) {
        Serial.println("AXP2101 init failed");
        return;
    }
    Serial.println("AXP2101 init OK");
    pmu_ok = true;

    pmu.enableBattDetection();
    pmu.enableBattVoltageMeasure();

    // The AXP2101 estimates % from the battery voltage and can latch into a
    // bad state (frozen, or stuck near zero) that a power cycle doesn't
    // clear: the gauge lives in the always-on domain the battery keeps
    // alive. Reset it at boot so it recalibrates from the current voltage.
    pmu.enableGauge();
    pmu.resetGauge();

    pmu.disableIRQ(XPOWERS_AXP2101_ALL_IRQ);
    pmu.clearIrqStatus();
    pmu.enableIRQ(XPOWERS_AXP2101_PKEY_SHORT_IRQ
                | XPOWERS_AXP2101_PKEY_LONG_IRQ
                | XPOWERS_AXP2101_PKEY_POSITIVE_IRQ);

    // Default press-off (force-shutdown) is 6s, only 2s after the pair gesture
    // arms at ~3s and disarms at ~6s. Bump to 8s so a slightly-too-long hold
    // doesn't shut the device down mid-gesture.
    pmu.setPowerKeyPressOffTime(XPOWERS_POWEROFF_8S);

    cached_charging = pmu.isCharging();
    cached_vbus     = pmu.isVbusIn();
    cached_pct = pmu.getBatteryPercent();
}

// Mode 2 keeps 90-95 %: the charger stops at 95 % and starts again at 90 %
// (the gauge's estimate). Without USB nothing charges anyway: it's re-armed.
static void care_tick(void) {
    bool want = true;
    if (care_mode == 2 && cached_vbus && cached_pct >= 0)
        want = charge_on ? cached_pct < 95 : cached_pct <= 90;
    if (want == charge_on) return;
    charge_on = want;
    if (want) pmu.enableCellbatteryCharge();
    else pmu.disableCellbatteryCharge();
    Serial.printf("Battery care: charging %s at %d %%\n", want ? "resumed" : "paused", cached_pct);
}

void power_hal_set_battery_care(int mode) {
    if (!pmu_ok) return;
    care_mode = mode;
    // 4.1 V tops out near 90 %; the other modes charge to the usual 4.2 V.
    pmu.setChargeTargetVoltage(mode == 1 ? XPOWERS_AXP2101_CHG_VOL_4V1 : XPOWERS_AXP2101_CHG_VOL_4V2);
    charge_on = true;
    pmu.enableCellbatteryCharge();
    care_tick();
}

void power_hal_tick(void) {
    uint32_t now = millis();

    if (now - last_charging_ms >= CHARGING_POLL_MS) {
        last_charging_ms = now;
        cached_charging = pmu.isCharging();
        cached_vbus     = pmu.isVbusIn();
    }
    if (now - last_battery_ms >= BATTERY_POLL_MS) {
        last_battery_ms = now;
        cached_pct = pmu.getBatteryPercent();
        if (pmu_ok) care_tick();
    }
    if (now - last_pwr_ms >= PWR_POLL_MS) {
        last_pwr_ms = now;
        // Read, and clear only when something fired: one I2C read per poll
        // instead of a read and three writes (the bus is shared with touch).
        if (pmu.getIrqStatus()) {
            if (pmu.isPekeyShortPressIrq())    pwr_pressed_flag  = true;
            if (pmu.isPekeyLongPressIrq())     pwr_long_flag     = true;
            if (pmu.isPekeyPositiveIrq())      pwr_released_flag = true;
            pmu.clearIrqStatus();
        }
    }
}

int  power_hal_battery_pct(void) { return cached_pct; }
bool power_hal_is_charging(void) { return cached_charging; }
bool power_hal_is_vbus_in(void)  { return cached_vbus; }

bool power_hal_pwr_pressed(void) {
    if (pwr_pressed_flag) { pwr_pressed_flag = false; return true; }
    return false;
}

bool power_hal_pwr_long_pressed(void) {
    if (pwr_long_flag) { pwr_long_flag = false; return true; }
    return false;
}

bool power_hal_shutdown(void) {
    if (!pmu_ok) return false;   // no PMU to ask: the app gets {"err":true}
    Serial.println("Power: shutting down (asked by the app)");
    Serial.flush();
    pmu.shutdown();
    return true;
}

bool power_hal_pwr_released(void) {
    if (pwr_released_flag) { pwr_released_flag = false; return true; }
    return false;
}
