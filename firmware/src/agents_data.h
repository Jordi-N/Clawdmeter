#pragma once
#include <stdint.h>

// What the "Agentes" page shows: the host's Claude Code sessions and the
// subagents each one runs. Filled by agents_parse() from the optional `ag`
// key (docs/protocol.md); plain data so the native tests can build it.

#define AGENTS_MAX_SESSIONS   4
#define AGENTS_MAX_AGENTS     10    // across all sessions
#define AGENTS_NAME_CHARS     16    // limits in characters, not bytes
#define AGENTS_TYPE_CHARS     14
#define AGENTS_TASK_CHARS     24
#define AGENTS_MAX_SECS       359999u   // 99h 59m 59s

enum AgentsSessionState : uint8_t {
    AGENTS_PAUSED  = 0,   // en pausa
    AGENTS_WORKING = 1,   // trabajando
    AGENTS_WAITING = 2,   // esperando agentes
};

struct AgentInfo {
    char     type[AGENTS_TYPE_CHARS * 3 + 1];   // UTF-8: up to 3 bytes a character
    char     task[AGENTS_TASK_CHARS * 3 + 1];
    uint32_t secs;      // running: elapsed when sent; done: how long it took
    bool     running;
};

struct AgentSession {
    char    name[AGENTS_NAME_CHARS * 3 + 1];
    uint8_t state;      // AgentsSessionState
    uint8_t first;      // its agents are agents[first .. first + count)
    uint8_t count;
};

struct AgentsData {
    bool         valid;          // false: nothing received yet ("sin datos")
    uint8_t      n_sessions;
    uint8_t      n_agents;
    AgentSession sessions[AGENTS_MAX_SESSIONS];
    AgentInfo    agents[AGENTS_MAX_AGENTS];
};
