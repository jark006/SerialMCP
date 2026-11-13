"""Opt-in UART file-size matrix on an already logged-in POSIX shell.

Requires executable lrz/lsz in PATH. Uses a private service and a unique /tmp
directory, never changes system tool permissions or retries a failed transfer.
Close the normal service's port before running. Python standard library only.
"""
import argparse
import datetime
import hashlib
import itertools
import json
import pathlib
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.stdout.reconfigure(encoding="utf-8")


def quote(value):
    return "'" + value.replace("'", "'\\''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--exe", default="target/release/serial-mcp.exe")
    parser.add_argument("--report", type=pathlib.Path, default=pathlib.Path("target/linux-matrix-report.json"))
    args = parser.parse_args()
    executable = pathlib.Path(args.exe).resolve()
    version = subprocess.check_output([str(executable), "--version"], text=True).strip()
    exe_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = sock.getsockname()[1]
    ids, checks, cases = itertools.count(1), [], []
    remote = None
    identity, failure, cleanup_error = "", None, None
    cleanup = False

    def call(name, arguments=None):
        arguments = {"port": args.port, **(arguments or {})}
        if name == "serial_status":
            arguments.pop("port", None)
        request = urllib.request.Request(
            f"http://127.0.0.1:{address}/mcp",
            json.dumps({"jsonrpc": "2.0", "id": next(ids), "method": "tools/call", "params": {"name": name, "arguments": arguments}}).encode(),
            headers={"Content-Type": "application/json", "Accept": "application/json, text/event-stream"},
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            message = json.load(response)
        assert "error" not in message, message
        result = message["result"]
        assert not result["isError"], result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])

    def execute(command):
        value = call("serial_exec", {"command": command, "timeout_ms": 5000})
        assert value.get("completed") and value.get("exit_code") == 0, value
        return value["data"]

    def check(name, passed):
        assert passed, name
        checks.append(name)
        print("PASS " + name, flush=True)

    with tempfile.TemporaryDirectory(prefix="serial-mcp-matrix-") as local, tempfile.TemporaryFile() as log:
        process = subprocess.Popen(
            [str(executable), "--bind", f"127.0.0.1:{address}"],
            stdout=subprocess.DEVNULL, stderr=log,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        try:
            deadline = time.monotonic() + 10
            while True:
                assert process.poll() is None, "test service exited"
                try:
                    call("serial_status")
                    break
                except OSError:
                    assert time.monotonic() < deadline
                    time.sleep(0.02)
            call("serial_open", {"profile": "terminal"})
            identity = execute("id; uname -a")
            check("logged-in Linux shell", "Linux" in identity)
            tools = call("serial_check_modem_tools")["tools"]
            check("system lrz/lsz executable", tools["lrz"] and tools["lsz"])
            candidate = execute("mktemp -d /tmp/serial-mcp-matrix.XXXXXX").strip()
            assert re.fullmatch(r"/tmp/serial-mcp-matrix\.[A-Za-z0-9]+", candidate), candidate
            remote = candidate
            print("REMOTE " + remote, flush=True)
            for protocol in ["xmodem", "xmodem-1k", "ymodem", "zmodem"]:
                for size in [0, 1, 127, 128, 129, 1023, 1024, 1025, 32769, 65537]:
                    seed = f"SerialMCP matrix {protocol} {size}".encode()
                    payload = (bytes(range(256)) + hashlib.shake_256(seed).digest(size))[:size]
                    if payload:
                        payload = payload[:-1] + b"\x1a"
                    source = pathlib.Path(local) / f"{protocol}-{size}.source"
                    destination = pathlib.Path(local) / f"{protocol}-{size}.received"
                    source.write_bytes(payload)
                    remote_path = f"{remote}/{protocol}-{size}.bin"
                    case = {"protocol": protocol, "source_bytes": size, "source_sha256": hashlib.sha256(payload).hexdigest(), "verified": False}
                    cases.append(case)
                    started = time.monotonic()
                    send_args = {"protocol": protocol, "local_path": str(source), "remote_path": remote_path, "timeout_ms": 2000}
                    if protocol in ("ymodem", "zmodem"):
                        # Batch protocols choose the destination from metadata;
                        # ZMODEM sends a basename, so select its directory explicitly.
                        receiver_args = "--ymodem -y" if protocol == "ymodem" else "-y"
                        send_args.update(remote_path=f"{protocol}-{size}.bin", command=f"cd {quote(remote)} && lrz {receiver_args}", shell_command=True)
                    sent = call("serial_send_file", send_args)
                    check(f"{protocol} {size} upload completed", sent["shell_ready"] and sent["remote_exit_code"] == 0 and sent["bytes_transferred"] == size)
                    received = call("serial_receive_file", {"protocol": protocol, "local_path": str(destination), "remote_path": remote_path, "timeout_ms": 2000})
                    actual = destination.read_bytes()
                    if protocol.startswith("xmodem"):
                        block = 1024 if protocol == "xmodem-1k" else 128
                        matches = payload == actual[:size] and size <= len(actual) <= ((size + block - 1) // block) * block and all(value == 0x1a for value in actual[size:])
                    else:
                        matches = actual == payload
                    check(f"{protocol} {size} downloaded bytes match", matches and received["shell_ready"] and received["remote_exit_code"] == 0 and received["bytes_transferred"] == len(actual))
                    check(f"{protocol} {size} next shell command", execute("command printf matrix-ready") == "matrix-ready")
                    case.update(received_bytes=len(actual), received_sha256=hashlib.sha256(actual).hexdigest(), seconds=round(time.monotonic()-started, 3), verified=True)
        except Exception as error:
            failure = f"{type(error).__name__}: {error}"
        finally:
            try:
                if remote:
                    assert process.poll() is None, "test service exited before remote cleanup"
                    execute(f"rm -rf -- {quote(remote)} && test ! -e {quote(remote)}")
                cleanup = True
            except Exception as error:
                cleanup_error = f"{type(error).__name__}: {error}"
            finally:
                if process.poll() is None:
                    try:
                        call("serial_close")
                    except Exception as error:
                        cleanup_error = cleanup_error or f"{type(error).__name__}: {error}"
                    finally:
                        process.terminate()
                process.wait(timeout=5)
                log.seek(0)
                print(log.read().decode(errors="replace"), flush=True)
    if cleanup and cleanup_error is None:
        check("remote temporary files cleaned", True)
    report = {
        "tested_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "version": version, "exe": str(executable), "exe_sha256": exe_hash,
        "port": args.port, "identity": identity, "remote_directory": remote,
        "checks": checks, "passed": len(checks), "cases": cases,
        "cleanup": cleanup and cleanup_error is None,
        "error": failure, "cleanup_error": cleanup_error,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"version": version, "passed": len(checks), "cases": len(cases), "cleanup": report["cleanup"], "error": failure, "cleanup_error": cleanup_error, "report": str(args.report)}, ensure_ascii=False))
    if failure or not report["cleanup"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
