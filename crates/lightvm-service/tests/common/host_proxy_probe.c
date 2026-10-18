/* Native host canary: verify the configured PAC is visible to Windows. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <winhttp.h>
#include <wininet.h>
#include <stdio.h>
#include <wchar.h>
#include <stdlib.h>
#pragma comment(lib, "wininet.lib")

static int pac_option(int argc, wchar_t **argv) {
    INTERNET_PER_CONN_OPTIONW entries[2] = {0};
    INTERNET_PER_CONN_OPTION_LISTW list = {0};
    DWORD length = sizeof(list);
    list.dwSize = sizeof(list);
    list.dwOptionCount = 1;
    list.pOptions = entries;
    entries[0].dwOption = INTERNET_PER_CONN_FLAGS;
    if (argc == 2 && wcscmp(argv[1], L"--flags") == 0) {
        if (!InternetQueryOptionW(NULL, INTERNET_OPTION_PER_CONNECTION_OPTION, &list, &length)) return 2;
        printf("%lu\n", entries[0].Value.dwValue);
        return 0;
    }
    if (argc == 4 && wcscmp(argv[1], L"--pac") == 0) {
        wchar_t *end = NULL;
        unsigned long flags = wcstoul(argv[3], &end, 10);
        int clear = wcscmp(argv[2], L"-") == 0;
        if (!end || *end || flags > 15 || (flags & PROXY_TYPE_PROXY)) return 2;
        if (!clear && wcsncmp(argv[2], L"http://127.0.0.1:", 17) != 0) return 2;
        list.dwOptionCount = 2;
        entries[0].Value.dwValue = flags;
        entries[1].dwOption = INTERNET_PER_CONN_AUTOCONFIG_URL;
        entries[1].Value.pszValue = clear ? L"" : argv[2];
        if (!InternetSetOptionW(NULL, INTERNET_OPTION_PER_CONNECTION_OPTION, &list, sizeof(list))) return 2;
        InternetSetOptionW(NULL, INTERNET_OPTION_SETTINGS_CHANGED, NULL, 0);
        InternetSetOptionW(NULL, INTERNET_OPTION_REFRESH, NULL, 0);
        return 0;
    }
    return -1;
}

int wmain(int argc, wchar_t **argv) {
    WINHTTP_CURRENT_USER_IE_PROXY_CONFIG config = {0};
    WINHTTP_AUTOPROXY_OPTIONS options = {0};
    WINHTTP_PROXY_INFO result = {0};
    HINTERNET session = NULL;
    int matched = 0;
    int option_result = pac_option(argc, argv);
    if (option_result >= 0) return option_result;
    if (!WinHttpGetIEProxyConfigForCurrentUser(&config)) return 2;
    printf("autodetect=%d\npac_present=%d\nproxy_present=%d\n", config.fAutoDetect,
           config.lpszAutoConfigUrl != NULL, config.lpszProxy != NULL);
    if (argc == 1) { matched = 1; goto done; }
    if (argc != 3 || config.lpszAutoConfigUrl == NULL) goto done;
    session = WinHttpOpen(L"LightVM host proxy canary", WINHTTP_ACCESS_TYPE_NO_PROXY,
                         WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (!session) goto done;
    WinHttpSetTimeouts(session, 5000, 5000, 5000, 5000);
    options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
    options.lpszAutoConfigUrl = config.lpszAutoConfigUrl;
    options.fAutoLogonIfChallenged = FALSE;
    if (WinHttpGetProxyForUrl(session, argv[1], &options, &result)) {
        matched = result.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY &&
            result.lpszProxy != NULL && wcsstr(result.lpszProxy, argv[2]) != NULL;
    } else printf("proxy_error=%lu\n", GetLastError());
    printf("configured_proxy_selected=%d\n", matched);
done:
    if (result.lpszProxy) GlobalFree(result.lpszProxy);
    if (result.lpszProxyBypass) GlobalFree(result.lpszProxyBypass);
    if (session) WinHttpCloseHandle(session);
    if (config.lpszAutoConfigUrl) GlobalFree(config.lpszAutoConfigUrl);
    if (config.lpszProxy) GlobalFree(config.lpszProxy);
    if (config.lpszProxyBypass) GlobalFree(config.lpszProxyBypass);
    return matched ? 0 : 1;
}
