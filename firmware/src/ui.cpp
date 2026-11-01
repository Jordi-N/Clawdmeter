#include "ui.h"
#include "performer.h"
#include "mascot.h"
#include "clawd_anim.h"
#include "clawd_policy.h"
#include "minimal_mode.h"
#include <lvgl.h>
#include <time.h>
#include <stdio.h>
#include <Arduino.h>
#include "logo.h"
#include "clawd_still.h"
#include "icons.h"
#include "hal/board_caps.h"
#include "layout.h"
#include "ui_text.h"
#include "pace.h"
#include "signals.h"
#include "agents_view.h"

// Where everything goes, from the panel's shape (layout.h). Computed once in
// ui_init() and treated as const for the rest of the program; a new panel
// size never needs an edit to the screen builders below.
static Layout L = {};

static_assert(LAYOUT_LOGO_SLOT == LOGO_HEIGHT && LAYOUT_LOGO_SLOT_SMALL == LOGO_SMALL_HEIGHT, "logo slot");
static_assert(LAYOUT_LOGO_W == CLAWD_STILL_W && LAYOUT_LOGO_W_SMALL == CLAWD_STILL_SMALL_W, "corner Clawd");
static_assert(LAYOUT_BATT_W == ICON_BATTERY_W && LAYOUT_BATT_W_SMALL == ICON_BATTERY_SMALL_W, "battery icon");

// Anthropic brand palette — design tokens live in theme.h
#include "theme.h"
#define COL_BG        THEME_BG
#define COL_PANEL     THEME_PANEL
#define COL_TEXT      THEME_TEXT
#define COL_DIM       THEME_DIM
#define COL_ACCENT    THEME_ACCENT
#define COL_GREEN     THEME_GREEN
#define COL_AMBER     THEME_AMBER
#define COL_RED       THEME_RED
#define COL_BAR_BG    THEME_BAR_BG

// ---- Usage screen widgets (single non-splash view) ----
static lv_obj_t* usage_container;
static lv_obj_t* lbl_title;
// Clock fed by the daemon: base epoch (local wall-clock seconds) + the lv_tick at
// which it landed, so the title ticks forward locally between 60s payloads.
static long     clock_base_epoch = 0;
static uint32_t clock_base_ms = 0;
static int      clock_fmt = 24;   // 12 or 24, set from the daemon payload
static int      clock_last_min = -1;   // last rendered minute; avoids redrawing the title every tick
static lv_obj_t* usage_group;   // the two usage panels — shown when connected
static lv_obj_t* pair_group;    // pairing hint — shown when disconnected
static lv_obj_t* pair_lines[4]; // its lines: heading, two steps, the hold note
static PairHint  pair_hint = PAIR_HINT_NORMAL;
static uint32_t  pair_hint_since = 0;
#define PAIR_CLEARED_SHOW_MS 8000   // "Ready to pair" stays this long
static lv_obj_t* bar_session;
static lv_obj_t* lbl_session_pct;
static lv_obj_t* lbl_session_label;
static lv_obj_t* lbl_session_reset;
static lv_obj_t* bar_weekly;
static lv_obj_t* lbl_weekly_pct;
static lv_obj_t* lbl_weekly_label;
static lv_obj_t* lbl_weekly_reset;
static lv_obj_t* panel_session = nullptr;
static lv_obj_t* panel_weekly = nullptr;
// Enterprise-only widgets inside panel_session
static lv_obj_t* lbl_session_pct_sym = nullptr;  // "%" in smaller font
static lv_obj_t* lbl_spending_desc = nullptr;     // "of spending limit"
static lv_obj_t* lbl_spending_status = nullptr;   // "Under pace" / "On pace" / "Over pace"
static lv_obj_t* lbl_anim;      // status line: connection state + whimsical idle

// ---- Pages: sessions | usage | agents, side by side. A side swipe slides
// the cards to the neighbour page (title, Clawd, battery and status line
// stay); PAGE_IDLE_MS without a touch slides back to usage.
enum { PAGE_SESSIONS, PAGE_USAGE, PAGE_AGENTS, PAGE_COUNT };
static lv_obj_t* pages[PAGE_COUNT];
static int       page = PAGE_USAGE;
static uint32_t  page_touch_ms = 0;
#define PAGE_IDLE_MS  20000
#define PAGE_SLIDE_MS 250
static bool      page_pinned = false;   // a long press turns the slide-back off / on
static bool      long_pressed = false;  // this press was a long press: not a tap for Clawd
static lv_obj_t* toast = nullptr;       // a short notice in the middle of the screen
static uint32_t  toast_until = 0;
#define TOAST_MS      2000
#define LONG_PRESS_MS 1000
static lv_obj_t* lbl_sess_num;
static lv_obj_t* lbl_sess_line;
static lv_obj_t* lbl_act_word;
static lv_obj_t* lbl_act_line;
static lv_obj_t* bar_act;
static int16_t   hook_sessions = -1;   // -1 = absent: no hooks, shown as "--", never 0
static int8_t    hook_activity = -1;

// ---- Battery indicator (shared, on top) ----
static lv_obj_t* battery_img;
static lv_obj_t* logo_img;
static lv_image_dsc_t battery_dscs[6];  // empty, low, medium, full, charging, plug-zap (no battery)

// ---- Live-data freshness → which usage sub-view to show ----
// Live meters when data is flowing; the same meters dimmed when the host is
// connected but no usage update landed within DATA_FRESH_MS (stale: the last
// numbers stay, visibly not live, and the status line says so); the pairing
// hint when BLE is down. The corner Clawd stays in his seat in all three:
// there is only ever one Clawd. Re-evaluated every loop in ui_tick_anim().
#define STALE_OPA LV_OPA_40
static uint32_t  last_data_ms = 0;      // lv_tick when the last valid usage update landed
static bool      data_received = false; // any valid update since boot
static bool      data_ok = true;        // last payload's ok flag; a {"ok":false} beat = "no fresh data"
static int       view_state = -1;       // -1 unknown / 0 pair / 1 stale / 2 live
static const uint32_t DATA_FRESH_MS = 90000;  // usage counts as "live" within this window (daemon sends ~60s)

// ---- Shared ----
static lv_image_dsc_t logo_dsc;
static bool     s_ble_connected = false;   // cached BLE connection state
static uint32_t connected_at_ms = 0;       // when we last entered CONNECTED ("Conectado" dwell)

// Animation state
static uint32_t anim_last_ms = 0;
static uint8_t anim_spinner_idx = 0;
static uint8_t anim_phase = 0;
static uint8_t anim_msg_idx = 0;    // counts verb changes (the stale line alternates on it)
static uint8_t anim_verb_idx = 0;   // the verb shown: the next one that fits the line
static uint32_t anim_msg_start = 0;
static const char* frozen_shown = nullptr;   // spinner held still: the word on screen

static const char* const spinner_frames[] = {
    "\xC2\xB7", "\xE2\x9C\xBB", "\xE2\x9C\xBD",
    "\xE2\x9C\xB6", "\xE2\x9C\xB3", "\xE2\x9C\xA2",
};
#define SPINNER_COUNT 6
#define SPINNER_PHASES (2 * (SPINNER_COUNT - 1))  // 10: ping-pong 0..5..0

static const uint16_t spinner_ms[SPINNER_COUNT] = {
    260, 130, 130, 130, 130, 260,
};

static const char* const anim_messages[] = {
    "Calving", "Pontificating", "Reticulating", "Churring",
    "Muauding", "Conjuring", "Considering", "Desdanieling",
    "Cooking", "Francesing", "Crafting", "Transmuting",
    "Deliberating", "Wandering", "Wizarding", "Working",
    "Al agua hirviendo",
};
#define ANIM_MSG_COUNT (sizeof(anim_messages) / sizeof(anim_messages[0]))

// The first verb from `i` on that fits the status line as "<glyph> <verb>…";
// a narrow panel skips the long ones rather than clipping them.
static uint8_t fitting_verb(uint8_t i) {
    for (uint8_t n = 0; n < ANIM_MSG_COUNT; n++, i = (i + 1) % ANIM_MSG_COUNT) {
        char buf[48];
        snprintf(buf, sizeof(buf), "%s %s\xE2\x80\xA6", spinner_frames[1], anim_messages[i]);
        if (layout_text_width(L.status_font, buf) <= L.status_w) return i;
    }
    return i;
}

static lv_color_t pct_color(float pct) {
    if (pct >= 80.0f) return COL_RED;
    if (pct >= 50.0f) return COL_AMBER;
    return COL_GREEN;
}

// ---- Pace marker (device setting `pm`, pace.h). Off: the bars use
// pct_color(). On: each Pro/Max bar gets a thin ivory line at the elapsed
// share of its window and the fill follows pace_level(). A bar with no reset
// time hides its line and keeps pct_color(). Spend-capped accounts are
// untouched: their Period bar already is the elapsed share.
static bool pace_marker = false;
static lv_obj_t* tick_session = nullptr;
static lv_obj_t* tick_weekly  = nullptr;
static UsageData last_data;                 // recoloured when the setting changes
static bool have_last_data = false;

static lv_color_t bar_color(float used_pct, int reset_mins, int window_mins) {
    if (pace_marker) {
        switch (pace_level(used_pct, pace_elapsed_pct(reset_mins, window_mins))) {
        case PACE_LIMIT: return COL_RED;
        case PACE_AHEAD: return COL_AMBER;
        case PACE_ON:    return COL_GREEN;
        case PACE_UNKNOWN: break;
        }
    }
    return pct_color(used_pct);
}

// A line inside the bar, exactly its height, drawn over the fill.
static lv_obj_t* make_pace_tick(lv_obj_t* bar) {
    lv_obj_t* tick = lv_obj_create(bar);
    lv_obj_remove_style_all(tick);
    lv_obj_set_size(tick, L.bar_h >= 20 ? 3 : 2, L.bar_h);
    lv_obj_set_style_bg_color(tick, COL_TEXT, 0);
    lv_obj_set_style_bg_opa(tick, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(tick, 1, 0);
    lv_obj_clear_flag(tick, LV_OBJ_FLAG_CLICKABLE);
    lv_obj_add_flag(tick, LV_OBJ_FLAG_HIDDEN);
    return tick;
}

// Centre the line on elapsed_pct, kept clear of the bar's rounded ends;
// hidden while the setting is off or the elapsed share is unknown.
static void place_pace_tick(lv_obj_t* tick, float elapsed_pct) {
    if (!pace_marker || elapsed_pct < 0) {
        lv_obj_add_flag(tick, LV_OBJ_FLAG_HIDDEN);
        return;
    }
    lv_obj_t* bar = lv_obj_get_parent(tick);
    const int bar_w = lv_obj_get_width(bar);
    const int w = lv_obj_get_width(tick);
    const int inset = lv_obj_get_style_radius(bar, LV_PART_MAIN) / 2;
    int x = (int)(elapsed_pct * bar_w / 100.0f + 0.5f) - w / 2;
    if (x < inset) x = inset;
    if (x > bar_w - inset - w) x = bar_w - inset - w;
    lv_obj_set_x(tick, x);
    lv_obj_clear_flag(tick, LV_OBJ_FLAG_HIDDEN);
}

// `none`: what a missing reset time means (-1 in the payload): the session
// window hasn't started, the weekly one is unknown.
static void format_reset_time(int mins, char* buf, size_t len, const char* none) {
    if (mins < 0) {
        snprintf(buf, len, "%s", none);
    } else if (mins < 60) {
        snprintf(buf, len, "Reinicio %dm", mins);
    } else if (mins < 1440) {
        snprintf(buf, len, "Reinicio %dh %dm", mins / 60, mins % 60);
    } else {
        snprintf(buf, len, "Reinicio %dd %dh", mins / 1440, (mins % 1440) / 60);
    }
}

static lv_obj_t* make_panel(lv_obj_t* parent, int x, int y, int w, int h) {
    lv_obj_t* panel = lv_obj_create(parent);
    lv_obj_set_pos(panel, x, y);
    lv_obj_set_size(panel, w, h);
    lv_obj_set_style_bg_color(panel, COL_PANEL, 0);
    lv_obj_set_style_bg_opa(panel, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(panel, 8, 0);
    lv_obj_set_style_border_width(panel, 0, 0);
    lv_obj_set_style_pad_left(panel, L.pad_x, 0);
    lv_obj_set_style_pad_right(panel, L.pad_x, 0);
    lv_obj_set_style_pad_top(panel, L.pad_y, 0);
    lv_obj_set_style_pad_bottom(panel, L.pad_y, 0);
    lv_obj_clear_flag(panel, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(panel, LV_OBJ_FLAG_EVENT_BUBBLE);
    return panel;
}

static lv_obj_t* make_bar(lv_obj_t* parent, int x, int y, int w, int h) {
    lv_obj_t* bar = lv_bar_create(parent);
    lv_obj_set_pos(bar, x, y);
    lv_obj_set_size(bar, w, h);
    lv_bar_set_range(bar, 0, 100);
    lv_bar_set_value(bar, 0, LV_ANIM_OFF);
    lv_obj_set_style_bg_color(bar, COL_BAR_BG, LV_PART_MAIN);
    lv_obj_set_style_bg_opa(bar, LV_OPA_COVER, LV_PART_MAIN);
    lv_obj_set_style_radius(bar, 6, LV_PART_MAIN);
    lv_obj_set_style_bg_color(bar, COL_GREEN, LV_PART_INDICATOR);
    lv_obj_set_style_bg_opa(bar, LV_OPA_COVER, LV_PART_INDICATOR);
    lv_obj_set_style_radius(bar, 6, LV_PART_INDICATOR);
    return bar;
}

static void init_icon_dsc_rgb565a8(lv_image_dsc_t* dsc, int w, int h, const uint8_t* data) {
    dsc->header.w = w;
    dsc->header.h = h;
    dsc->header.cf = LV_COLOR_FORMAT_RGB565A8;
    dsc->header.stride = w * 2;
    dsc->data = data;
    dsc->data_size = w * h * 3;
}

static lv_obj_t* make_pill(lv_obj_t* parent, const char* text) {
    lv_obj_t* lbl = lv_label_create(parent);
    lv_label_set_text(lbl, text);
    lv_obj_set_style_text_font(lbl, L.pill_font, 0);
    lv_obj_set_style_text_color(lbl, COL_TEXT, 0);
    lv_obj_set_style_bg_color(lbl, COL_BAR_BG, 0);
    lv_obj_set_style_bg_opa(lbl, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(lbl, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_pad_left(lbl, L.pill_pad_x, 0);
    lv_obj_set_style_pad_right(lbl, L.pill_pad_x, 0);
    lv_obj_set_style_pad_top(lbl, L.pill_pad_y, 0);
    lv_obj_set_style_pad_bottom(lbl, L.pill_pad_y, 0);
    return lbl;
}

static void init_battery_icons(void) {
    if (L.small_icons) {
        init_icon_dsc_rgb565a8(&battery_dscs[0], ICON_BATTERY_SMALL_W, ICON_BATTERY_SMALL_H, icon_battery_small_data);
        init_icon_dsc_rgb565a8(&battery_dscs[1], ICON_BATTERY_LOW_SMALL_W, ICON_BATTERY_LOW_SMALL_H, icon_battery_low_small_data);
        init_icon_dsc_rgb565a8(&battery_dscs[2], ICON_BATTERY_MEDIUM_SMALL_W, ICON_BATTERY_MEDIUM_SMALL_H, icon_battery_medium_small_data);
        init_icon_dsc_rgb565a8(&battery_dscs[3], ICON_BATTERY_FULL_SMALL_W, ICON_BATTERY_FULL_SMALL_H, icon_battery_full_small_data);
        init_icon_dsc_rgb565a8(&battery_dscs[4], ICON_BATTERY_CHARGING_SMALL_W, ICON_BATTERY_CHARGING_SMALL_H, icon_battery_charging_small_data);
        init_icon_dsc_rgb565a8(&battery_dscs[5], ICON_PLUG_ZAP_SMALL_W, ICON_PLUG_ZAP_SMALL_H, icon_plug_zap_small_data);
        return;
    }
    init_icon_dsc_rgb565a8(&battery_dscs[0], ICON_BATTERY_W, ICON_BATTERY_H, icon_battery_data);
    init_icon_dsc_rgb565a8(&battery_dscs[1], ICON_BATTERY_LOW_W, ICON_BATTERY_LOW_H, icon_battery_low_data);
    init_icon_dsc_rgb565a8(&battery_dscs[2], ICON_BATTERY_MEDIUM_W, ICON_BATTERY_MEDIUM_H, icon_battery_medium_data);
    init_icon_dsc_rgb565a8(&battery_dscs[3], ICON_BATTERY_FULL_W, ICON_BATTERY_FULL_H, icon_battery_full_data);
    init_icon_dsc_rgb565a8(&battery_dscs[4], ICON_BATTERY_CHARGING_W, ICON_BATTERY_CHARGING_H, icon_battery_charging_data);
    init_icon_dsc_rgb565a8(&battery_dscs[5], ICON_PLUG_ZAP_W, ICON_PLUG_ZAP_H, icon_plug_zap_data);
}

// ======== Usage Screen ========

static lv_obj_t* make_usage_panel(lv_obj_t* parent, const LayoutRect& r, const char* pill_text,
                                  lv_obj_t** out_pct, lv_obj_t** out_pill,
                                  lv_obj_t** out_bar, lv_obj_t** out_reset) {
    lv_obj_t* panel = make_panel(parent, r.x, r.y, r.w, r.h);

    *out_pct = lv_label_create(panel);
    lv_label_set_text(*out_pct, "--%");
    lv_obj_set_style_text_font(*out_pct, L.pct_font, 0);
    lv_obj_set_style_text_color(*out_pct, COL_TEXT, 0);
    lv_obj_set_pos(*out_pct, 0, 0);

    *out_pill = make_pill(panel, pill_text);
    lv_obj_align(*out_pill, LV_ALIGN_TOP_RIGHT, 0, 1);

    *out_bar = make_bar(panel, 0, L.bar_y, r.w - 2 * L.pad_x, L.bar_h);

    *out_reset = lv_label_create(panel);
    lv_label_set_text(*out_reset, "---");
    lv_obj_set_style_text_font(*out_reset, L.reset_font, 0);
    lv_obj_set_style_text_color(*out_reset, COL_DIM, 0);
    lv_obj_set_pos(*out_reset, 0, L.reset_y);

    return panel;
}

// Pairing hint — shown when disconnected so the screen isn't empty and the
// user knows how to (re)pair. Boards with a PWR button name the 3-second
// release gesture (main.cpp pair_tick); boards without one (LCD-4) name the
// 5 s touch hold on this hint that clears an old pairing (pair_hold.h), and
// only those lines change while the hold runs and after a clear.
static void apply_pair_text(void) {
    if (!pair_lines[0]) return;
    const bool pwr = board_caps().has_pwr_button;
    const char* const* lines = pwr ? PAIR_TEXT_PWR : PAIR_TEXT_TOUCH;
    const char* note = "";
    // Only the touch hold talks back; the PWR hint stays as it is.
    if (!pwr && pair_hint == PAIR_HINT_HOLDING) note = PAIR_TEXT_HOLDING;
    else if (!pwr && pair_hint == PAIR_HINT_CLEARED) lines = PAIR_TEXT_CLEARED;
    for (int i = 0; i < 3; i++) lv_label_set_text(pair_lines[i], lines[i]);
    lv_label_set_text(pair_lines[3], note);
    lv_obj_set_style_text_color(pair_lines[3], pair_hint == PAIR_HINT_NORMAL ? COL_DIM : COL_ACCENT, 0);
}

// A centred line; on panels where the steps wrap, it wraps at pair_w.
static lv_obj_t* make_pair_line(int i, lv_color_t color) {
    lv_obj_t* l = lv_label_create(pair_group);
    lv_label_set_text(l, "");
    lv_obj_set_style_text_font(l, L.pair_font[i], 0);
    lv_obj_set_style_text_color(l, color, 0);
    if (L.pair_wrap) {
        lv_obj_set_width(l, L.pair_w);
        lv_obj_set_style_text_align(l, LV_TEXT_ALIGN_CENTER, 0);
    }
    lv_obj_align(l, LV_ALIGN_TOP_MID, 0, L.pair_line_y[i]);
    return l;
}

static void build_pair_group(lv_obj_t* parent) {
    pair_group = lv_obj_create(parent);
    lv_obj_set_size(pair_group, L.w, L.h - L.pair_y);
    lv_obj_set_pos(pair_group, 0, L.pair_y);
    lv_obj_set_style_bg_opa(pair_group, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(pair_group, 0, 0);
    lv_obj_set_style_pad_all(pair_group, 0, 0);
    lv_obj_clear_flag(pair_group, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(pair_group, LV_OBJ_FLAG_EVENT_BUBBLE);

    pair_lines[0] = make_pair_line(0, COL_TEXT);
    pair_lines[1] = make_pair_line(1, COL_DIM);
    pair_lines[2] = make_pair_line(2, COL_DIM);
    pair_lines[3] = make_pair_line(3, COL_DIM);
    apply_pair_text();

    lv_obj_add_flag(pair_group, LV_OBJ_FLAG_HIDDEN);  // ui_update_ble_status decides
}

bool ui_pair_hint_visible(void) { return view_state == 0; }

void ui_set_pair_hint(PairHint hint) {
    if (hint == pair_hint) return;
    pair_hint = hint;
    pair_hint_since = lv_tick_get();
    apply_pair_text();
}

// The title's font follows the clock format (a 12-hour clock may need a
// smaller one); hidden where the header has no room for it at all.
static void apply_title_font(void) {
    const int fmt = clock_base_epoch > 0 && clock_fmt == 12 ? 1 : 0;
    const lv_font_t* f = L.title_font[fmt];
    if (!f) {
        lv_obj_add_flag(lbl_title, LV_OBJ_FLAG_HIDDEN);
        return;
    }
    lv_obj_set_style_text_font(lbl_title, f, 0);
    lv_obj_align(lbl_title, LV_ALIGN_TOP_MID, L.title_cx - L.w / 2, L.title_y[fmt]);
    lv_obj_clear_flag(lbl_title, LV_OBJ_FLAG_HIDDEN);
}

// A transparent full-screen layer: the cards of one page, toggled as a unit.
static lv_obj_t* make_group(lv_obj_t* parent) {
    lv_obj_t* g = lv_obj_create(parent);
    lv_obj_set_size(g, L.w, L.h);
    lv_obj_set_pos(g, 0, 0);
    lv_obj_set_style_bg_opa(g, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(g, 0, 0);
    lv_obj_set_style_pad_all(g, 0, 0);
    lv_obj_clear_flag(g, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(g, LV_OBJ_FLAG_EVENT_BUBBLE);
    return g;
}

static void apply_sessions_text(void) {
    if (!pages[PAGE_SESSIONS]) return;
    if (hook_sessions < 0) {
        lv_label_set_text(lbl_sess_num, "--");
        lv_label_set_text(lbl_sess_line, "sin hooks");
    } else {
        lv_label_set_text_fmt(lbl_sess_num, "%d", hook_sessions);
        lv_label_set_text(lbl_sess_line, hook_sessions == 1 ? "sesión activa" : "sesiones activas");
    }
    static const char* const act_words[4] = { "Nada", "Bajo", "Medio", "Alto" };
    lv_label_set_text(lbl_act_word, hook_activity < 0 ? "--" : act_words[hook_activity]);
    lv_label_set_text(lbl_act_line, hook_activity < 0 ? "sin hooks" : "uso de herramientas");
    lv_bar_set_value(bar_act, hook_activity < 0 ? 0 : hook_activity * 100 / 3, LV_ANIM_ON);
}

// Same two cards as the meters. Pills no wider than "Gasto"/"Ciclo", lines no
// wider than the reset lines, so every layout that fits those fits these.
static void build_sessions_page(lv_obj_t* parent) {
    lv_obj_t* g = pages[PAGE_SESSIONS] = make_group(parent);
    lv_obj_add_flag(g, LV_OBJ_FLAG_HIDDEN);
    lv_obj_t *pill, *bar_sess;
    make_usage_panel(g, L.card[0], "Ahora", &lbl_sess_num, &pill, &bar_sess, &lbl_sess_line);
    lv_obj_add_flag(bar_sess, LV_OBJ_FLAG_HIDDEN);   // a count has no full scale
    make_usage_panel(g, L.card[1], "Ritmo", &lbl_act_word, &pill, &bar_act, &lbl_act_line);
    lv_obj_set_style_bg_color(bar_act, COL_ACCENT, LV_PART_INDICATOR);
    apply_sessions_text();
}

// One card over both card slots (agents_view.h).
static void build_agents_page(lv_obj_t* parent) {
    lv_obj_t* g = pages[PAGE_AGENTS] = make_group(parent);
    lv_obj_add_flag(g, LV_OBJ_FLAG_HIDDEN);
    const LayoutRect& a = L.card[0];
    const LayoutRect& b = L.card[1];
    agents_view_create(g, a.x, a.y, a.w, b.y + b.h - a.y);
}

static void slide_done(lv_anim_t* a) {
    lv_obj_t* obj = (lv_obj_t*)a->var;
    lv_obj_add_flag(obj, LV_OBJ_FLAG_HIDDEN);
    lv_obj_set_x(obj, 0);
}

static void slide_x(lv_obj_t* obj, int from, int to, bool hide_after) {
    lv_anim_t a;
    lv_anim_init(&a);
    lv_anim_set_var(&a, obj);
    lv_anim_set_exec_cb(&a, (lv_anim_exec_xcb_t)lv_obj_set_x);
    lv_anim_set_values(&a, from, to);
    lv_anim_set_duration(&a, PAGE_SLIDE_MS);
    lv_anim_set_path_cb(&a, lv_anim_path_ease_out);
    if (hide_after) lv_anim_set_completed_cb(&a, slide_done);
    lv_anim_start(&a);
}

// The incoming page enters from the side the finger came from.
static void slide_to(int target) {
    if (target == page || target < 0 || target >= PAGE_COUNT) return;
    lv_obj_t* from = pages[page];
    lv_obj_t* to = pages[target];
    const int dx = target > page ? -L.w : L.w;
    page = target;
    page_touch_ms = lv_tick_get();
    lv_anim_delete(from, NULL);
    lv_anim_delete(to, NULL);
    lv_obj_set_style_opa(to, view_state == 1 ? STALE_OPA : LV_OPA_COVER, 0);
    lv_obj_clear_flag(to, LV_OBJ_FLAG_HIDDEN);
    if (minimal_mode()) {          // nothing moves: a plain swap
        lv_obj_add_flag(from, LV_OBJ_FLAG_HIDDEN);
        lv_obj_set_x(from, 0);
        lv_obj_set_x(to, 0);
        return;
    }
    slide_x(from, 0, dx, true);
    slide_x(to, -dx, 0, false);
}

static void on_gesture(lv_event_t* e) {
    (void)e;
    if (view_state <= 0) return;   // pairing hint: no pages
    const lv_dir_t dir = lv_indev_get_gesture_dir(lv_indev_active());
    if (dir == LV_DIR_LEFT) slide_to(page + 1);
    else if (dir == LV_DIR_RIGHT) slide_to(page - 1);
}

static void show_toast(const char* text) {
    if (!toast) {
        toast = lv_label_create(lv_layer_top());
        lv_obj_set_style_text_font(toast, L.reset_font, 0);
        lv_obj_set_style_text_color(toast, COL_TEXT, 0);
        lv_obj_set_style_bg_color(toast, COL_BAR_BG, 0);
        lv_obj_set_style_bg_opa(toast, LV_OPA_COVER, 0);
        lv_obj_set_style_radius(toast, LV_RADIUS_CIRCLE, 0);
        lv_obj_set_style_pad_hor(toast, 18, 0);
        lv_obj_set_style_pad_ver(toast, 10, 0);
    }
    lv_label_set_text(toast, text);
    lv_obj_center(toast);
    lv_obj_clear_flag(toast, LV_OBJ_FLAG_HIDDEN);
    toast_until = lv_tick_get() + TOAST_MS;
}

static void on_indev(lv_event_t* e) {
    const lv_event_code_t code = lv_event_get_code(e);
    if (code == LV_EVENT_PRESSED) { long_pressed = false; return; }
    // LONG_PRESSED: pin / unpin the page. Not on the pairing hint, whose
    // touch hold clears the pairing.
    if (view_state <= 0) return;
    long_pressed = true;
    page_pinned = !page_pinned;
    page_touch_ms = lv_tick_get();
    show_toast(page_pinned ? "Pantalla fijada" : "Vuelve a Uso en 20 s");
}

bool ui_clawd_takes_taps(void) {
    return !long_pressed && (page != PAGE_AGENTS || view_state <= 0);
}

static void init_usage_screen(lv_obj_t* scr) {
    usage_container = lv_obj_create(scr);
    lv_obj_set_size(usage_container, L.w, L.h);
    lv_obj_set_pos(usage_container, 0, 0);
    lv_obj_set_style_bg_opa(usage_container, LV_OPA_TRANSP, 0);
    lv_obj_set_style_border_width(usage_container, 0, 0);
    lv_obj_set_style_pad_all(usage_container, 0, 0);
    lv_obj_clear_flag(usage_container, LV_OBJ_FLAG_SCROLLABLE);

    lbl_title = lv_label_create(usage_container);
    lv_label_set_text(lbl_title, "Uso");
    lv_obj_set_style_text_color(lbl_title, COL_TEXT, 0);
    apply_title_font();

    // Usage panels (shown when connected) live in a transparent full-size group
    // so they can be toggled against the pairing hint as one unit.
    usage_group = pages[PAGE_USAGE] = make_group(usage_container);

    panel_session = make_usage_panel(usage_group, L.card[0], L.pills[0],
                     &lbl_session_pct, &lbl_session_label,
                     &bar_session, &lbl_session_reset);

    // Enterprise-only overlays inside panel_session — hidden until enterprise data arrives
    lbl_session_pct_sym = lv_label_create(panel_session);
    lv_label_set_text(lbl_session_pct_sym, "%");
    lv_obj_set_style_text_font(lbl_session_pct_sym, L.reset_font, 0);
    lv_obj_set_style_text_color(lbl_session_pct_sym, COL_TEXT, 0);
    lv_obj_add_flag(lbl_session_pct_sym, LV_OBJ_FLAG_HIDDEN);

    lbl_spending_desc = lv_label_create(panel_session);
    lv_label_set_text(lbl_spending_desc, "del límite de gasto");
    lv_obj_set_style_text_font(lbl_spending_desc, L.reset_font, 0);
    lv_obj_set_style_text_color(lbl_spending_desc, COL_DIM, 0);
    lv_obj_set_pos(lbl_spending_desc, 0, L.reset_y);
    lv_obj_add_flag(lbl_spending_desc, LV_OBJ_FLAG_HIDDEN);

    lbl_spending_status = lv_label_create(panel_session);
    lv_label_set_text(lbl_spending_status, "");
    lv_obj_set_style_text_font(lbl_spending_status, L.pace_font, 0);
    lv_obj_set_pos(lbl_spending_status, 0, L.reset_y + 20);
    lv_obj_add_flag(lbl_spending_status, LV_OBJ_FLAG_HIDDEN);

    panel_weekly = make_usage_panel(usage_group, L.card[1], L.pills[1],
                     &lbl_weekly_pct, &lbl_weekly_label,
                     &bar_weekly, &lbl_weekly_reset);
    // Recolor enabled so enterprise period box can color pace and reset separately
    lv_label_set_recolor(lbl_weekly_reset, true);

    tick_session = make_pace_tick(bar_session);
    tick_weekly  = make_pace_tick(bar_weekly);

    build_sessions_page(usage_container);
    build_agents_page(usage_container);
    build_pair_group(usage_container);

    // Status line — always visible on the usage view. Driven by ui_tick_anim().
    lbl_anim = lv_label_create(usage_container);
    lv_label_set_text(lbl_anim, "");
    lv_obj_set_style_text_font(lbl_anim, L.status_font, 0);
    lv_obj_set_style_text_color(lbl_anim, COL_ACCENT, 0);
    lv_obj_align(lbl_anim, LV_ALIGN_BOTTOM_MID, 0, L.status_y);
}

// ---- Attention message on the status line (the director sequences the rest).
static bool alert_active = false;
static const char* still_shown = nullptr;   // minimal mode: status word on screen

// ======== Public API ========

void ui_init(void) {
    const BoardCaps& caps = board_caps();
    L = layout_compute({ caps.width, caps.height, caps.round_panel });

    lv_obj_t* scr = lv_screen_active();
    lv_obj_set_style_bg_color(scr, COL_BG, 0);
    lv_obj_set_style_bg_opa(scr, LV_OPA_COVER, 0);
    // The screen is a fixed room, not a scroll view. Left scrollable (the
    // LVGL default), it grows a theme scrollbar (a faint 4 px grey bar ~6 px
    // above the bottom edge) whenever Clawd walks past an edge, e.g. on a
    // lurk trip or the screensaver's walk-off, and a drag could scroll it.
    lv_obj_clear_flag(scr, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_set_scrollbar_mode(scr, LV_SCROLLBAR_MODE_OFF);

    // Static corner mascot (see clawd_still.h): fallback when the animated
    // one can't get its buffer.
    if (L.small_icons) init_icon_dsc_rgb565a8(&logo_dsc, CLAWD_STILL_SMALL_W, CLAWD_STILL_SMALL_H, clawd_still_small_data);
    else               init_icon_dsc_rgb565a8(&logo_dsc, CLAWD_STILL_W, CLAWD_STILL_H, clawd_still_data);
    init_battery_icons();

    init_usage_screen(scr);
    lv_obj_remove_flag(scr, LV_OBJ_FLAG_GESTURE_BUBBLE);   // swipes stop at the screen
    lv_obj_add_event_cb(scr, on_gesture, LV_EVENT_GESTURE, NULL);
    if (lv_indev_t* indev = lv_indev_get_next(NULL)) {
        lv_indev_set_long_press_time(indev, LONG_PRESS_MS);
        lv_indev_add_event_cb(indev, on_indev, LV_EVENT_PRESSED, NULL);
        lv_indev_add_event_cb(indev, on_indev, LV_EVENT_LONG_PRESSED, NULL);
    }

    battery_img = lv_image_create(scr);
    lv_image_set_src(battery_img, &battery_dscs[0]);
    lv_obj_set_pos(battery_img, L.batt_x, L.batt_y);
    // Boards without battery telemetry never show the indicator (per the HAL
    // contract).
    if (!board_caps().has_battery) {
        lv_obj_del(battery_img);
        battery_img = nullptr;
    }

    // Corner mascot in the old logo slot. The still Clawd is shorter than the
    // 80/40 px slot the spark logo used; center it vertically in that slot.
    {
        const int slot  = L.small_icons ? LOGO_SMALL_HEIGHT : LOGO_HEIGHT;
        const int art_h = L.small_icons ? CLAWD_STILL_SMALL_H : CLAWD_STILL_H;
        const int top   = L.logo_y + (slot - art_h) / 2;
        const int cell  = L.small_icons ? 2 : 3;
        // His tallest act (jumping happy, the fresh-window joy) rises well
        // above the idle pose. Lower the seat just enough for it to clear the
        // top edge, without sitting on the first panel. Where the layout has
        // no room for that (240 px panels) the mascot swaps in a shorter
        // official act instead of clipping (mascot.cpp).
        const ClawdAnim* jump = clawd_anim("jumping happy");
        const int feet = jump ? corner_seat_feet(top + art_h, jump->h * cell, L.card[0].y - 4, 2)
                              : top + art_h;
        // Animated: idles, does acts, and takes walk-off/lurk trips (the
        // trips need PSRAM; without it he stays in his seat).
        // Created after the panels so he walks over the battery.
        if (!mascot_create(scr, L.logo_x, feet, cell)) {
            logo_img = lv_image_create(scr);
            lv_image_set_src(logo_img, &logo_dsc);
            lv_obj_set_pos(logo_img, L.logo_x, top);
        }
    }

    // Clawd performs in front of the meters, fully drawn, like the
    // attention walk-on (a tap sends him off). Created last so he's on top
    // of everything on the screen. His feet stand on the bottom edge of the
    // last panel, clear of the (Clawd-orange) status line.
    performer_init(scr, L.card[1].y + L.card[1].h);

    lv_obj_set_style_text_align(lbl_anim, LV_TEXT_ALIGN_CENTER, 0);
    anim_verb_idx = fitting_verb(0);
}

// The Pro/Max bars' fill colours and pace markers.
static void apply_bar_colors(const UsageData* data) {
    if (data->enterprise) {
        // Spending has no 5-hour window, and the Period bar (coloured in
        // ui_update) already is the elapsed share: no markers.
        lv_obj_set_style_bg_color(bar_session, pct_color(data->session_pct), LV_PART_INDICATOR);
        place_pace_tick(tick_session, -1.0f);
        place_pace_tick(tick_weekly, -1.0f);
        return;
    }
    lv_obj_set_style_bg_color(bar_session,
        bar_color(data->session_pct, data->session_reset_mins, PACE_SESSION_MINS), LV_PART_INDICATOR);
    place_pace_tick(tick_session, pace_elapsed_pct(data->session_reset_mins, PACE_SESSION_MINS));
    lv_obj_set_style_bg_color(bar_weekly,
        bar_color(data->weekly_pct, data->weekly_reset_mins, PACE_WEEK_MINS), LV_PART_INDICATOR);
    place_pace_tick(tick_weekly, pace_elapsed_pct(data->weekly_reset_mins, PACE_WEEK_MINS));
}

void ui_set_pace_marker(bool on) {
    if (on == pace_marker) return;
    pace_marker = on;
    if (have_last_data) apply_bar_colors(&last_data);
}

void ui_update(const UsageData* data) {
    if (!data->valid) return;
    if (data->sessions != hook_sessions || data->activity != hook_activity) {
        hook_sessions = data->sessions;
        hook_activity = data->activity;
        apply_sessions_text();
    }
    data_ok = data->ok;
    if (!data->ok) return;          // a {"ok":false} "no data" beat → fall through to idle, keep last numbers
    last_data = *data;
    have_last_data = true;
    last_data_ms = lv_tick_get();   // a real usage update just landed
    data_received = true;

    if (data->clock_epoch > 0) {    // daemon supplied wall-clock time → drive the title clock
        if (clock_fmt != data->clock_fmt) clock_last_min = -1;
        clock_base_epoch = data->clock_epoch;
        clock_base_ms = last_data_ms;
        clock_fmt = data->clock_fmt;
        apply_title_font();
    } else if (clock_base_epoch != 0) {   // clock turned off daemon-side → revert title to "Usage"
        clock_base_epoch = 0;
        clock_last_min = -1;
        lv_label_set_text(lbl_title, "Uso");
        apply_title_font();
    }

    int s_pct = (int)(data->session_pct + 0.5f);

    if (data->enterprise) {
        // Spending box: big number-only label + small "%" symbol + desc + pace
        lv_obj_set_style_text_font(lbl_session_pct, L.ent_pct_font, 0);
        lv_label_set_text(lbl_session_label, L.pills[2]);
        lv_obj_add_flag(lbl_session_reset, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(lbl_session_pct_sym, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(lbl_spending_desc,   LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_spending_status,   LV_OBJ_FLAG_HIDDEN);
        if (panel_weekly) lv_obj_clear_flag(panel_weekly, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_set_style_text_font(lbl_session_pct, L.pct_font, 0);
        lv_label_set_text(lbl_session_label, L.pills[0]);
        lv_obj_clear_flag(lbl_session_reset, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_session_pct_sym, LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_spending_desc,   LV_OBJ_FLAG_HIDDEN);
        lv_obj_add_flag(lbl_spending_status, LV_OBJ_FLAG_HIDDEN);
        if (panel_weekly) lv_obj_clear_flag(panel_weekly, LV_OBJ_FLAG_HIDDEN);
    }

    char buf[48];

    // Pace vars used in both enterprise blocks below
    const char* pace_text = "Por debajo";
    lv_color_t  pace_color = COL_GREEN;
    const char* pace_hex   = "788c5d";   // matches THEME_GREEN
    if (data->session_pct > (float)data->time_pct + 15.0f) {
        pace_text = "Por encima"; pace_color = COL_RED;   pace_hex = "c0392b";
    } else if (data->session_pct > (float)data->time_pct - 15.0f) {
        pace_text = "Al ritmo";   pace_color = COL_AMBER; pace_hex = "d97757";
    }

    if (data->enterprise) {
        lv_label_set_text_fmt(lbl_session_pct, "%d", s_pct);
        lv_obj_align_to(lbl_session_pct_sym, lbl_session_pct,
                        LV_ALIGN_OUT_RIGHT_TOP, 4, 12);
    } else {
        lv_label_set_text_fmt(lbl_session_pct, "%d%%", s_pct);
        format_reset_time(data->session_reset_mins, buf, sizeof(buf), "Sin empezar");
        lv_label_set_text(lbl_session_reset, buf);
    }

    lv_bar_set_value(bar_session, s_pct, LV_ANIM_ON);

    if (data->enterprise) {
        // Period box: time % + dynamic pace color + "Resets <date>" label
        lv_label_set_text(lbl_weekly_label, L.pills[3]);
        lv_label_set_text_fmt(lbl_weekly_pct, "%d%%", data->time_pct);
        lv_bar_set_value(bar_weekly, data->time_pct, LV_ANIM_ON);
        lv_color_t bar_pace = (data->session_pct <= (float)data->time_pct) ? COL_GREEN :
                              (data->session_pct <= (float)data->time_pct + 15.0f) ? COL_AMBER :
                              COL_RED;
        lv_obj_set_style_bg_color(bar_weekly, bar_pace, LV_PART_INDICATOR);
        if (L.period_short)
            snprintf(buf, sizeof(buf), "#%s Reinicio %s#", pace_hex, data->reset_date);
        else
            snprintf(buf, sizeof(buf), "#%s %s# - #faf9f5 Reinicio %s#",
                     pace_hex, pace_text, data->reset_date);
        lv_obj_set_style_text_font(lbl_weekly_reset, L.period_font, 0);
        lv_label_set_text(lbl_weekly_reset, buf);
    } else {
        int w_pct = (int)(data->weekly_pct + 0.5f);
        lv_label_set_text(lbl_weekly_label, L.pills[1]);
        lv_label_set_text_fmt(lbl_weekly_pct, "%d%%", w_pct);
        lv_bar_set_value(bar_weekly, w_pct, LV_ANIM_ON);
        format_reset_time(data->weekly_reset_mins, buf, sizeof(buf), "---");
        lv_obj_set_style_text_font(lbl_weekly_reset, L.reset_font, 0);
        lv_label_set_text(lbl_weekly_reset, buf);
    }
    apply_bar_colors(data);
}

// Pick the usage-view sub-screen: pairing hint (BLE down), stale meters
// (connected but data has gone stale: dimmed, last values kept), or the live
// meters. Only restyles on an actual change. The animated status line stays
// visible everywhere and names the state.
static void update_view_state(void) {
    if (!usage_group || !pair_group) return;
    int v;
    if (!s_ble_connected) {
        v = 0;  // pairing hint
    } else if (data_received && data_ok && (lv_tick_get() - last_data_ms) < DATA_FRESH_MS) {
        v = 2;  // live usage
    } else {
        v = 1;  // stale
    }
    if (v == view_state) return;
    view_state = v;
    if (v == 0) page = PAGE_USAGE;   // back on the meters when the link returns
    for (lv_obj_t* p : pages) {      // a slide in flight snaps to its end
        lv_anim_delete(p, NULL);
        lv_obj_set_x(p, 0);
        lv_obj_add_flag(p, LV_OBJ_FLAG_HIDDEN);
    }
    lv_obj_t* shown = pages[page];
    if (v == 0) {
        lv_obj_clear_flag(pair_group, LV_OBJ_FLAG_HIDDEN);
    } else {
        lv_obj_add_flag(pair_group, LV_OBJ_FLAG_HIDDEN);
        lv_obj_clear_flag(shown, LV_OBJ_FLAG_HIDDEN);
        // `opa` (not `opa_layered`) scales every draw of the group and its
        // children: no layer buffer, so it costs nothing on the C6.
        lv_obj_set_style_opa(shown, v == 1 ? STALE_OPA : LV_OPA_COVER, 0);
    }
}

bool ui_meters_live(void) { return view_state == 2; }

void ui_tick_anim(void) {
    // Any touch keeps a side page up; PAGE_IDLE_MS untouched slides back.
    lv_indev_t* indev = lv_indev_get_next(NULL);
    if (indev && lv_indev_get_state(indev) == LV_INDEV_STATE_PRESSED) page_touch_ms = lv_tick_get();
    if (page != PAGE_USAGE && !page_pinned && lv_tick_get() - page_touch_ms >= PAGE_IDLE_MS) slide_to(PAGE_USAGE);
    if (toast && toast_until && (int32_t)(lv_tick_get() - toast_until) >= 0) {
        lv_obj_add_flag(toast, LV_OBJ_FLAG_HIDDEN);
        toast_until = 0;
    }
    update_view_state();
    if (pair_hint == PAIR_HINT_CLEARED && lv_tick_get() - pair_hint_since >= PAIR_CLEARED_SHOW_MS)
        ui_set_pair_hint(PAIR_HINT_NORMAL);
    const bool still = minimal_mode();   // minimal: nothing moves but the numbers

    uint32_t now = lv_tick_get();

    // Title clock: once the daemon has sent wall-clock time, replace "Usage" with
    // the live time, advanced locally so it ticks every minute between payloads.
    if (clock_base_epoch > 0) {
        time_t cur = (time_t)(clock_base_epoch + (now - clock_base_ms) / 1000);
        struct tm tmv;
        gmtime_r(&cur, &tmv);   // epoch is already local wall-clock → gmtime keeps it as-is
        if (tmv.tm_min != clock_last_min) {   // only rewrite the title when the minute changes
            clock_last_min = tmv.tm_min;
            char tbuf[12];
            if (clock_fmt == 12) {
                int h12 = tmv.tm_hour % 12;
                if (h12 == 0) h12 = 12;
                snprintf(tbuf, sizeof(tbuf), "%d:%02d %s", h12, tmv.tm_min,
                         tmv.tm_hour < 12 ? "AM" : "PM");
            } else {
                snprintf(tbuf, sizeof(tbuf), "%02d:%02d", tmv.tm_hour, tmv.tm_min);
            }
            lv_label_set_text(lbl_title, tbuf);
        }
    }

    if (alert_active) return;          // the status line holds the alert message

    if (still) {
        // One glyph, one word per state; redrawn only when it changes.
        const char* text = !s_ble_connected ? "Esperando"
                         : view_state == 1 ? (data_received ? "Desfasado" : "Sin datos") : "Conectado";
        if (text != still_shown) {
            still_shown = text;
            static char sbuf[40];
            snprintf(sbuf, sizeof(sbuf), "%s %s", spinner_frames[1], text);
            lv_label_set_text(lbl_anim, sbuf);
        }
        return;
    }

    // The verb changes every verb_ms; at No-nonsense that's once a minute
    // and the spinner holds still, so the line only redraws when the word
    // does (clawd_policy.h, liveliness).
    const ClawdTuning& tune = clawd_tuning();
    if (now - anim_msg_start >= tune.verb_ms) {
        anim_msg_idx = (anim_msg_idx + 1) % ANIM_MSG_COUNT;
        anim_verb_idx = fitting_verb((anim_verb_idx + 1) % ANIM_MSG_COUNT);
        anim_msg_start = now;
    }

    // With hooks, the verbs mean Claude is working: after a stop, until the
    // next prompt, the line rests on a still "En pausa" (before any event since
    // boot, tool activity decides). Without hooks there's no way to tell, so
    // the line rests on a still "Conectado" instead.
    const char ev = signals_last_event();
    const bool hooks = signals_hooks_seen();
    const bool paused = s_ble_connected && view_state == 2
                        && (!hooks || ev == 's' || (ev == '\0' && signals_activity() <= 0));
    const bool spin = tune.spinner && !paused;

    if (spin) {
        if (now - anim_last_ms < spinner_ms[anim_spinner_idx]) return;
        anim_last_ms = now;
        anim_phase = (anim_phase + 1) % SPINNER_PHASES;
        anim_spinner_idx = (anim_phase < SPINNER_COUNT) ? anim_phase
                                                        : (SPINNER_PHASES - anim_phase);
        frozen_shown = nullptr;
    } else {
        anim_spinner_idx = 1;
    }

    // Status text by priority. Whimsical messages only when connected & settled.
    const char* text;
    if (!s_ble_connected) {
        text = "Esperando";              // advertising / waiting for a host connection
    } else if (view_state == 1) {      // stale — alternate so it reads as alive AND not live
        text = (anim_msg_idx & 1) ? (data_received ? "Desfasado" : "Sin datos") : "Atento";
    } else if (now - connected_at_ms < 5000) {
        text = "Conectado";
    } else if (paused) {
        text = hooks ? "En pausa" : "Conectado";
    } else {
        text = anim_messages[anim_verb_idx];
    }

    if (!spin) {
        if (text == frozen_shown) return;
        frozen_shown = text;
    }

    // All states share the whimsical style: "<glyph> <Title-case word>…"
    static char buf[80];
    snprintf(buf, sizeof(buf), "%s %s\xE2\x80\xA6",
             spinner_frames[anim_spinner_idx], text);
    lv_label_set_text(lbl_anim, buf);
}

static void apply_battery_visibility(void) {
    if (battery_img) lv_obj_clear_flag(battery_img, LV_OBJ_FLAG_HIDDEN);
}

void ui_update_ble_status(ble_state_t state, const char* name, const char* mac) {
    (void)name; (void)mac;
    bool was_connected = s_ble_connected;
    s_ble_connected = (state == BLE_STATE_CONNECTED);

    if (s_ble_connected && !was_connected) connected_at_ms = lv_tick_get();
    // pair / idle / usage — picked from connection + data freshness.
    update_view_state();
}

void ui_update_battery(int percent, bool charging, bool usb_power) {
    if (!battery_img) return;
    int idx;
    if (percent < 0 && usb_power) {
        idx = 5;   // no battery, running on USB: show the plug-zap glyph, not an empty battery
    } else if (charging) {
        idx = 4;
    } else if (percent < 0) {
        idx = 0;
    } else if (percent <= 10) {
        idx = 0;
    } else if (percent <= 35) {
        idx = 1;
    } else if (percent <= 75) {
        idx = 2;
    } else {
        idx = 3;
    }
    lv_image_set_src(battery_img, &battery_dscs[idx]);
    apply_battery_visibility();
}

void ui_set_alert_text(const char* message) {
    if (message && message[0]) {
        alert_active = true;
        // Minimal mode: nothing moves, so a long message is cut, not scrolled.
        lv_label_set_long_mode(lbl_anim, minimal_mode() ? LV_LABEL_LONG_DOT
                                                        : LV_LABEL_LONG_SCROLL_CIRCULAR);
        // One line: without a fixed height LONG_DOT would wrap onto the cards.
        lv_obj_set_size(lbl_anim, L.status_w, lv_font_get_line_height(L.status_font));
        lv_label_set_text(lbl_anim, message);
        return;
    }
    if (!alert_active) return;
    alert_active = false;
    lv_label_set_long_mode(lbl_anim, LV_LABEL_LONG_WRAP);
    lv_obj_set_size(lbl_anim, LV_SIZE_CONTENT, LV_SIZE_CONTENT);
    anim_last_ms = 0;          // whimsical line redraws on the next tick
    still_shown = nullptr;
    frozen_shown = nullptr;
}
