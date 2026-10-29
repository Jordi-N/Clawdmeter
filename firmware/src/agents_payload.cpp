#include "agents_payload.h"
#include <ArduinoJson.h>
#include <stdio.h>
#include <string.h>

// Copy at most max_chars UTF-8 characters, never splitting one and always
// leaving room for the NUL.
static void copy_utf8(char* dst, size_t cap, const char* src, int max_chars) {
    size_t n = 0;
    for (int chars = 0; src[n] && chars < max_chars; chars++) {
        const uint8_t c = (uint8_t)src[n];
        size_t len = c >= 0xF0 ? 4 : c >= 0xE0 ? 3 : c >= 0xC0 ? 2 : 1;
        if (n + len >= cap) break;
        size_t k = 1;
        while (k < len && src[n + k]) k++;
        if (k < len) break;   // truncated sequence at the end of the string
        n += len;
    }
    memcpy(dst, src, n);
    dst[n] = '\0';
}

static long clamp_l(long v, long lo, long hi) { return v < lo ? lo : v > hi ? hi : v; }

bool agents_parse(const char* json, AgentsData* out, bool* agents_only) {
    if (agents_only) *agents_only = false;
    // Most lines are usage-only: skip the second deserialize for them.
    if (!json || !strstr(json, "\"ag\"")) return false;
    JsonDocument doc;
    if (deserializeJson(doc, json)) return false;
    JsonArray ag = doc["ag"].as<JsonArray>();
    if (ag.isNull()) return false;

    AgentsData d;
    memset(&d, 0, sizeof(d));
    for (JsonVariant sv : ag) {
        if (d.n_sessions >= AGENTS_MAX_SESSIONS) break;
        JsonArray s = sv.as<JsonArray>();
        if (s.isNull()) continue;
        AgentSession& ss = d.sessions[d.n_sessions++];
        copy_utf8(ss.name, sizeof(ss.name), s[0] | "", AGENTS_NAME_CHARS);
        ss.state = (uint8_t)clamp_l(s[1].as<long>(), AGENTS_PAUSED, AGENTS_WAITING);
        ss.first = d.n_agents;
        for (JsonVariant av : s[2].as<JsonArray>()) {
            if (d.n_agents >= AGENTS_MAX_AGENTS) break;
            JsonArray a = av.as<JsonArray>();
            if (a.isNull()) continue;
            AgentInfo& ai = d.agents[d.n_agents++];
            copy_utf8(ai.type, sizeof(ai.type), a[0] | "", AGENTS_TYPE_CHARS);
            copy_utf8(ai.task, sizeof(ai.task), a[1] | "", AGENTS_TASK_CHARS);
            ai.secs = (uint32_t)clamp_l(a[2].as<long>(), 0, AGENTS_MAX_SECS);
            ai.running = a[3].as<bool>();
        }
        ss.count = d.n_agents - ss.first;
    }
    d.valid = true;
    *out = d;
    if (agents_only) *agents_only = doc["ok"].isNull();
    return true;
}

void agents_fmt_secs(uint32_t s, char* buf, size_t cap) {
    if (s < 60)        snprintf(buf, cap, "%us", (unsigned)s);
    else if (s < 3600) snprintf(buf, cap, "%um", (unsigned)(s / 60));
    else if (s % 3600 < 60) snprintf(buf, cap, "%uh", (unsigned)(s / 3600));
    else               snprintf(buf, cap, "%uh %um", (unsigned)(s / 3600), (unsigned)(s % 3600 / 60));
}
