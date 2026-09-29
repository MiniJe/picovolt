#!/usr/bin/env python3
"""Linux network/resource regression for picovolt-server's pre-auth ingress.

Run with: python3 scripts/verify_http_ingress.py target/debug/picovolt-server
The binary is launched with a token and observed through /proc. This test uses
real sockets because handler/unit tests cannot exercise pre-handler parsing.
"""

import json
import os
import socket
import subprocess
import sys
import time


def metrics(pid):
    with open(f"/proc/{pid}/status", encoding="ascii") as status:
        lines = status.readlines()
    fields = dict(line.split(":", 1) for line in lines if ":" in line)
    return {
        "threads": int(fields["Threads"].strip()),
        "rss_kib": int(fields["VmRSS"].strip().split()[0]),
        "fds": len(os.listdir(f"/proc/{pid}/fd")),
    }


def connect(port):
    stream = socket.create_connection(("127.0.0.1", port), timeout=2)
    stream.settimeout(8)
    return stream


def response(stream):
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = stream.recv(4096)
        if not chunk:
            break
        data.extend(chunk)
    return bytes(data).split(b"\r\n", 1)[0]


def request(port, head, body=b""):
    with connect(port) as stream:
        stream.sendall(head + body)
        return response(stream)


def expect_status(actual, status):
    assert actual.startswith(f"HTTP/1.1 {status} ".encode()), actual


def run(binary):
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    env = dict(os.environ, PICOVOLT_SERVER_TOKEN="ingress-test-token")
    process = subprocess.Popen(
        [binary, "--addr", f"127.0.0.1:{port}", "--memory"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    held = []
    try:
        for _ in range(100):
            try:
                if request(port, b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n").startswith(b"HTTP/1.1 200 "):
                    break
            except OSError:
                pass
            if process.poll() is not None:
                raise AssertionError(process.stderr.read().decode())
            time.sleep(0.05)
        else:
            raise AssertionError("server did not start")

        idle = metrics(process.pid)
        auth = b"Authorization: Bearer ingress-test-token\r\n"
        setup = b'{"sql":"CREATE TABLE ingress_test (id)"}'
        setup_head = (b"POST /v1/query HTTP/1.1\r\nHost: localhost\r\n" + auth
                      + b"Content-Type: application/json\r\nContent-Length: "
                      + str(len(setup)).encode() + b"\r\n\r\n")
        expect_status(request(port, setup_head, setup), 200)
        query = b'{"sql":"SELECT COUNT(*) FROM ingress_test"}'
        head = (b"POST /v1/query HTTP/1.1\r\nHost: localhost\r\n" + auth
                + b"Content-Type: application/json\r\nContent-Length: "
                + str(len(query)).encode() + b"\r\n\r\n")
        expect_status(request(port, head, query), 200)
        expect_status(request(port, head.replace(auth, b""), query), 401)
        expect_status(request(port, head.replace(b"Content-Type: application/json", b"Content-Type: text/plain"), query), 415)
        expect_status(request(port, head.replace(b"Content-Type:", b"Origin: https://example.invalid\r\nContent-Type:"), query), 403)
        chunked = (b"POST /v1/query HTTP/1.1\r\nHost: localhost\r\n" + auth
                   + b"Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n")
        expect_status(request(port, chunked, f"{len(query):x}\r\n".encode() + query + b"\r\n0\r\n\r\n"), 200)

        # A valid body close to the retained 1 MiB application limit.
        large = json.dumps({"sql": "SELECT COUNT(*) FROM ingress_test", "padding": "x" * 900_000}).encode()
        large_head = (b"POST /v1/query HTTP/1.1\r\nHost: localhost\r\n" + auth
                      + b"Content-Type: application/json\r\nContent-Length: "
                      + str(len(large)).encode() + b"\r\n\r\n")
        expect_status(request(port, large_head, large), 200)
        too_large = (b"POST /v1/query HTTP/1.1\r\nHost: localhost\r\n" + auth
                     + b"Content-Type: application/json\r\nContent-Length: 1048577\r\n\r\n")
        expect_status(request(port, too_large), 413)

        expect_status(request(port, b"GET /" + b"x" * 5000 + b" HTTP/1.1\r\n\r\n"), 431)
        expect_status(request(port, b"GET /v1/health HTTP/1.1\r\nX-Fill: "
                             + b"x" * 20_000 + b"\r\n\r\n"), 431)
        expect_status(request(port, b"GET /v1/health HTTP/1.1\r\n"
                             + b"X-Fill: a\r\n" * 65 + b"\r\n"), 431)
        expect_status(request(port, b"POST /v1/query HTTP/1.1\r\n"
                             + auth + b"Content-Type: application/json\r\n"
                             + b"Content-Length: 1\r\nContent-Length: 2\r\n\r\n"), 400)
        expect_status(request(port, b"POST /v1/query HTTP/1.1\r\n"
                             + auth + b"Content-Type: application/json\r\n"
                             + b"Content-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n"), 400)

        slow = connect(port)
        slow.sendall(b"GET /v1/health HTTP/1.1\r\nX-Slow: ")
        start = time.monotonic()
        expect_status(response(slow), 408)
        elapsed = time.monotonic() - start
        assert 4.0 <= elapsed <= 7.5, elapsed
        slow.close()

        trickle = connect(port)
        trickle.sendall(b"GET /v1/health HTTP/1.1\r\nX-Trickle: ")
        start = time.monotonic()
        for _ in range(4):
            time.sleep(1)
            trickle.sendall(b"x")
        expect_status(response(trickle), 408)
        assert time.monotonic() - start <= 7.5
        trickle.close()

        # Exercise the per-connection byte bound while workers are available.
        before_long = metrics(process.pid)
        long_header = connect(port)
        long_header.sendall(b"GET /v1/health HTTP/1.1\r\nX-Fill: ")
        try:
            long_header.sendall(b"x" * (4 * 1024 * 1024))
        except OSError:
            pass  # The bounded parser is allowed to close as soon as it reaches its cap.
        long_header.close()
        filled = metrics(process.pid)
        assert filled["rss_kib"] <= before_long["rss_kib"] + 2048, (before_long, filled)

        # Match A7's many unfinished unauthenticated connections separately.
        for _ in range(96):
            stream = connect(port)
            try:
                stream.sendall(b"GET /v1/health HTTP/1.1\r\nX-Hold: ")
            except OSError:
                stream.close()
                continue
            held.append(stream)
        time.sleep(0.5)
        pressure = metrics(process.pid)
        assert pressure["threads"] <= idle["threads"] + 2, (idle, pressure)
        assert pressure["fds"] <= idle["fds"] + 66, (idle, pressure)
        assert pressure["rss_kib"] <= idle["rss_kib"] + 8192, (idle, pressure)

        statuses = []
        for stream in held:
            try:
                statuses.append(response(stream))
            except OSError:
                pass
            stream.close()
        held.clear()
        overload_responses = sum(b"503 Service Unavailable" in status for status in statuses)
        assert overload_responses > 0, statuses
        time.sleep(0.3)
        cleaned = metrics(process.pid)
        assert cleaned["fds"] <= idle["fds"] + 4, (idle, cleaned)
        expect_status(request(port, b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n"), 200)
        expect_status(request(port, head, query), 200)
        print(json.dumps({"idle": idle, "before_long": before_long,
                          "pressure": pressure, "filled": filled,
                          "cleaned": cleaned, "overload_responses": overload_responses}, sort_keys=True))
    finally:
        for stream in held:
            stream.close()
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)
        process.stderr.close()


if __name__ == "__main__":
    if sys.platform != "linux" or len(sys.argv) != 2:
        raise SystemExit("usage on Linux: verify_http_ingress.py <picovolt-server binary>")
    run(sys.argv[1])
