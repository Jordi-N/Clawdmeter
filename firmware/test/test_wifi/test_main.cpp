// wifi_proto: the `wifi` provisioning key, the TCP auth line, newline
// framing, the status object and the reconnect backoff.
//   pio test -d firmware -e native -f test_wifi
#include <unity.h>
#include <string.h>
#include "wifi_proto.h"

void setUp(void) {}
void tearDown(void) {}

static const char* KEY = "0123456789abcdef0123456789ABCDEF";

static void test_set(void) {
    WifiCmd c;
    bool only = false;
    TEST_ASSERT_TRUE(wifi_cmd_parse(
        "{\"wifi\":{\"ssid\":\"Casa\",\"pass\":\"secreto123\","
        "\"key\":\"0123456789abcdef0123456789ABCDEF\",\"en\":true}}", &c, &only));
    TEST_ASSERT_TRUE(only);
    TEST_ASSERT_EQUAL_INT(WIFI_CMD_SET, c.kind);
    TEST_ASSERT_TRUE(c.en);
    TEST_ASSERT_EQUAL_STRING("Casa", c.ssid);
    TEST_ASSERT_EQUAL_STRING("secreto123", c.pass);
    TEST_ASSERT_EQUAL_STRING("0123456789abcdef0123456789abcdef", c.key);  // lowercased
}

static void test_enable_forget(void) {
    WifiCmd c;
    bool only = false;
    TEST_ASSERT_TRUE(wifi_cmd_parse("{\"wifi\":{\"en\":false}}", &c, &only));
    TEST_ASSERT_EQUAL_INT(WIFI_CMD_ENABLE, c.kind);
    TEST_ASSERT_FALSE(c.en);
    TEST_ASSERT_TRUE(wifi_cmd_parse("{\"wifi\":{\"en\":true}}", &c, &only));
    TEST_ASSERT_TRUE(c.en);
    TEST_ASSERT_TRUE(wifi_cmd_parse("{\"wifi\":null}", &c, &only));
    TEST_ASSERT_EQUAL_INT(WIFI_CMD_FORGET, c.kind);
    TEST_ASSERT_TRUE(only);
}

static void test_invalid(void) {
    WifiCmd c;
    bool only = false;
    // Bad key length / non-hex / empty ssid / too-long pass / no en.
    TEST_ASSERT_FALSE(wifi_cmd_parse("{\"wifi\":{\"ssid\":\"a\",\"key\":\"abc\"}}", &c, &only));
    TEST_ASSERT_TRUE(only);   // still not usage data
    TEST_ASSERT_FALSE(wifi_cmd_parse(
        "{\"wifi\":{\"ssid\":\"a\",\"key\":\"0123456789abcdef0123456789abcdeg\"}}", &c, &only));
    TEST_ASSERT_FALSE(wifi_cmd_parse(
        "{\"wifi\":{\"ssid\":\"\",\"key\":\"0123456789abcdef0123456789abcdef\"}}", &c, &only));
    TEST_ASSERT_FALSE(wifi_cmd_parse(
        "{\"wifi\":{\"ssid\":\"a\",\"pass\":\"0123456789012345678901234567890123456789012345678901234567890123\","
        "\"key\":\"0123456789abcdef0123456789abcdef\"}}", &c, &only));
    TEST_ASSERT_FALSE(wifi_cmd_parse("{\"wifi\":{}}", &c, &only));
    TEST_ASSERT_FALSE(wifi_cmd_parse("{\"wifi\":3}", &c, &only));
}

static void test_routing(void) {
    WifiCmd c;
    bool only = true;
    TEST_ASSERT_FALSE(wifi_cmd_parse("{\"s\":4,\"ok\":true}", &c, &only));
    TEST_ASSERT_FALSE(only);
    TEST_ASSERT_TRUE(wifi_cmd_parse("{\"s\":4,\"ok\":true,\"wifi\":{\"en\":true}}", &c, &only));
    TEST_ASSERT_FALSE(only);   // rides with usage: usage path still runs
    TEST_ASSERT_FALSE(wifi_cmd_parse("not json \"wifi\"", &c, &only));
    TEST_ASSERT_FALSE(only);
}

static void test_auth(void) {
    TEST_ASSERT_TRUE(wifi_auth_ok("{\"auth\":\"0123456789abcdef0123456789abcdef\"}", KEY));
    TEST_ASSERT_TRUE(wifi_auth_ok("{\"auth\":\"0123456789ABCDEF0123456789abcdef\"}", KEY));
    TEST_ASSERT_FALSE(wifi_auth_ok("{\"auth\":\"0123456789abcdef0123456789abcdee\"}", KEY));
    TEST_ASSERT_FALSE(wifi_auth_ok("{\"auth\":\"\"}", KEY));
    TEST_ASSERT_FALSE(wifi_auth_ok("{\"s\":1}", KEY));
    TEST_ASSERT_FALSE(wifi_auth_ok("garbage", KEY));
    TEST_ASSERT_FALSE(wifi_auth_ok("{\"auth\":\"0123456789abcdef0123456789abcdef\"}", ""));
}

struct Lines { char got[4][WIFI_LINE_MAX]; int n; int stop_after; };
static bool collect(const char* line, void* ctx) {
    Lines* l = (Lines*)ctx;
    if (l->n < 4) strcpy(l->got[l->n], line);
    l->n++;
    return l->stop_after == 0 || l->n < l->stop_after;
}

static void test_framing(void) {
    LineBuf lb;
    linebuf_reset(&lb);
    Lines l = {};
    const char* a = "{\"a\":1}\r\n\n{\"b\"";
    TEST_ASSERT_EQUAL_UINT(strlen(a), linebuf_feed(&lb, a, strlen(a), collect, &l));
    TEST_ASSERT_EQUAL_INT(1, l.n);
    TEST_ASSERT_EQUAL_STRING("{\"a\":1}", l.got[0]);
    linebuf_feed(&lb, ":2}\n", 4, collect, &l);   // split across reads
    TEST_ASSERT_EQUAL_INT(2, l.n);
    TEST_ASSERT_EQUAL_STRING("{\"b\":2}", l.got[1]);

    // An overlong line is dropped whole; the next one still arrives.
    static char big[WIFI_LINE_MAX + 20];
    memset(big, 'x', sizeof(big));
    big[sizeof(big) - 1] = '\n';
    linebuf_feed(&lb, big, sizeof(big), collect, &l);
    TEST_ASSERT_EQUAL_INT(2, l.n);
    linebuf_feed(&lb, "ok\n", 3, collect, &l);
    TEST_ASSERT_EQUAL_INT(3, l.n);
    TEST_ASSERT_EQUAL_STRING("ok", l.got[2]);

    // on_line returning false stops consumption right after that line.
    Lines s = {};
    s.stop_after = 1;
    linebuf_reset(&lb);
    TEST_ASSERT_EQUAL_UINT(2, linebuf_feed(&lb, "a\nb\n", 4, collect, &s));
    TEST_ASSERT_EQUAL_INT(1, s.n);
}

static void test_status_backoff(void) {
    char buf[96];
    TEST_ASSERT_TRUE(wifi_status_json(buf, sizeof(buf), WIFI_ST_OK, "192.168.1.5", "Casa \"2\"") > 0);
    TEST_ASSERT_EQUAL_STRING("{\"st\":\"ok\",\"ip\":\"192.168.1.5\",\"ssid\":\"Casa \\\"2\\\"\"}", buf);
    wifi_status_json(buf, sizeof(buf), WIFI_ST_OFF, "", nullptr);
    TEST_ASSERT_EQUAL_STRING("{\"st\":\"off\"}", buf);
    TEST_ASSERT_EQUAL_UINT(0, wifi_status_json(buf, 8, WIFI_ST_CONNECTING, "", ""));
    TEST_ASSERT_EQUAL_UINT32(2000, wifi_backoff_ms(0));
    TEST_ASSERT_EQUAL_UINT32(4000, wifi_backoff_ms(1));
    TEST_ASSERT_EQUAL_UINT32(60000, wifi_backoff_ms(5));
    TEST_ASSERT_EQUAL_UINT32(60000, wifi_backoff_ms(40));
}

int main(int, char**) {
    UNITY_BEGIN();
    RUN_TEST(test_set);
    RUN_TEST(test_enable_forget);
    RUN_TEST(test_invalid);
    RUN_TEST(test_routing);
    RUN_TEST(test_auth);
    RUN_TEST(test_framing);
    RUN_TEST(test_status_backoff);
    return UNITY_END();
}
