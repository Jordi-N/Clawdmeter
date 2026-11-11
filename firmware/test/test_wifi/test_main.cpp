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

static void hexs(char* out, const uint8_t* b, size_t n) {
    static const char H[] = "0123456789abcdef";
    for (size_t i = 0; i < n; i++) { out[2 * i] = H[b[i] >> 4]; out[2 * i + 1] = H[b[i] & 15]; }
    out[2 * n] = '\0';
}

static void test_sha256_hmac(void) {
    uint8_t d[32];
    char h[65];
    wifi_sha256((const uint8_t*)"abc", 3, d);
    hexs(h, d, 32);
    TEST_ASSERT_EQUAL_STRING("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", h);
    // 56 bytes: the length spills into a second padding block.
    uint8_t a56[56];
    memset(a56, 'a', sizeof(a56));
    wifi_sha256(a56, sizeof(a56), d);
    hexs(h, d, 32);
    TEST_ASSERT_EQUAL_STRING("b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a", h);
    // RFC 4231 cases 2 and 6 (a key longer than a block is hashed first).
    const char* msg2 = "what do ya want for nothing?";
    wifi_hmac_sha256((const uint8_t*)"Jefe", 4, (const uint8_t*)msg2, strlen(msg2), d);
    hexs(h, d, 32);
    TEST_ASSERT_EQUAL_STRING("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843", h);
    uint8_t k6[131];
    memset(k6, 0xaa, sizeof(k6));
    const char* msg6 = "Test Using Larger Than Block-Size Key - Hash Key First";
    wifi_hmac_sha256(k6, sizeof(k6), (const uint8_t*)msg6, strlen(msg6), d);
    hexs(h, d, 32);
    TEST_ASSERT_EQUAL_STRING("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54", h);
}

static void test_auth(void) {
    // Same vector as the app's `handshake_macs` test (clawdmeter-core wifi.rs).
    const char* cn = "00112233445566778899aabbccddeeff";
    const char* dn = "ffeeddccbbaa99887766554433221100";
    char mac[WIFI_MAC_LEN + 1];
    TEST_ASSERT_TRUE(wifi_auth_mac(KEY, "clawd-dev", cn, dn, mac));
    TEST_ASSERT_EQUAL_STRING("b25373f2a04516f222890f7c51695e305c9441d0aa78afae3941f3eda63b047b", mac);
    TEST_ASSERT_TRUE(wifi_auth_mac(KEY, "clawd-app", cn, dn, mac));
    TEST_ASSERT_EQUAL_STRING("0270cf6e163ea087baac72102c05606e60bed122d519e0e18e721119ece1a5a4", mac);
    TEST_ASSERT_FALSE(wifi_auth_mac("", "clawd-app", cn, dn, mac));

    char got[WIFI_NONCE_LEN + 1];
    TEST_ASSERT_TRUE(wifi_hello_parse("{\"hello\":\"00112233445566778899AABBCCDDEEFF\"}", got));
    TEST_ASSERT_EQUAL_STRING(cn, got);
    TEST_ASSERT_FALSE(wifi_hello_parse("{\"hello\":\"0011\"}", got));
    TEST_ASSERT_FALSE(wifi_hello_parse("{\"auth\":\"0123456789abcdef0123456789abcdef\"}", got));

    char line[96];
    snprintf(line, sizeof(line), "{\"auth\":\"%s\"}", mac);
    TEST_ASSERT_TRUE(wifi_auth_check(line, mac));
    TEST_ASSERT_FALSE(wifi_auth_check(line, "0270cf6e163ea087baac72102c05606e60bed122d519e0e18e721119ece1a5a5"));
    // The old handshake (the key itself) no longer passes.
    TEST_ASSERT_FALSE(wifi_auth_check("{\"auth\":\"0123456789abcdef0123456789abcdef\"}", mac));
    TEST_ASSERT_FALSE(wifi_auth_check("garbage", mac));
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
    RUN_TEST(test_sha256_hmac);
    RUN_TEST(test_auth);
    RUN_TEST(test_framing);
    RUN_TEST(test_status_backoff);
    return UNITY_END();
}
