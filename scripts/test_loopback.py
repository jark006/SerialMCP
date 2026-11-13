"""Opt-in real UART test. Run ONLY on a port whose TX and RX are connected.

Python 3 standard library only. Starts a private server, writes bytes/control
characters to --port, closes the port, then terminates its own server process.
"""
import argparse
import concurrent.futures
import hashlib
import itertools
import json
import pathlib
import socket
import subprocess
import tempfile
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--exe", default="target/release/serial-mcp.exe")
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()
    port = args.port.strip().upper()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = sock.getsockname()[1]
    url = f"http://127.0.0.1:{address}/mcp"
    ids = itertools.count(1)
    checks = []
    def rpc(method, params=None, notification=False):
        message = {"jsonrpc": "2.0", "method": method}
        if not notification:
            message["id"] = next(ids)
        if params is not None:
            message["params"] = params
        request = urllib.request.Request(url, json.dumps(message).encode(), headers={
            "Content-Type": "application/json", "Accept": "application/json, text/event-stream"})
        with urllib.request.urlopen(request, timeout=15) as response:
            body = response.read()
        return json.loads(body) if body else None
    def call(name, arguments=None, error=False):
        result = rpc("tools/call", {"name": name, "arguments": arguments or {}})
        assert "error" not in result, result
        result = result["result"]
        assert result["isError"] == error, result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])
    def check(name, condition):
        assert condition, name
        checks.append(name)
        print(f"PASS {name}", flush=True)
    with tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(pathlib.Path(args.exe).resolve()), "--bind", f"127.0.0.1:{address}"],
            stdout=subprocess.DEVNULL, stderr=log,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            deadline = time.monotonic() + 10
            while True:
                assert process.poll() is None, "server exited during startup"
                try:
                    rpc("ping")
                    break
                except (OSError, TimeoutError):
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.02)
            devices = call("serial_list_ports")["ports"]
            check("port enumerated", any(p["port"].upper() == port for p in devices))
            opened = call("serial_open", {"port": port, "profile": "binary"})
            check("binary profile opened", opened["profile"] == "binary")
            call("serial_flush", {"direction": "input"})
            payload = bytes(range(256)) * 4
            response = call("serial_exchange", {"data": payload.hex(), "max_bytes": len(payload), "timeout_ms": 3000})
            check("all 256 byte values round trip exactly", bytes.fromhex(response["data"]) == payload and response["bytes_written"] == len(payload))
            check("exact frame length stops read", response["stop_reason"] == "max_bytes")
            response = call("serial_exchange", {"data": "AA 00 BB 0D 0A 80 42", "terminator": "0D 0A", "timeout_ms": 2000})
            check("hex delimiter includes only first frame", response["data"] == "AA 00 BB 0D 0A" and response["matched"])
            check("suffix survives next read", call("serial_read", {"max_bytes": 2})["data"] == "80 42")
            call("serial_configure", {"profile": "terminal"})
            text = "中文 UART \x1b[31mRED\x1b[0m"
            response = call("serial_run_command", {"command": text, "line_ending": "none", "idle_timeout_ms": 80, "timeout_ms": 1000})
            check("terminal UTF-8 and ANSI cleanup", response["data"] == "中文 UART RED" and response["stop_reason"] == "idle")
            for key, expected in [("ctrl_c", "03"), ("up", "1B 5B 41"), ("enter", "0D")]:
                call("serial_send_key", {"key": key})
                response = call("serial_read", {"encoding": "hex", "max_bytes": len(bytes.fromhex(expected))})
                check(f"terminal key {key}", response["data"] == expected)
            response = call("serial_exec", {"command": "printf hello", "timeout_ms": 300, "response_mode": "detailed"})
            check("echoed shell wrapper cannot fake completion", response["completed"] is False and response["exit_code"] is None and response["stop_reason"] == "timeout")
            probe = call("serial_check_modem_tools", {"timeout_ms": 200}, error=True)
            check("echoed probe cannot report tool availability", "completion marker" in probe["error"])
            text = "0123456789abcdef" * 64
            response = call("serial_exchange", {"data": text, "max_bytes": len(text), "output_limit": 128, "timeout_ms": 3000})
            check("compact result is bounded", response["truncated"] and len(response["data"]) == 128)
            recovered, offset = "", 0
            while True:
                page = call("serial_get_output", {"output_id": response["output_id"], "offset": offset, "limit": 1000, "output_limit": 128})
                recovered += page["data"]
                offset = page["next_offset"]
                if not page["has_more"]:
                    break
            check("paged output is lossless with smaller output_limit", recovered == text)
            call("serial_configure", {"profile": "binary"})
            result = rpc("tools/call", {"name": "serial_write", "arguments": {"data": "GG", "encoding": "hex"}})
            check("invalid hex is rejected before write", result["error"]["code"] == -32602)
            result = rpc("tools/call", {"name": "serial_exec", "arguments": {"command": "echo unsafe"}})
            check("binary profile rejects implicit shell traffic", result["result"]["isError"])
            with concurrent.futures.ThreadPoolExecutor() as executor:
                future = executor.submit(call, "serial_read", {"timeout_ms": 10000})
                deadline = time.monotonic() + 3
                while True:
                    state = call("serial_status")
                    if any(p.get("busy") for p in state["ports"]):
                        break
                    assert time.monotonic() < deadline, "read never became busy"
                    time.sleep(0.01)
                started = time.monotonic()
                check("ping responds during long read", rpc("ping")["result"] == {})
                busy = rpc("tools/call", {"name": "serial_write", "arguments": {"data": "FF"}})
                check("same port rejects write immediately", busy["result"]["isError"] and "busy" in busy["result"]["content"][0]["text"])
                other = rpc("tools/call", {"name": "serial_read", "arguments": {"port": "COM9999", "timeout_ms": 50}})
                check("other port request is independent", other["result"]["isError"] and "busy" not in other["result"]["content"][0]["text"])
                cancelled = call("serial_cancel")
                response = future.result(timeout=3)
                check("cancel releases read and preserves active selection", cancelled["cancel_requested"] and response["stop_reason"] == "cancelled" and call("serial_status")["active_port"] == port)
                check("control operations did not wait for overall timeout", time.monotonic() - started < 3)
            check("busy-rejected request wrote no bytes", call("serial_read", {"timeout_ms": 80})["bytes_read"] == 0)
            reconnect = call("serial_reconnect")
            check("reconnect preserves binary profile", reconnect["profile"] == "binary")
            check("post-reconnect transfer", call("serial_exchange", {"data": "00 FF 03 1B", "max_bytes": 4})["data"] == "00 FF 03 1B")
            call("serial_close")
            check("closing active port clears selection", "active_port" not in call("serial_status"))
            report = {"port": port, "checks": checks, "passed": len(checks), "binary_sha256": hashlib.sha256(payload).hexdigest(), "exe": str(pathlib.Path(args.exe).resolve())}
            if args.report:
                args.report.parent.mkdir(parents=True, exist_ok=True)
                args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
            print(json.dumps(report, ensure_ascii=False, indent=2))
        finally:
            if process.poll() is None:
                try:
                    call("serial_close", {"port": port})
                except Exception:
                    pass
                process.terminate()
            process.wait(timeout=5)
            log.seek(0)
            print(log.read().decode(errors="replace"))


if __name__ == "__main__":
    main()
