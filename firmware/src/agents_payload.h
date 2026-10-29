#pragma once
#include <stddef.h>
#include "agents_data.h"

// The optional `ag` key of a BLE payload line (fork-only; see
// docs/protocol.md). Pure: no Arduino, no LVGL, runs in the native tests.
//
//   {"ag":[[name, state, [[type, task, secs, running], ...]], ...]}
//
// Returns true when the line carries an `ag` array; `out` is then fully
// overwritten (valid = true), with counts and strings clamped to the limits
// in agents_data.h. Returns false (out untouched) on malformed JSON or no
// `ag`. `*agents_only` (if non-null) is set to true when the line has `ag`
// but no `ok`: such a line must not reach payload_parse/ui_update, whose
// absent usage keys would read as zeros.
bool agents_parse(const char* json, AgentsData* out, bool* agents_only);

// "45s", "4m", "1h 5m", "2h".
void agents_fmt_secs(uint32_t secs, char* buf, size_t cap);
