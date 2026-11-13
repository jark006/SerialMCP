"""Opt-in integration test on an already logged-in POSIX UART shell.

Requires lrz/lsz at --modem-source (files may lack execute permission).
Creates executable copies and test data only in a unique /tmp directory, then
removes that directory and restores PATH/cwd. Does not log in or store credentials.
Close the port in any existing MCP service before running this script.
"""
import argparse
import concurrent.futures
import datetime
import hashlib
import itertools
import json
import pathlib
import re
import socket
import subprocess
import tempfile
import time
import urllib.request


def quote(value):
    return "'" + value.replace("'", "'\\''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("--exe", default="target/release/serial-mcp.exe")
    parser.add_argument("--modem-source", default="/system/bin")
    parser.add_argument("--use-system-tools", action="store_true", help="Use executable lrz/lsz in modem-source directly; do not create copies")
    parser.add_argument("--report", type=pathlib.Path, default=pathlib.Path("target/linux-board-report.json"))
    args = parser.parse_args()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = sock.getsockname()[1]
    url = f"http://127.0.0.1:{address}/mcp"
    ids = itertools.count(1)
    checks, transfers = [], []
    remote = None
    environment_changed = False
    cleanup_passed = False
    def rpc(method, params=None):
        request = urllib.request.Request(url, json.dumps({"jsonrpc":"2.0", "id":next(ids), "method":method, "params":params or {}}).encode(),
            headers={"Content-Type":"application/json", "Accept":"application/json, text/event-stream"})
        with urllib.request.urlopen(request, timeout=45) as response:
            return json.load(response)
    def call(name, arguments=None, expect_error=False):
        arguments = {"port":args.port, **(arguments or {})}
        if name in ("serial_status", "serial_list_ports", "serial_get_output"):
            arguments.pop("port", None)
        response = rpc("tools/call", {"name":name,"arguments":arguments})
        assert "error" not in response, response
        result = response["result"]
        assert result["isError"] == expect_error, result
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])
    def execute(command, **kwargs):
        result = call("serial_exec", {"command":command, "timeout_ms":5000, **kwargs})
        assert result.get("completed") and result.get("exit_code") == 0, result
        return result
    def check(name, value):
        assert value, name
        checks.append(name)
        print("PASS " + name, flush=True)
    with tempfile.TemporaryDirectory(prefix="serial-mcp-board-") as local, tempfile.TemporaryFile() as log:
        process = subprocess.Popen([str(pathlib.Path(args.exe).resolve()), "--bind", f"127.0.0.1:{address}"], stdout=subprocess.DEVNULL, stderr=log,
            creationflags=getattr(subprocess,"CREATE_NO_WINDOW",0))
        try:
            deadline = time.monotonic() + 10
            while True:
                assert process.poll() is None, "test service exited"
                try:
                    rpc("ping")
                    break
                except OSError:
                    assert time.monotonic() < deadline
                    time.sleep(0.02)
            call("serial_open", {"profile":"terminal"})
            identity = execute("id; uname -a")["data"]
            check("logged-in Linux shell completion", "Linux" in identity)
            initial_probe = call("serial_check_modem_tools")["tools"]
            permissions = execute(f"for t in lrz lsz; do p=$(command -v \"$t\"); [ -f \"$p\" ] && [ -x \"$p\" ] && printf '%s=1\\n' \"$t\" || printf '%s=0\\n' \"$t\"; done")["data"]
            check("probe agrees with executable permissions", all((f"{name}=1" in permissions) == initial_probe[name] for name in ("lrz","lsz")))
            if not initial_probe["lrz"]:
                fixture = pathlib.Path(local)/"startup-failure.bin"
                fixture.write_bytes(b"x")
                failed = call("serial_send_file", {"local_path":str(fixture), "command":quote(args.modem_source+'/lrz')+' -y', "shell_command":True, "timeout_ms":500}, expect_error=True)
                assert "remote file command exited with code " in failed["error"] and "Permission denied" in failed["error"], failed
                state = call("serial_status")
                assert any(port["port"] == args.port and port["state"] == "open" for port in state["ports"]), state
                check("remote startup failure retains exit code and permission detail", True)
            remote = execute("mktemp -d /tmp/serial-mcp-board.XXXXXX")["data"].strip()
            assert re.fullmatch(r"/tmp/serial-mcp-board\.[A-Za-z0-9]+", remote), remote
            print("REMOTE " + remote, flush=True)
            if args.use_system_tools:
                execute(f"test -x {quote(args.modem_source+'/lrz')} && test -x {quote(args.modem_source+'/lsz')}")
                tool_dir = args.modem_source
            else:
                execute(f"cp {quote(args.modem_source+'/lrz')} {quote(args.modem_source+'/lsz')} {quote(remote)}/ && chmod 700 {quote(remote)}/lrz {quote(remote)}/lsz")
                tool_dir = remote
            call("serial_run_command", {"command":f"SMCP_TEST_PATH=$PATH; SMCP_TEST_CWD=$PWD; export PATH={quote(tool_dir)}:$PATH; cd {quote(remote)}", "idle_timeout_ms":100})
            environment_changed = True
            tools = call("serial_check_modem_tools")["tools"]
            check("executable system tools detected" if args.use_system_tools else "executable copies detected", tools["lrz"] and tools["lsz"])
            versions = execute("lrz --version; lsz --version")["data"]
            value = call("serial_exec", {"command":"printf '%s\\n' \"it's 中文\"; printf no-newline; exit 7"})
            check("quotes UTF-8 no-final-newline and exit 7", value["completed"] and value["exit_code"] == 7 and value["data"] == "it's 中文\r\nno-newline")
            check("quiet gap does not end shell command", execute("printf before; sleep 1; printf after")["data"] == "beforeafter")
            value = execute("i=0; while [ $i -lt 180 ]; do printf 'L%03d 中文 UART ABCDEFGHIJKLMNOPQRSTUVWXYZ\\n' $i; i=$((i+1)); done", output_limit=128)
            check("long output marked truncated", value.get("truncated"))
            text, offset = "", 0
            while True:
                page = call("serial_get_output", {"output_id":value["output_id"],"offset":offset,"limit":257})
                text += page["data"]
                offset = page["next_offset"]
                if not page["has_more"]:
                    break
            check("UTF-8 paged output exactly 180 lines", text == "".join(f"L{i:03} 中文 UART ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n" for i in range(180)))
            timed = call("serial_exec", {"command":"printf timeout-start; sleep 1; printf timeout-end", "timeout_ms":150})
            check("timeout reports remote may still run", not timed["completed"] and timed["remote_may_be_running"] and timed["stop_reason"] == "timeout")
            tail = call("serial_read", {"timeout_ms":3000, "idle_timeout_ms":150})
            check("later output survives timeout", "timeout-end" in tail["data"])
            with concurrent.futures.ThreadPoolExecutor() as pool:
                future = pool.submit(call, "serial_exec", {"command":"printf cancel-start; sleep 2; printf cancel-end", "timeout_ms":25000})
                deadline = time.monotonic() + 3
                while True:
                    state = call("serial_status")
                    if any(p.get("busy") for p in state["ports"]):
                        break
                    assert time.monotonic() < deadline
                    time.sleep(0.01)
                check("ping remains responsive", rpc("ping")["result"] == {})
                busy = call("serial_write", {"data":"printf unexpected\n"}, expect_error=True)
                check("busy write rejected", "busy" in busy["error"])
                call("serial_cancel")
                value = future.result(timeout=3)
                check("cancel returns partial data without shell completion", value["stop_reason"] == "cancelled" and not value["completed"])
            call("serial_send_key", {"key":"ctrl_c"})
            call("serial_read", {"timeout_ms":2000,"idle_timeout_ms":150})
            check("shell recovers after cancelled wait and control key", execute("printf recovered")["data"] == "recovered")
            payload = bytes(range(256))*16+b"\x00\x1b\xff"
            source = pathlib.Path(local)/"pattern.bin"
            source.write_bytes(payload)
            for protocol in ("zmodem","ymodem","xmodem","xmodem-1k"):
                remote_path = f"{remote}/{protocol}.bin"
                sent = call("serial_send_file", {"protocol":protocol,"local_path":str(source),"remote_path":remote_path,"timeout_ms":2000})
                check(f"{protocol} upload waits for shell exit", sent["shell_ready"] and sent["remote_exit_code"] == 0)
                # Intentionally receive immediately: no prompt read or delay.
                received_path = pathlib.Path(local)/f"{protocol}.received"
                received = call("serial_receive_file", {"protocol":protocol,"remote_path":remote_path,"local_path":str(received_path),"timeout_ms":2000})
                actual = received_path.read_bytes()
                if protocol.startswith("xmodem"):
                    matched = actual[:len(payload)] == payload and all(byte == 0x1a for byte in actual[len(payload):])
                else:
                    matched = actual == payload
                check(f"{protocol} round trip byte exact (XMODEM padding allowed)", matched)
                check(f"{protocol} immediate next shell command", execute("printf transfer-ready")["data"] == "transfer-ready")
                if not protocol.startswith("xmodem"):
                    md5 = execute(f"md5sum {quote(remote_path)}")["data"].split()[0]
                    check(f"{protocol} board MD5 agrees", md5 == hashlib.md5(payload).hexdigest())
                transfers.append({"protocol":protocol,"sent_bytes":len(payload),"received_bytes":len(actual),"sha256":hashlib.sha256(actual).hexdigest(),"shell_ready":received["shell_ready"],"verified":True})
            execute("cd /; printf child-directory");
            check("subshell cwd does not change session", execute("pwd")["data"].strip() == remote)
            call("serial_reconnect")
            check("reconnect resumes logged-in shell", execute("printf reconnect-ok")["data"] == "reconnect-ok")
        finally:
            if process.poll() is None:
                try:
                    if environment_changed:
                        call("serial_run_command", {"command":"PATH=$SMCP_TEST_PATH; export PATH; cd \"$SMCP_TEST_CWD\"; unset SMCP_TEST_PATH SMCP_TEST_CWD", "idle_timeout_ms":100})
                    if remote:
                        execute(f"rm -rf -- {quote(remote)}; test ! -e {quote(remote)}")
                    cleanup_passed = True
                except Exception as error:
                    print("CLEANUP " + str(error), flush=True)
                finally:
                    try:
                        call("serial_close")
                    except Exception as error:
                        print("CLOSE " + str(error), flush=True)
                    finally:
                        process.terminate()
            process.wait(timeout=5)
            log.seek(0)
            print(log.read().decode(errors="replace"), flush=True)
        check("temporary files and terminal environment restored", cleanup_passed)
        version = subprocess.check_output([str(pathlib.Path(args.exe).resolve()), "--version"], text=True).strip()
        report = {"tested_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),"version":version,"port":args.port,"identity":identity,"modem_versions":versions,"checks":checks,"passed":len(checks),"transfers":transfers,"cleanup":cleanup_passed,"exe":str(pathlib.Path(args.exe).resolve()),"exe_sha256":hashlib.sha256(pathlib.Path(args.exe).read_bytes()).hexdigest()}
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
        print(json.dumps(report,ensure_ascii=False,indent=2))


if __name__ == "__main__":
    main()
