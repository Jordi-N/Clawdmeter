#include "agents_view.h"
#include "agents_payload.h"
#include "theme.h"
#include <stdio.h>
#include <string.h>

LV_FONT_DECLARE(font_styrene_48);
LV_FONT_DECLARE(font_styrene_20);
LV_FONT_DECLARE(font_styrene_16);
LV_FONT_DECLARE(font_mono_18);

// Matches the usage cards on the large design (layout.cpp LARGE).
#define PAD_X       16
#define PAD_Y       12
#define PILL_PAD_X  18
#define PILL_PAD_Y  6
// Agent row columns, from the list's left edge.
#define COL_GLYPH   16
#define COL_TYPE    36
#define TYPE_W      118
#define COL_TASK    (COL_TYPE + TYPE_W + 8)
#define TIME_W      56

#define GLYPH_DONE  "\xE2\x9C\x93"   // ✓

// The status line's spinner (ui.cpp): · ✻ ✽ ✶ ✳ ✢, ping-pong.
static const char* const spin_frames[] = {
    "\xC2\xB7", "\xE2\x9C\xBB", "\xE2\x9C\xBD",
    "\xE2\x9C\xB6", "\xE2\x9C\xB3", "\xE2\x9C\xA2",
};
#define SPIN_COUNT  6
#define SPIN_PHASES (2 * (SPIN_COUNT - 1))
static const uint16_t spin_ms[SPIN_COUNT] = { 260, 130, 130, 130, 130, 260 };

static AgentsData s_data;              // zero: valid = false, "sin datos"
static uint32_t   s_data_ms;           // tick time s_data's running secs refer to
static bool       s_rebase = true;     // take s_data_ms from the next tick

static lv_obj_t *s_count, *s_sub1, *s_sub2, *s_list;
static int s_list_w;                   // its width, known before LVGL lays it out
static char  s_filter[sizeof(AgentSession::name)];   // shown session's name
static bool  s_filtered = false;

// A running agent's glyph and time, refreshed by the tick.
struct Live { lv_obj_t* glyph; lv_obj_t* time; uint8_t agent; char shown[12]; };
static Live     s_live[AGENTS_MAX_AGENTS];
static int      s_n_live = 0;
static uint8_t  s_spin_idx = 1, s_spin_phase = 1;
static uint32_t s_spin_ms = 0, s_time_ms = 0;

static lv_obj_t* make_label(lv_obj_t* parent, const lv_font_t* font, lv_color_t col, const char* txt) {
    lv_obj_t* l = lv_label_create(parent);
    lv_obj_set_style_text_font(l, font, 0);
    lv_obj_set_style_text_color(l, col, 0);
    lv_label_set_text(l, txt);
    return l;
}

// A bare full-width row; its height follows its labels.
static lv_obj_t* make_row(bool clickable) {
    lv_obj_t* r = lv_obj_create(s_list);
    lv_obj_remove_style_all(r);
    lv_obj_set_size(r, lv_pct(100), LV_SIZE_CONTENT);
    lv_obj_clear_flag(r, LV_OBJ_FLAG_SCROLLABLE);
    if (!clickable) lv_obj_clear_flag(r, LV_OBJ_FLAG_CLICKABLE);
    return r;
}

static void note_row(const char* txt) {
    lv_obj_t* r = make_row(false);
    lv_obj_set_pos(make_label(r, &font_styrene_16, THEME_DIM, txt), COL_TYPE, 0);
}

static void rebuild(void);
static void rebuild_async(void*) {
    rebuild();
    lv_obj_scroll_to_y(s_list, 0, LV_ANIM_OFF);
}

static void on_session_click(lv_event_t* e) {
    const int si = (int)(intptr_t)lv_event_get_user_data(e);
    if (s_filtered) {
        s_filtered = false;
    } else {
        memcpy(s_filter, s_data.sessions[si].name, sizeof(s_filter));
        s_filtered = true;
    }
    // Rebuilding deletes the row this event is running on: do it after.
    lv_async_call(rebuild_async, nullptr);
}

static uint32_t agent_secs(const AgentInfo& a, uint32_t now) {
    if (!a.running || s_rebase) return a.secs;
    const uint32_t s = a.secs + (now - s_data_ms) / 1000;
    return s > AGENTS_MAX_SECS ? AGENTS_MAX_SECS : s;
}

static void session_row(int si) {
    const AgentSession& s = s_data.sessions[si];
    lv_obj_t* r = make_row(true);
    lv_obj_set_style_pad_top(r, 6, 0);
    lv_obj_set_style_pad_bottom(r, 4, 0);
    lv_obj_set_style_radius(r, 6, 0);
    lv_obj_set_style_bg_color(r, THEME_BAR_BG, 0);
    lv_obj_set_style_bg_opa(r, s_filtered ? LV_OPA_COVER : LV_OPA_TRANSP, 0);
    lv_obj_set_style_bg_opa(r, LV_OPA_COVER, LV_STATE_PRESSED);
    lv_obj_add_event_cb(r, on_session_click, LV_EVENT_CLICKED, (void*)(intptr_t)si);

    lv_obj_t* name = make_label(r, &font_styrene_20, THEME_TEXT, s.name);
    lv_label_set_long_mode(name, LV_LABEL_LONG_DOT);
    lv_obj_set_width(name, s_list_w - 170);
    lv_obj_set_pos(name, 4, 0);

    static const char* const words[] = { "en pausa", "trabajando", "esperando agentes" };
    lv_obj_t* st = make_label(r, &font_styrene_16, s.state == AGENTS_PAUSED ? THEME_DIM : THEME_ACCENT,
                              words[s.state]);
    lv_obj_align(st, LV_ALIGN_RIGHT_MID, -4, 0);
}

static void agent_row(int ai) {
    const AgentInfo& a = s_data.agents[ai];
    lv_obj_t* r = make_row(false);
    lv_obj_set_style_pad_bottom(r, 2, 0);

    lv_obj_t* g = make_label(r, &font_mono_18, a.running ? THEME_ACCENT : THEME_GREEN,
                             a.running ? spin_frames[s_spin_idx] : GLYPH_DONE);
    lv_obj_set_pos(g, COL_GLYPH, 0);

    lv_obj_t* type = make_label(r, &font_styrene_16, THEME_TEXT, a.type);
    lv_label_set_long_mode(type, LV_LABEL_LONG_DOT);
    lv_obj_set_width(type, TYPE_W);
    lv_obj_set_pos(type, COL_TYPE, 1);

    lv_obj_t* task = make_label(r, &font_styrene_16, THEME_DIM, a.task);
    // The filtered view has room for the whole task, so it wraps there.
    lv_label_set_long_mode(task, s_filtered ? LV_LABEL_LONG_WRAP : LV_LABEL_LONG_DOT);
    lv_obj_set_width(task, s_list_w - COL_TASK - TIME_W);
    lv_obj_set_pos(task, COL_TASK, 1);

    char buf[12];
    agents_fmt_secs(agent_secs(a, s_time_ms), buf, sizeof(buf));
    lv_obj_t* t = make_label(r, &font_styrene_16, a.running ? THEME_TEXT : THEME_DIM, buf);
    lv_obj_align(t, LV_ALIGN_TOP_RIGHT, -4, 1);

    if (a.running && s_n_live < AGENTS_MAX_AGENTS) {
        Live& l = s_live[s_n_live++];
        l.glyph = g;
        l.time = t;
        l.agent = (uint8_t)ai;
        memcpy(l.shown, buf, sizeof(buf));
    }
}

static void rebuild(void) {
    if (!s_list) return;
    s_n_live = 0;
    lv_obj_clean(s_list);

    if (!s_data.valid) {
        lv_label_set_text(s_count, "--");
        lv_label_set_text(s_sub1, "sin datos");
        lv_label_set_text(s_sub2, "");
        return;
    }
    int running = 0;
    for (int i = 0; i < s_data.n_agents; i++) running += s_data.agents[i].running;
    lv_label_set_text_fmt(s_count, "%d", running);
    lv_label_set_text(s_sub1, "en marcha");
    if (s_data.n_sessions == 1) lv_label_set_text(s_sub2, "1 sesi\xC3\xB3n");
    else lv_label_set_text_fmt(s_sub2, "%d sesiones", s_data.n_sessions);

    if (s_filtered) {   // the shown session may have gone
        bool found = false;
        for (int i = 0; i < s_data.n_sessions; i++)
            found |= strcmp(s_data.sessions[i].name, s_filter) == 0;
        s_filtered = found;
    }
    if (!s_data.n_sessions) {
        note_row("sin agentes");
        return;
    }
    // Active sessions first, paused last; inside one, running agents first.
    for (int pass = 0; pass < 2; pass++) {
        for (int si = 0; si < s_data.n_sessions; si++) {
            const AgentSession& s = s_data.sessions[si];
            if ((s.state == AGENTS_PAUSED) != (pass == 1)) continue;
            if (s_filtered && strcmp(s.name, s_filter) != 0) continue;
            session_row(si);
            for (int run = 1; run >= 0; run--)
                for (int ai = s.first; ai < s.first + s.count; ai++)
                    if (s_data.agents[ai].running == (run == 1)) agent_row(ai);
            if (!s.count) note_row("sin agentes");
        }
    }
}

lv_obj_t* agents_view_create(lv_obj_t* parent, int x, int y, int w, int h) {
    lv_obj_t* card = lv_obj_create(parent);
    lv_obj_set_pos(card, x, y);
    lv_obj_set_size(card, w, h);
    lv_obj_set_style_bg_color(card, THEME_PANEL, 0);
    lv_obj_set_style_bg_opa(card, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(card, 8, 0);
    lv_obj_set_style_border_width(card, 0, 0);
    lv_obj_set_style_pad_hor(card, PAD_X, 0);
    lv_obj_set_style_pad_ver(card, PAD_Y, 0);
    lv_obj_clear_flag(card, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(card, LV_OBJ_FLAG_EVENT_BUBBLE);

    const int lh_big = lv_font_get_line_height(&font_styrene_48);
    const int lh_sub = lv_font_get_line_height(&font_styrene_16);
    s_count = make_label(card, &font_styrene_48, THEME_TEXT, "--");
    lv_obj_set_pos(s_count, 0, 0);
    // Room for two digits, so the words don't move with the count.
    const int sub_x = 2 * lv_font_get_glyph_width(&font_styrene_48, '0', 0) + 12;
    const int sub_y = (lh_big - 2 * lh_sub) / 2;
    s_sub1 = make_label(card, &font_styrene_16, THEME_DIM, "");
    lv_obj_set_pos(s_sub1, sub_x, sub_y);
    s_sub2 = make_label(card, &font_styrene_16, THEME_DIM, "");
    lv_obj_set_pos(s_sub2, sub_x, sub_y + lh_sub);

    lv_obj_t* pill = make_label(card, &font_styrene_20, THEME_TEXT, "Agentes");
    lv_obj_set_style_bg_color(pill, THEME_BAR_BG, 0);
    lv_obj_set_style_bg_opa(pill, LV_OPA_COVER, 0);
    lv_obj_set_style_radius(pill, LV_RADIUS_CIRCLE, 0);
    lv_obj_set_style_pad_hor(pill, PILL_PAD_X, 0);
    lv_obj_set_style_pad_ver(pill, PILL_PAD_Y, 0);
    lv_obj_align(pill, LV_ALIGN_TOP_RIGHT, 0, 1);

    const int list_y = lh_big + 6;
    s_list = lv_obj_create(card);
    lv_obj_remove_style_all(s_list);
    lv_obj_set_pos(s_list, 0, list_y);
    s_list_w = w - 2 * PAD_X;
    lv_obj_set_size(s_list, s_list_w, h - 2 * PAD_Y - list_y);
    lv_obj_set_flex_flow(s_list, LV_FLEX_FLOW_COLUMN);
    lv_obj_set_scroll_dir(s_list, LV_DIR_VER);
    lv_obj_set_scrollbar_mode(s_list, LV_SCROLLBAR_MODE_OFF);
    lv_obj_clear_flag(s_list, LV_OBJ_FLAG_SCROLL_ELASTIC);
    lv_obj_clear_flag(s_list, LV_OBJ_FLAG_CLICKABLE);

    rebuild();
    return card;
}

void agents_view_set(const AgentsData* d) {
    s_data = *d;
    s_rebase = true;
    if (!s_list) return;
    const int32_t y = lv_obj_get_scroll_y(s_list);
    rebuild();
    lv_obj_update_layout(s_list);
    lv_obj_scroll_to_y(s_list, y, LV_ANIM_OFF);   // a refresh keeps your place
}

void agents_view_tick(uint32_t now) {
    if (s_rebase) {
        s_rebase = false;
        s_data_ms = now;
    }
    s_time_ms = now;
    if (!s_n_live) return;

    if (now - s_spin_ms >= spin_ms[s_spin_idx]) {
        s_spin_ms = now;
        s_spin_phase = (s_spin_phase + 1) % SPIN_PHASES;
        s_spin_idx = s_spin_phase < SPIN_COUNT ? s_spin_phase : SPIN_PHASES - s_spin_phase;
        for (int i = 0; i < s_n_live; i++) lv_label_set_text_static(s_live[i].glyph, spin_frames[s_spin_idx]);
    }
    for (int i = 0; i < s_n_live; i++) {
        Live& l = s_live[i];
        char buf[12];
        agents_fmt_secs(agent_secs(s_data.agents[l.agent], now), buf, sizeof(buf));
        if (strcmp(buf, l.shown) != 0) {   // a label redraws only when its text changes
            memcpy(l.shown, buf, sizeof(buf));
            lv_label_set_text(l.time, buf);
        }
    }
}
