#pragma once
#include <stdint.h>
#include <lvgl.h>
#include "agents_data.h"

// The "Agentes" card: how many subagents are running, and a scrollable list
// of sessions with their agents. Tap a session's row to show only that one
// (full task text); tap it again for all. Rows let horizontal swipes bubble
// up as gestures, so the page switcher still sees them.

// Builds the card filling (x, y, w, h) of `parent`. Call once.
lv_obj_t* agents_view_create(lv_obj_t* parent, int x, int y, int w, int h);

// Keeps a copy (safe before the view exists) and rebuilds the list.
void agents_view_set(const AgentsData* d);

// Every loop: advances running agents' timers and the spinner. Cheap.
void agents_view_tick(uint32_t now_ms);
