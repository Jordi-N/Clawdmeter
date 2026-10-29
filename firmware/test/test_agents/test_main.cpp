// agents_parse: the optional `ag` key, its clamps, and the agents-only
// routing rule (no `ok` → keep the line away from the usage path).
//   pio test -d firmware -e native -f test_agents
#include <unity.h>
#include <string.h>
#include "agents_payload.h"

void setUp(void) {}
void tearDown(void) {}

static void test_full_parse(void) {
    AgentsData d;
    bool only = false;
    TEST_ASSERT_TRUE(agents_parse(
        "{\"ag\":[[\"Clawdmeter\",1,[[\"programador\",\"sesiones\",240,1],"
        "[\"plan\",\"3 p\xC3\xA1ginas\",360,0]]],"
        "[\"Web-MGZ\",2,[[\"Explore\",\"rutas API\",120,1]]],[\"Docs\",0,[]]]}",
        &d, &only));
    TEST_ASSERT_TRUE(only);
    TEST_ASSERT_TRUE(d.valid);
    TEST_ASSERT_EQUAL_INT(3, d.n_sessions);
    TEST_ASSERT_EQUAL_INT(3, d.n_agents);
    TEST_ASSERT_EQUAL_STRING("Clawdmeter", d.sessions[0].name);
    TEST_ASSERT_EQUAL_INT(AGENTS_WORKING, d.sessions[0].state);
    TEST_ASSERT_EQUAL_INT(0, d.sessions[0].first);
    TEST_ASSERT_EQUAL_INT(2, d.sessions[0].count);
    TEST_ASSERT_EQUAL_STRING("programador", d.agents[0].type);
    TEST_ASSERT_EQUAL_STRING("sesiones", d.agents[0].task);
    TEST_ASSERT_EQUAL_UINT32(240, d.agents[0].secs);
    TEST_ASSERT_TRUE(d.agents[0].running);
    TEST_ASSERT_EQUAL_STRING("3 p\xC3\xA1ginas", d.agents[1].task);
    TEST_ASSERT_FALSE(d.agents[1].running);
    TEST_ASSERT_EQUAL_INT(AGENTS_WAITING, d.sessions[1].state);
    TEST_ASSERT_EQUAL_INT(2, d.sessions[1].first);
    TEST_ASSERT_EQUAL_INT(1, d.sessions[1].count);
    TEST_ASSERT_EQUAL_STRING("Docs", d.sessions[2].name);
    TEST_ASSERT_EQUAL_INT(AGENTS_PAUSED, d.sessions[2].state);
    TEST_ASSERT_EQUAL_INT(0, d.sessions[2].count);
}

static void test_clamps_and_truncation(void) {
    AgentsData d;
    // 5 sessions (max 4), 12 agents (max 10), long strings, bad state/secs.
    TEST_ASSERT_TRUE(agents_parse(
        "{\"ag\":["
        "[\"abcdefghijklmnopqrstuvwxyz\",7,[[\"tipo-muy-largo-de-agente\","
        "\"\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1"
        "\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1"
        "\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\xC3\xB1\",-5,1],[\"b\",\"\",9999999,0]]],"
        "[\"s2\",-3,[[\"a\",\"\",1,1],[\"a\",\"\",1,1],[\"a\",\"\",1,1],[\"a\",\"\",1,1]]],"
        "[\"s3\",1,[[\"a\",\"\",1,1],[\"a\",\"\",1,1],[\"a\",\"\",1,1],[\"a\",\"\",1,1],"
        "[\"x\",\"\",1,1],[\"x\",\"\",1,1]]],"
        "[\"s4\",1,[]],[\"s5\",1,[]]],\"ok\":true}", &d, nullptr));
    TEST_ASSERT_EQUAL_INT(AGENTS_MAX_SESSIONS, d.n_sessions);
    TEST_ASSERT_EQUAL_INT(AGENTS_MAX_AGENTS, d.n_agents);
    TEST_ASSERT_EQUAL_STRING("abcdefghijklmnop", d.sessions[0].name);   // 16 chars
    TEST_ASSERT_EQUAL_INT(AGENTS_WAITING, d.sessions[0].state);
    TEST_ASSERT_EQUAL_INT(AGENTS_PAUSED, d.sessions[1].state);
    TEST_ASSERT_EQUAL_STRING("tipo-muy-largo", d.agents[0].type);       // 14 chars
    TEST_ASSERT_EQUAL_INT(24 * 2, strlen(d.agents[0].task));            // 24 two-byte chars, none split
    TEST_ASSERT_EQUAL_UINT32(0, d.agents[0].secs);
    TEST_ASSERT_EQUAL_UINT32(AGENTS_MAX_SECS, d.agents[1].secs);
    // s3 got the last 4 slots, s4 none, s5 dropped.
    TEST_ASSERT_EQUAL_INT(4, d.sessions[2].count);
    TEST_ASSERT_EQUAL_INT(0, d.sessions[3].count);
    TEST_ASSERT_EQUAL_INT(10, d.sessions[3].first);
}

static void test_missing_fields(void) {
    AgentsData d;
    TEST_ASSERT_TRUE(agents_parse(
        "{\"ag\":[[\"solo\"],[],[\"x\",1,[[],[\"t\"],5]]]}", &d, nullptr));
    TEST_ASSERT_EQUAL_INT(3, d.n_sessions);
    TEST_ASSERT_EQUAL_STRING("solo", d.sessions[0].name);
    TEST_ASSERT_EQUAL_INT(AGENTS_PAUSED, d.sessions[0].state);
    TEST_ASSERT_EQUAL_INT(0, d.sessions[0].count);
    TEST_ASSERT_EQUAL_STRING("", d.sessions[1].name);
    TEST_ASSERT_EQUAL_INT(2, d.sessions[2].count);   // the bare 5 is skipped
    TEST_ASSERT_EQUAL_STRING("", d.agents[0].type);
    TEST_ASSERT_EQUAL_UINT32(0, d.agents[0].secs);
    TEST_ASSERT_FALSE(d.agents[0].running);
    TEST_ASSERT_EQUAL_STRING("t", d.agents[1].type);
    TEST_ASSERT_EQUAL_STRING("", d.agents[1].task);
}

static void test_empty_but_valid(void) {
    AgentsData d;
    d.valid = false;
    TEST_ASSERT_TRUE(agents_parse("{\"ag\":[]}", &d, nullptr));
    TEST_ASSERT_TRUE(d.valid);
    TEST_ASSERT_EQUAL_INT(0, d.n_sessions);
    TEST_ASSERT_EQUAL_INT(0, d.n_agents);
}

static void test_routing(void) {
    AgentsData d;
    bool only = true;
    TEST_ASSERT_TRUE(agents_parse("{\"ag\":[],\"s\":12,\"ok\":true}", &d, &only));
    TEST_ASSERT_FALSE(only);                 // carries usage too: feed both
    TEST_ASSERT_TRUE(agents_parse("{\"ag\":[],\"ok\":false}", &d, &only));
    TEST_ASSERT_FALSE(only);                 // `ok` present, even false
    TEST_ASSERT_TRUE(agents_parse("{\"s\":12,\"ag\":[]}", &d, &only));
    TEST_ASSERT_TRUE(only);                  // no `ok`: agents only
}

static void test_absent_or_bad_key(void) {
    AgentsData d;
    memset(&d, 0, sizeof(d));
    d.n_sessions = 3;
    bool only = true;
    TEST_ASSERT_FALSE(agents_parse("{\"s\":12,\"ok\":true}", &d, &only));
    TEST_ASSERT_FALSE(only);
    TEST_ASSERT_FALSE(agents_parse("{\"ag\":5,\"ok\":true}", &d, &only));
    TEST_ASSERT_FALSE(agents_parse("{\"ag\":[", &d, &only));
    TEST_ASSERT_FALSE(agents_parse("{\"m\":\"say \\\"ag\\\"\"}", &d, &only));
    TEST_ASSERT_FALSE(agents_parse(nullptr, &d, &only));
    TEST_ASSERT_FALSE(only);
    TEST_ASSERT_EQUAL_INT(3, d.n_sessions);  // untouched
}

static void test_fmt_secs(void) {
    char b[16];
    agents_fmt_secs(0, b, sizeof(b));      TEST_ASSERT_EQUAL_STRING("0s", b);
    agents_fmt_secs(45, b, sizeof(b));     TEST_ASSERT_EQUAL_STRING("45s", b);
    agents_fmt_secs(240, b, sizeof(b));    TEST_ASSERT_EQUAL_STRING("4m", b);
    agents_fmt_secs(3599, b, sizeof(b));   TEST_ASSERT_EQUAL_STRING("59m", b);
    agents_fmt_secs(3600, b, sizeof(b));   TEST_ASSERT_EQUAL_STRING("1h", b);
    agents_fmt_secs(3900, b, sizeof(b));   TEST_ASSERT_EQUAL_STRING("1h 5m", b);
    agents_fmt_secs(AGENTS_MAX_SECS, b, sizeof(b)); TEST_ASSERT_EQUAL_STRING("99h 59m", b);
}

int main(void) {
    UNITY_BEGIN();
    RUN_TEST(test_full_parse);
    RUN_TEST(test_clamps_and_truncation);
    RUN_TEST(test_missing_fields);
    RUN_TEST(test_empty_but_valid);
    RUN_TEST(test_routing);
    RUN_TEST(test_absent_or_bad_key);
    RUN_TEST(test_fmt_secs);
    return UNITY_END();
}
