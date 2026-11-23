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
static int      cached_mv         = -1;     // battery voltage, -1 = no battery
static int      chg_state         = -1;     // xpowers_chg_status_t, -1 = not read yet
static int      low_logged        = 0;      // 1 low, 2 critical: each logged once until USB returns

#define BAT_LOW_MV  3400   // roughly the last 5-10 %
#define BAT_CRIT_MV 3300   // brownout territory under radio/panel load

static const char* chg_name(int st) {
    switch (st) {
    case XPOWERS_AXP2101_CHG_TRI_STATE:  return "trickle";
    case XPOWERS_AXP2101_CHG_PRE_STATE:  return "pre";
    case XPOWERS_AXP2101_CHG_CC_STATE:   return "cc";
    case XPOWERS_AXP2101_CHG_CV_STATE:   return "cv";
    case XPOWERS_AXP2101_CHG_DONE_STATE: return "done";
    case XPOWERS_AXP2101_CHG_STOP_STATE: return "stopped";
    default:                             return "?";
    }
}

// Why the PMU last turned on and off (REG 20h/21h bit flags). They describe
// the last real power cycle: a chip reset (panic, watchdog) keeps the PMU on.
static void log_power_sources(void) {
    static const char* const on_bits[] = { "pwrkey", "irq-pin", "vbus", "charge", "battery", "en" };
    static const char* const off_bits[] = { "pwrkey", "software", "pwrkey-low", "vsys-under",
                                            "vbus-over", "dcdc-under", "dcdc-over", "over-temp" };
    const int on = pmu.readRegister(XPOWERS_AXP2101_PWRON_STATUS);
    const int off = pmu.readRegister(XPOWERS_AXP2101_PWROFF_STATUS);
    char on_s[64] = "", off_s[96] = "";
    for (int i = 0; i < 6; i++)
        if (on >= 0 && (on & (1 << i)))
            snprintf(on_s + strlen(on_s), sizeof(on_s) - strlen(on_s), " %s", on_bits[i]);
    for (int i = 0; i < 8; i++)
        if (off >= 0 && (off & (1 << i)))
            snprintf(off_s + strlen(off_s), sizeof(off_s) - strlen(off_s), " %s", off_bits[i]);
    Serial.printf("AXP2101: power-on source 0x%02x (%s), last power-off 0x%02x (%s)\n",
                  on & 0xff, on_s[0] ? on_s + 1 : "none", off & 0xff, off_s[0] ? off_s + 1 : "none");
}

void power_hal_init(void) {
    if (!pmu.begin(Wire, AXP2101_ADDR, IIC_SDA, IIC_SCL)) {
        Serial.println("AXP2101 init failed");
        return;
    }
    Serial.println("AXP2101 init OK");
    pmu_ok = true;

    pmu.enableBattDetection();
    pmu.enableBattVoltageMeasure();
    pmu.enableTemperatureMeasure();   // die temperature, for the health log

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
    cached_mv = pmu.isBatteryConnect() ? pmu.getBattVoltage() : -1;
    chg_state = pmu.getChargerStatus();
    log_power_sources();
    Serial.printf("Power: boot on %s, battery %d %% %d mV, charger %s\n", cached_vbus ? "USB" : "battery",
                  cached_pct, cached_mv, chg_name(chg_state));
}

// USB plug/unplug, and the low-voltage warnings on battery (each once until
// USB is back).
static void log_transitions(bool vbus_was) {
    if (cached_vbus != vbus_was)
        Serial.printf("Power: USB %s, battery %d %% %d mV\n", cached_vbus ? "plugged" : "unplugged",
                      cached_pct, cached_mv);
    if (cached_vbus) {
        low_logged = 0;
    } else if (cached_mv > 0 && low_logged < 2 && cached_mv < (low_logged ? BAT_CRIT_MV : BAT_LOW_MV)) {
        low_logged++;
        Serial.printf("Power: battery %s, %d mV %d %%\n",
                      low_logged == 2 ? "critical (brownout risk)" : "low", cached_mv, cached_pct);
    }
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
    Serial.printf("Battery care: charging %s at %d %% %d mV\n", want ? "resumed" : "paused", cached_pct,
                  cached_mv);
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
    if (!pmu_ok) return;   // no PMU: don't poll a missing device on the shared bus
    uint32_t now = millis();

    if (now - last_charging_ms >= CHARGING_POLL_MS) {
        last_charging_ms = now;
        const bool vbus_was = cached_vbus;
        cached_charging = pmu.isCharging();
        cached_vbus     = pmu.isVbusIn();
        if (cached_vbus != vbus_was) log_transitions(vbus_was);
    }
    if (now - last_battery_ms >= BATTERY_POLL_MS) {
        last_battery_ms = now;
        cached_pct = pmu.getBatteryPercent();
        cached_mv = pmu.isBatteryConnect() ? pmu.getBattVoltage() : -1;
        const int st = pmu.getChargerStatus();
        if (st != chg_state) {
            Serial.printf("Power: charger %s -> %s, battery %d %% %d mV%s\n", chg_name(chg_state),
                          chg_name(st), cached_pct, cached_mv, charge_on ? "" : " (paused by battery care)");
            chg_state = st;
        }
        log_transitions(cached_vbus);
        care_tick();
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
    Serial.printf("Power: shutting down, battery %d %% %d mV\n", cached_pct, cached_mv);
    Serial.flush();
    pmu.shutdown();
    return true;
}

bool power_hal_pwr_released(void) {
    if (pwr_released_flag) { pwr_released_flag = false; return true; }
    return false;
}

int power_hal_battery_mv(void) { return cached_mv; }

size_t power_hal_health(char* buf, size_t cap) {
    if (!pmu_ok) return 0;
    const int n = snprintf(buf, cap, "bat %d %% %d mV, %s, charger %s%s, pmu %.0f C", cached_pct, cached_mv,
                           cached_vbus ? "usb" : "on battery", chg_name(chg_state),
                           charge_on ? "" : " (care pause)", pmu.getTemperature());
    return n > 0 && (size_t)n < cap ? (size_t)n : 0;
}
