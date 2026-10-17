"""Guest application delivered through Tools; no host execution or shell RPC."""
import json
import pathlib
import socket
import ssl
import time

nonce = "NONCE_PLACEHOLDER"
hold = HOLD_PLACEHOLDER
out = pathlib.Path.home() / 'Documents/LightVM Tools/outbox'
result = dict(nonce=nonce)
try:
    result['dns'] = socket.gethostbyname('example.com')
    assert result['dns'] == '10.0.2.101'
    context = ssl.create_default_context()
    with socket.create_connection(('example.com', 443), timeout=5) as raw:
        with context.wrap_socket(raw, server_hostname='example.com') as tls:
            result['tls'] = tls.version()
            tls.sendall(b'GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n')
            response = tls.recv(4096).split(b'\r\n', 1)[0].decode('ascii')
            assert ' 200 ' in response, response
            result['http'] = response
    # These attempts must still fail after recovery; no bypass NIC is added.
    for address in [('1.1.1.1', 443), ('10.0.2.2', 443), ('10.0.2.101', 80)]:
        try:
            connection = socket.create_connection(address, timeout=0.5)
        except OSError:
            continue
        connection.close()
        raise AssertionError('unexpected reachability: ' + str(address))
    result['blocked_attempts'] = 3
    with socket.create_connection(('example.com', 443), timeout=5) as raw:
        with context.wrap_socket(raw, server_hostname='example.com') as tls:
            result['persistent_tls'] = tls.version()
            temp = out / 'network-connected.tmp'
            temp.write_text(json.dumps(result), encoding='utf-8')
            temp.replace(out / 'network-connected.json')
            if hold:
                time.sleep(12)
except Exception as error:
    result['error'] = repr(error)
    temp = out / 'network-connected.tmp'
    temp.write_text(json.dumps(result), encoding='utf-8')
    temp.replace(out / 'network-connected.json')
    raise
print(json.dumps(result))
