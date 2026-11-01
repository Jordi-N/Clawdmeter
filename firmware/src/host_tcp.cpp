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

struct TcpPeer {
    int fd;
    LineBuf lb;
    uint32_t since_ms;
};

static int listen_fd = -1;
static TcpPeer active  = { -1, {}, 0 };   // authenticated
static TcpPeer pending = { -1, {}, 0 };   // connected, waiting for {"auth":...}
static const char* tick_key = nullptr;
static void (*tick_on_line)(const char*) = nullptr;
static bool just_promoted = false;   // pending -> active inside this read

static void close_client(TcpPeer* c) {
    if (c->fd >= 0) close(c->fd);
    c->fd = -1;
    linebuf_reset(&c->lb);
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

// First line of a pending client: the auth. Success replaces the old client.
static bool on_pending_line(const char* line, void*) {
    if (!wifi_auth_ok(line, tick_key) || !write_line(pending.fd, "{\"auth\":\"ok\"}")) {
        Serial.println("WiFi: TCP auth failed, closing");
        close_client(&pending);
        return false;
    }
    close_client(&active);
    active = pending;          // the rest of this read is already its data
    just_promoted = true;
    pending.fd = -1;
    linebuf_reset(&pending.lb);
    Serial.println("WiFi: TCP host authenticated");
    return false;              // stop: the remaining bytes go to active below
}

static bool on_active_line(const char* line, void*) {
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
    }
}
#endif
