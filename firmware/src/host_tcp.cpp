#include "host_wifi.h"
#if BOARD_HAS_WIFI
#include "host_tcp.h"
#include "wifi_proto.h"
#include <Arduino.h>
#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0
#endif

#define AUTH_TIMEOUT_MS 3000
// The app writes at least every 15 s (a {"hb":1} when idle): a silent client
// is gone (the laptop slept or left the network) and is dropped, so the
// device stops counting it as its host and turns Bluetooth back on.
#define IDLE_TIMEOUT_MS 45000

struct TcpPeer {
    int fd;
    LineBuf lb;
    uint32_t since_ms;
};

static int listen_fd = -1;
static TcpPeer active  = { -1, {}, 0 };   // authenticated
static TcpPeer pending = { -1, {}, 0 };   // connected, in the handshake
// The pending client's handshake: its auth MAC, once the hello is answered
// (empty: waiting for the hello).
static char want_auth[WIFI_MAC_LEN + 1];
static const char* tick_key = nullptr;
static void (*tick_on_line)(const char*) = nullptr;
static bool just_promoted = false;   // pending -> active inside this read

static void close_client(TcpPeer* c) {
    if (c->fd >= 0) close(c->fd);
    c->fd = -1;
    linebuf_reset(&c->lb);
    if (c == &pending) want_auth[0] = '\0';
}

static void set_nonblocking(int fd) {
    fcntl(fd, F_SETFL, fcntl(fd, F_GETFL, 0) | O_NONBLOCK);
}

static bool write_line(int fd, const char* line) {
    char out[WIFI_LINE_MAX + 1];
    size_t n = strlen(line);
    if (n >= sizeof(out) - 1) return false;
    memcpy(out, line, n);
    out[n++] = '\n';
    // shortcut: a short write (socket buffer full) drops the client instead of
    // queueing; lines are tiny and the host reconnects, upgrade if it shows up.
    return send(fd, out, n, MSG_NOSIGNAL) == (ssize_t)n;
}

bool tcp_host_start(uint16_t port) {
    if (listen_fd >= 0) return true;
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return false;
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(fd, (struct sockaddr*)&addr, sizeof(addr)) != 0 || listen(fd, 2) != 0) {
        Serial.printf("WiFi: TCP listen on %u failed (errno %d)\n", (unsigned)port, errno);
        close(fd);
        return false;
    }
    set_nonblocking(fd);
    listen_fd = fd;
    Serial.printf("WiFi: TCP listening on %u\n", (unsigned)port);
    return true;
}

void tcp_host_drop_clients(void) {
    close_client(&pending);
    close_client(&active);
}

void tcp_host_stop(void) {
    tcp_host_drop_clients();
    if (listen_fd >= 0) close(listen_fd);
    listen_fd = -1;
}

bool tcp_host_running(void) { return listen_fd >= 0; }
bool tcp_host_authed(void) { return active.fd >= 0; }

void tcp_host_send(const char* line) {
    if (active.fd >= 0 && !write_line(active.fd, line)) {
        Serial.println("WiFi: TCP client write failed, dropping it");
        close_client(&active);
    }
}

static void hex(char* out, const uint8_t* b, size_t n) {
    static const char H[] = "0123456789abcdef";
    for (size_t i = 0; i < n; i++) { out[2 * i] = H[b[i] >> 4]; out[2 * i + 1] = H[b[i] & 15]; }
    out[2 * n] = '\0';
}

// The hello: answer with our nonce and the proof that we know the key.
static bool answer_hello(const char* line) {
    char cn[WIFI_NONCE_LEN + 1], dn[WIFI_NONCE_LEN + 1], proof[WIFI_MAC_LEN + 1];
    if (!wifi_hello_parse(line, cn)) return false;
    uint8_t r[WIFI_NONCE_LEN / 2];
    radio_random(r, sizeof(r));
    hex(dn, r, sizeof(r));
    if (!wifi_auth_mac(tick_key, "clawd-dev", cn, dn, proof)) return false;
    if (!wifi_auth_mac(tick_key, "clawd-app", cn, dn, want_auth)) return false;
    char reply[160];
    snprintf(reply, sizeof(reply), "{\"nonce\":\"%s\",\"proof\":\"%s\"}", dn, proof);
    return write_line(pending.fd, reply);
}

// A pending client's lines: the hello, then the auth. Success replaces the
// old client.
static bool on_pending_line(const char* line, void*) {
    if (!want_auth[0]) {
        if (answer_hello(line)) return true;
        Serial.println("WiFi: TCP hello refused, closing");
        close_client(&pending);
        return false;
    }
    if (!wifi_auth_check(line, want_auth) || !write_line(pending.fd, "{\"auth\":\"ok\"}")) {
        Serial.println("WiFi: TCP auth failed, closing");
        close_client(&pending);
        return false;
    }
    want_auth[0] = '\0';
    close_client(&active);
    active = pending;          // the rest of this read is already its data
    active.since_ms = millis();
    just_promoted = true;
    pending.fd = -1;
    linebuf_reset(&pending.lb);
    Serial.println("WiFi: TCP host authenticated");
    return false;              // stop: the remaining bytes go to active below
}

static bool on_active_line(const char* line, void*) {
    active.since_ms = millis();   // for the active client: its last line
    tick_on_line(line);
    return active.fd >= 0;     // the handler may have dropped the client
}

// Reads what's there (bounded per tick). Returns false when the peer closed.
static bool pump(TcpPeer* c, bool (*on_line)(const char*, void*)) {
    char buf[256];
    for (int i = 0; i < 4 && c->fd >= 0; i++) {
        ssize_t n = recv(c->fd, buf, sizeof(buf), 0);
        if (n == 0) return false;
        if (n < 0) return errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR;
        size_t used = 0;
        while (used < (size_t)n && c->fd >= 0) {
            just_promoted = false;
            used += linebuf_feed(&c->lb, buf + used, (size_t)n - used, on_line, nullptr);
            if (just_promoted) { c = &active; on_line = on_active_line; }
        }
    }
    return true;
}

void tcp_host_tick(const char* key, void (*on_line)(const char* line)) {
    if (listen_fd < 0) return;
    tick_key = key;
    tick_on_line = on_line;

    int fd = accept(listen_fd, nullptr, nullptr);
    if (fd >= 0) {
        close_client(&pending);   // one handshake at a time; newest wins
        set_nonblocking(fd);
        int one = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
        pending.fd = fd;
        pending.since_ms = millis();
        linebuf_reset(&pending.lb);
    }

    if (pending.fd >= 0) {
        if (millis() - pending.since_ms >= AUTH_TIMEOUT_MS) {
            Serial.println("WiFi: TCP auth timeout, closing");
            close_client(&pending);
        } else if (!pump(&pending, on_pending_line)) {
            close_client(pending.fd >= 0 ? &pending : &active);
        }
    }
    if (active.fd >= 0 && !pump(&active, on_active_line)) {
        Serial.println("WiFi: TCP host disconnected");
        close_client(&active);
    } else if (active.fd >= 0 && millis() - active.since_ms >= IDLE_TIMEOUT_MS) {
        Serial.println("WiFi: TCP host silent for 45 s, dropping it");
        close_client(&active);
    }
}
#endif
