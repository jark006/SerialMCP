"""Opt-in UART tests for long shell input, missing files and peer interruption.

Requires an already logged-in POSIX shell with executable lrz/lsz and timeout.
Uses a private service; close the port in the normal service first. All remote
test files live in a unique /tmp directory. Never changes system tool permissions.
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
    parser.add_argument("--report", type=pathlib.Path, default=pathlib.Path("target/linux-recovery-report.json"))
    args = parser.parse_args()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = sock.getsockname()[1]
    ids = itertools.count(1)
    checks = []
    remote = None
    cleanup = False
    def call(name, arguments=None, error=False):
        arguments = {"port":args.port, **(arguments or {})}
        if name in ("serial_status", "serial_get_output"):
            arguments.pop("port", None)
        request = urllib.request.Request(f"http://127.0.0.1:{address}/mcp", json.dumps({"jsonrpc":"2.0", "id":next(ids), "method":"tools/call", "params":{"name":name,"arguments":arguments}}).encode(),
            headers={"Content-Type":"application/json", "Accept":"application/json, text/event-stream"})
        with urllib.request.urlopen(request, timeout=60) as response:
            message = json.load(response)
        assert "error" not in message, message
        result = message["result"]
        assert result["isError"] == error, result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])
    def execute(command, **kwargs):
        result = call("serial_exec", {"command":command,"timeout_ms":5000, **kwargs})
        if not result.get("completed") and result.get("output_id"):
            first = call("serial_get_output", {"output_id":result["output_id"],"field":"raw_data","limit":4})
            tail = call("serial_get_output", {"output_id":result["output_id"],"field":"raw_data","offset":max(0,first["total_bytes"]-1024),"limit":1024})
            print("INCOMPLETE TAIL " + repr(tail["data"]), flush=True)
        assert result.get("completed") and result.get("exit_code") == 0, result
        return result
    def full_text(result):
        if not result.get("truncated"):
            return result["data"]
        text, offset = "", 0
        while True:
            page = call("serial_get_output", {"output_id":result["output_id"],"offset":offset,"limit":4096})
            text += page["data"]
            offset = page["next_offset"]
            if not page["has_more"]:
                return text
    def check(name, value, **evidence):
        assert value, (name, evidence)
        checks.append({"name":name, "passed":True, **evidence})
        print("PASS " + name, flush=True)
    with tempfile.TemporaryDirectory(prefix="serial-mcp-recovery-") as local, tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(pathlib.Path(args.exe).resolve()),"--bind",f"127.0.0.1:{address}"], stdout=subprocess.DEVNULL,stderr=log,
            creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
        try:
            deadline = time.monotonic()+10
            while True:
                assert process.poll() is None
                try:
                    call("serial_status")
                    break
                except OSError:
                    assert time.monotonic() < deadline
                    time.sleep(0.02)
            call("serial_open", {"profile":"terminal"})
            identity = execute("id; uname -a")["data"]
            tools = call("serial_check_modem_tools")["tools"]
            check("system lrz/lsz executable", tools["lrz"] and tools["lsz"])
            for length in [900,1800,4096,16000,64000]:
                value = "A"*length
                result = execute("v="+quote(value)+"; printf '%s' \"$v\"", timeout_ms=30000, max_bytes=262144, output_limit=128)
                check(f"long command {length} bytes", full_text(result)==value, bytes=length)
            value = "中文 ' backslash \\" * 150
            result = execute("printf '%s' "+quote(value), timeout_ms=10000, output_limit=128)
            check("long UTF-8 quoted command", full_text(result)==value, bytes=len(value.encode()))
            remote = execute("mktemp -d /tmp/serial-mcp-recovery.XXXXXX")["data"].strip()
            assert re.fullmatch(r"/tmp/serial-mcp-recovery\.[A-Za-z0-9]+",remote),remote
            print("REMOTE " + remote, flush=True)
            for protocol in ["zmodem","ymodem","xmodem","xmodem-1k"]:
                destination = pathlib.Path(local)/(protocol+"-missing.bin")
                started = time.monotonic()
                failure = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"remote_path":remote+"/missing.bin","timeout_ms":200}, error=True)
                elapsed = time.monotonic()-started
                state = call("serial_status")
                check(f"{protocol} missing file returns and stays open", elapsed<8 and not destination.exists() and any(p["port"]==args.port and p["state"]=="open" and not p.get("busy") for p in state["ports"]), seconds=round(elapsed,3), error=failure["error"])
                check(f"{protocol} shell usable after missing file", execute("printf missing-recovered")["data"]=="missing-recovered")
            data = b"UART-interruption-test-0123456789"*2048+b"x"
            source = pathlib.Path(local)/"pattern.bin"
            source.write_bytes(data)
            # BusyBox timeout syntax varies by version. Determine it from help.
            help_text = call("serial_exec", {"command":"timeout --help 2>&1", "timeout_ms":2000})["data"]
            timeout_args = "-t 1" if "-t" in help_text else "1"
            failure = call("serial_send_file", {"local_path":str(source),"remote_path":"partial.bin","command":f"cd {quote(remote)} && timeout {timeout_args} lrz -y","shell_command":True,"timeout_ms":500}, error=True)
            call("serial_send_key", {"key":"ctrl_c"})
            call("serial_read", {"timeout_ms":2000,"idle_timeout_ms":150})
            partial = execute(f"test -f {quote(remote)}/partial.bin && wc -c < {quote(remote)}/partial.bin")["data"].strip()
            check("peer interruption releases port and shell", execute("printf interrupt-recovered")["data"]=="interrupt-recovered" and 0<int(partial)<len(data), received_bytes=int(partial), error=failure["error"])
            # A fresh command after the failed transfer is an explicit new operation.
            small = data[:1025]
            source.write_bytes(small)
            for protocol in ["xmodem","xmodem-1k","ymodem","zmodem"]:
                filename = f"{remote}/manual-{protocol}.bin"
                receive_args = f"-X -c -y {quote(filename)}" if protocol.startswith("xmodem") else "--ymodem -y" if protocol=="ymodem" else "-y"
                call("serial_write", {"data":f"( cd {quote(remote)} && lrz {receive_args} )", "line_ending":"lf"})
                # Match terminal echo newline; any handshake bytes read beyond it stay buffered.
                call("serial_read", {"until":"sequence","terminator":"*\x18B" if protocol=="zmodem" else "\r\n","timeout_ms":3000})
                sent = call("serial_send_file", {"protocol":protocol,"local_path":str(source),"remote_path":f"manual-{protocol}.bin","start_command":False,"timeout_ms":2000})
                call("serial_read", {"until":"sequence","terminator":"]# ","timeout_ms":3000})
                send_args = "-X -k" if protocol=="xmodem-1k" else "-X" if protocol=="xmodem" else "--ymodem" if protocol=="ymodem" else ""
                call("serial_write", {"data":f"lsz {send_args} {quote(filename)}", "line_ending":"lf"})
                call("serial_read", {"until":"sequence","terminator":"\r\n","timeout_ms":2000})
                destination = pathlib.Path(local)/(protocol+"-manual.bin")
                received = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"start_command":False,"timeout_ms":2000})
                call("serial_read", {"until":"sequence","terminator":"]# ","timeout_ms":3000})
                actual = destination.read_bytes()
                check(f"{protocol} manual handshake round trip", actual[:len(small)]==small and all(b==0x1a for b in actual[len(small):]) and not sent["shell_ready"] and not received["shell_ready"], received_bytes=len(actual))
        finally:
            try:
                if remote and process.poll() is None:
                    execute(f"rm -rf -- {quote(remote)} && test ! -e {quote(remote)}")
                cleanup = True
            finally:
                if process.poll() is None:
                    try:
                        call("serial_close")
                    finally:
                        process.terminate()
                process.wait(timeout=5)
                log.seek(0)
                print(log.read().decode(errors="replace"),flush=True)
        check("remote temporary files cleaned",cleanup)
        report = {"tested_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),"identity":identity,"version":subprocess.check_output([str(pathlib.Path(args.exe).resolve()),"--version"],text=True).strip(),"exe":str(pathlib.Path(args.exe).resolve()),"exe_sha256":hashlib.sha256(pathlib.Path(args.exe).read_bytes()).hexdigest(),"checks":checks,"passed":len(checks),"cleanup":cleanup}
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
        print(json.dumps(report,ensure_ascii=False,indent=2))


if __name__ == "__main__":
    main()
