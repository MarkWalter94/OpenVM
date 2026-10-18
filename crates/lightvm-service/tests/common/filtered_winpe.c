/* Disposable WinPE probe: normal certificate checks, bounded TCP attempts. */
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <winhttp.h>
#include <stdio.h>
#include <string.h>

static int reachable(const char *name, unsigned short port) {
    struct sockaddr_in address = {0};
    SOCKET sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    u_long nonblocking = 1;
    fd_set write_set, error_set;
    struct timeval timeout = {1, 0};
    int error = 0, size = sizeof(error), selected;
    if (sock == INVALID_SOCKET) return 0;
    address.sin_family = AF_INET;
    address.sin_port = htons(port);
    if (inet_pton(AF_INET, name, &address.sin_addr) != 1) { closesocket(sock); return 0; }
    ioctlsocket(sock, FIONBIO, &nonblocking);
    if (connect(sock, (struct sockaddr *)&address, sizeof(address)) == 0) { closesocket(sock); return 1; }
    FD_ZERO(&write_set); FD_ZERO(&error_set);
    FD_SET(sock, &write_set); FD_SET(sock, &error_set);
    selected = select(0, NULL, &write_set, &error_set, &timeout);
    if (selected > 0 && getsockopt(sock, SOL_SOCKET, SO_ERROR, (char *)&error, &size) == 0 && error == 0) {
        closesocket(sock); return 1;
    }
    closesocket(sock); return 0;
}

int main(int argc, char **argv) {
    WSADATA data;
    struct addrinfo hints = {0}, *answer = NULL;
    char ip[64] = {0};
    HINTERNET session = NULL, connection = NULL, request = NULL;
    DWORD status = 0, length = sizeof(status), bytes = 0, failure = 0;
    char body[1024];
    int dns_ok = 0, unknown_blocked, direct_blocked, host_blocked, port_blocked, ok;
    if (argc != 2 || WSAStartup(MAKEWORD(2, 2), &data) != 0) return 2;
    printf("nonce=%s\n", argv[1]);
    hints.ai_family = AF_INET; hints.ai_socktype = SOCK_STREAM;
    if (getaddrinfo("example.com", "443", &hints, &answer) == 0 && answer != NULL) {
        struct sockaddr_in *address = (struct sockaddr_in *)answer->ai_addr;
        inet_ntop(AF_INET, &address->sin_addr, ip, sizeof(ip));
        dns_ok = strcmp(ip, "10.0.2.101") == 0;
        freeaddrinfo(answer); answer = NULL;
    }
    printf("dns=%s\n", ip);
    unknown_blocked = getaddrinfo("ungranted.lightvm.invalid", "443", &hints, &answer) != 0;
    if (answer) freeaddrinfo(answer);
    direct_blocked = !reachable("1.1.1.1", 443);
    host_blocked = !reachable("10.0.2.2", 443);
    port_blocked = !reachable("10.0.2.101", 80);
    printf("unknown_blocked=%d\ndirect_blocked=%d\nhost_blocked=%d\nport_blocked=%d\n",
           unknown_blocked, direct_blocked, host_blocked, port_blocked);
    session = WinHttpOpen(L"LightVM WinPE qualification", WINHTTP_ACCESS_TYPE_NO_PROXY,
                          WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (session) {
        WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000);
        connection = WinHttpConnect(session, L"example.com", 443, 0);
    }
    if (connection) request = WinHttpOpenRequest(connection, L"GET", L"/", NULL,
        WINHTTP_NO_REFERER, WINHTTP_DEFAULT_ACCEPT_TYPES, WINHTTP_FLAG_SECURE);
    if (request && WinHttpSendRequest(request, WINHTTP_NO_ADDITIONAL_HEADERS, 0,
            WINHTTP_NO_REQUEST_DATA, 0, 0, 0) && WinHttpReceiveResponse(request, NULL)) {
        WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                            NULL, &status, &length, NULL);
        WinHttpReadData(request, body, sizeof(body), &bytes);
    } else failure = GetLastError();
    printf("https_status=%lu\nhttps_bytes=%lu\nhttps_error=%lu\n", status, bytes, failure);
    ok = dns_ok && unknown_blocked && direct_blocked && host_blocked && port_blocked && status == 200 && bytes > 0;
    printf("passed=%d\n", ok);
    if (request) WinHttpCloseHandle(request);
    if (connection) WinHttpCloseHandle(connection);
    if (session) WinHttpCloseHandle(session);
    WSACleanup();
    return ok ? 0 : 1;
}
