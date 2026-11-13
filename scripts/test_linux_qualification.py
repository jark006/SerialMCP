"""Opt-in UART negative-case and output-cache tests on a logged-in POSIX shell.

Requires executable lrz/lsz in PATH. Uses a private HTTP service and unique /tmp
directory, does not change system tool permissions. Close the normal service's
port before running. Python standard library only; no credentials are stored.
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
    parser.add_argument("--report", type=pathlib.Path, default=pathlib.Path("target/linux-qualification-report.json"))
    args = parser.parse_args()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        address = sock.getsockname()[1]
    ids, checks = itertools.count(1), []
    remote = None
    cleanup = False

    def call(name, arguments=None, error=False, envelope=False):
        arguments = {"port":args.port, **(arguments or {})}
        if name in ("serial_status", "serial_get_output"):
            arguments.pop("port", None)
        request = urllib.request.Request(f"http://127.0.0.1:{address}/mcp", json.dumps({"jsonrpc":"2.0", "id":next(ids), "method":"tools/call", "params":{"name":name,"arguments":arguments}}).encode(),
            headers={"Content-Type":"application/json", "Accept":"application/json, text/event-stream"})
        with urllib.request.urlopen(request, timeout=30) as response:
            message = json.load(response)
        assert "error" not in message, message
        result = message["result"]
        assert result["isError"] == error, result
        return result if envelope else result.get("structuredContent") or json.loads(result["content"][0]["text"])

    def execute(command, **kwargs):
        result = call("serial_exec", {"command":command,"timeout_ms":3000, **kwargs})
        assert result.get("completed") and result.get("exit_code") == 0, result
        return result

    def check(name, passed, **evidence):
        assert passed, (name, evidence)
        checks.append({"name":name,"passed":True, **evidence})
        print("PASS " + name, flush=True)

    def page_all(output_id, field="data", limit=64, output_limit=None, response_mode="compact"):
        text, offset = "", 0
        while True:
            page = call("serial_get_output", {"output_id":output_id,"field":field,"offset":offset,"limit":limit,"output_limit":limit if output_limit is None else output_limit,"response_mode":response_mode})
            assert page["output_id"]==output_id and not page.get("truncated"),page
            assert page["next_offset"]==offset+len(page["data"].encode()),page
            assert page["next_offset"]>offset or not page["has_more"], page
            text += page["data"]
            offset = page["next_offset"]
            if not page["has_more"]:
                return text

    with tempfile.TemporaryDirectory(prefix="serial-mcp-qualification-") as local, tempfile.TemporaryFile() as log:
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
                    assert time.monotonic()<deadline
                    time.sleep(0.02)
            call("serial_open", {"profile":"terminal"})
            identity = execute("id; uname -a")["data"]
            cwd = execute("pwd")["data"].strip()
            tools = call("serial_check_modem_tools")["tools"]
            check("system modem tools available",tools["lrz"] and tools["lsz"])
            for padding in [0,180]:
                tail = "\n: # " + "A"*padding
                result = execute("read value; code=$?; printf 'read=%s:%s' \"$code\" \"$value\""+tail)
                check(f"exec stdin is EOF with padding {padding}",result["data"]=="read=1:")
                result = execute("printf 'pipe-input\\n' | { read value; printf '%s' \"$value\"; }"+tail)
                check(f"explicit pipeline stdin with padding {padding}",result["data"]=="pipe-input")
                result = execute("read value <<'TEST_DATA'\nredirected-input\nTEST_DATA\nprintf '%s' \"$value\""+tail)
                check(f"explicit heredoc stdin with padding {padding}",result["data"]=="redirected-input")
            remote = execute("mktemp -d /tmp/serial-mcp-qualification.XXXXXX")["data"].strip()
            assert re.fullmatch(r"/tmp/serial-mcp-qualification\.[A-Za-z0-9]+",remote),remote
            print("REMOTE "+remote,flush=True)
            execute(f"printf 'first file\\n' > {quote(remote+'/first.txt')}; printf 'second file\\n' > {quote(remote+'/second.txt')}")
            # Delayed CRC requests are not negative acknowledgements for a packet.
            # Inject enough to exceed the old resend budget before starting lrz.
            noise_payload = b"UART packet response noise fixture - printable payload\n"*24
            noise_source = pathlib.Path(local)/"noise-source.bin"
            noise_source.write_bytes(noise_payload)
            for protocol in ["xmodem","xmodem-1k","ymodem"]:
                filename = remote+f"/noise-{protocol}.bin"
                receive_args = "--ymodem -y" if protocol=="ymodem" else f"-X -c -y {quote(filename)}"
                command = f"cd {quote(remote)}; command printf '{'C'*40}'; lrz {receive_args}"
                sent = call("serial_send_file", {"protocol":protocol,"local_path":str(noise_source),"remote_path":filename,"command":command,"shell_command":True,"timeout_ms":2000})
                check(f"{protocol} upload ignores extra CRC requests",sent["shell_ready"] and sent["remote_exit_code"]==0)
                destination = pathlib.Path(local)/f"noise-{protocol}.received"
                received = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"remote_path":filename,"timeout_ms":2000})
                actual = destination.read_bytes()
                matches = actual==noise_payload if protocol=="ymodem" else actual[:len(noise_payload)]==noise_payload and all(byte==0x1a for byte in actual[len(noise_payload):])
                check(f"{protocol} extra CRC request round trip byte exact",received["shell_ready"] and matches)
                check(f"{protocol} shell ready after noisy handshake",execute("command printf noise-ready")["data"]=="noise-ready")
            # Opt-in environment tests require the default builtin initially so
            # cleanup never discards a user's pre-existing printf definition.
            printf_type = execute("type printf")["data"]
            assert "builtin" in printf_type and "alias" not in printf_type and "function" not in printf_type, "environment test requires default printf builtin"
            call("serial_run_command", {"command":"printf() { command printf 'shadowed-printf\\n'; }","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for padding in [0,180]:
                    check(f"printf function cannot replace markers padding {padding}",execute("echo marker-function-ready\n: # "+"A"*padding)["data"]=="marker-function-ready\r\n")
                destination = pathlib.Path(local)/"function-transfer.bin"
                received = call("serial_receive_file", {"protocol":"zmodem","local_path":str(destination),"remote_path":remote+"/first.txt","timeout_ms":2000})
                check("printf function cannot break file command markers",received["shell_ready"] and destination.read_bytes()==b"first file\n")
            finally:
                call("serial_run_command", {"command":"unset -f printf","timeout_ms":1000,"idle_timeout_ms":100})
            call("serial_run_command", {"command":"alias printf='echo shadowed-printf'","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for padding in [0,180]:
                    check(f"printf alias cannot replace markers padding {padding}",execute("echo marker-alias-ready\n: # "+"A"*padding)["data"]=="marker-alias-ready\r\n")
            finally:
                call("serial_run_command", {"command":"unalias printf","timeout_ms":1000,"idle_timeout_ms":100})
            eval_type = execute("type eval")["data"]
            assert "builtin" in eval_type and "alias" not in eval_type and "function" not in eval_type, "environment test requires default eval builtin"
            call("serial_run_command", {"command":"alias eval='echo shadowed-eval'","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for padding in [0,180]:
                    tail = "\n: # "+"A"*padding
                    result = call("serial_exec", {"command":"command printf eval-ready; exit 7"+tail,"timeout_ms":2000})
                    check(f"eval alias cannot replace executed command padding {padding}",result["completed"] and result["exit_code"]==7 and result["data"]=="eval-ready")
                    destination = pathlib.Path(local)/f"eval-transfer-{padding}.bin"
                    received = call("serial_receive_file", {"protocol":"zmodem","local_path":str(destination),"command":f"lsz {quote(remote+'/first.txt')}"+tail,"shell_command":True,"timeout_ms":2000})
                    check(f"eval alias cannot replace file command padding {padding}",received["shell_ready"] and received["remote_exit_code"]==0 and destination.read_bytes()==b"first file\n")
            finally:
                call("serial_run_command", {"command":"unalias eval","timeout_ms":1000,"idle_timeout_ms":100})
            for padding in [0,180]:
                result = call("serial_exec", {"command":"if\n: # "+"A"*padding,"timeout_ms":2000})
                check(f"eval syntax error completes and shell recovers padding {padding}",result["completed"] and result["exit_code"]!=0 and "syntax error" in result["data"].lower() and execute("command printf syntax-recovered")["data"]=="syntax-recovered")
            original_path = execute("command printf '%s' \"$PATH\"")["data"]
            call("serial_run_command", {"command":"export PATH='/dev/null'","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for padding in [0,180]:
                    result = execute("command printf '%s' \"$PATH\"\n: # "+"A"*padding)
                    check(f"custom PATH survives helper launch padding {padding}",result["data"]=="/dev/null")
            finally:
                call("serial_run_command", {"command":f"export PATH={quote(original_path)}","timeout_ms":1000,"idle_timeout_ms":100})
            env_name = "SMCP_QUAL_ENV_"+remote.rsplit(".",1)[1]
            env_value = "exported 中文 ' space \\"
            call("serial_run_command", {"command":f"export {env_name}={quote(env_value)}","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for padding in [0,180]:
                    result = execute(f"command printf '%s' \"${env_name}\"\n: # "+"A"*padding)
                    check(f"exported environment inherited padding {padding}",result["data"]==env_value)
            finally:
                call("serial_run_command", {"command":f"unset {env_name}","timeout_ms":1000,"idle_timeout_ms":100})
            for name,command,data,code in [
                ("child stdout redirection keeps completion", "exec >/dev/null; echo hidden; exit 7", "",7),
                ("child errexit keeps completion", "set -e; false; echo should-not-print", "",1),
                ("child nounset keeps completion", "set -u; echo \"$SMCP_QUAL_UNSET_"+remote.rsplit(".",1)[1]+"\"", "SMCP_QUAL_UNSET_",None),
                ("stderr and nonzero exit remain readable", "command printf 'stderr-output\\n' >&2; exit 11", "stderr-output\r\n",11)]:
                result = call("serial_exec", {"command":command,"timeout_ms":2000})
                matches = data in result["data"] and result["exit_code"]!=0 if code is None else data==result["data"] and code==result["exit_code"]
                check(name,result["completed"] and matches)
            for name,command,expected in [
                ("ANSI color and backspace", "printf '\\033[31mRED\\033[0m\\tA\\bB\\007\\r\\n'", "RED\tB\r\r\n"),
                ("Unicode backspace", "printf '中文😀\\bZ\\n'", "中文Z\r\n"),
                ("OSC and DCS cleanup", "printf 'before\\033]0;title\\007middle\\033Ppayload\\033\\\\after\\n'", "beforemiddleafter\r\n"),
                ("marker-like text cannot finish command", "printf '\\036OTHER_BEGIN\\037literal\\036OTHER_RC:7\\037\\036OTHER_END\\037\\n'", "OTHER_BEGINliteralOTHER_RC:7OTHER_END\r\n")]:
                result = execute(command,response_mode="detailed",output_limit=4096)
                check(name,result["data"]==expected)
            result = execute("printf 'A\\000B\\377C\\n'",response_mode="detailed",output_limit=4096)
            check("NUL and invalid UTF-8 retained in raw hex", b"A\x00B\xffC\r\n" in bytes.fromhex(result["hex"]))
            # A receive cap must leave every remaining byte available, including
            # completion markers that have not yet been consumed by exec.
            begin, end = b"\x1eCAP_BEGIN\x1f", b"\x1eCAP_END\x1f"
            capped = call("serial_exec", {"command":"command printf '\\036CAP_BEGIN\\037'; command printf '%02048d' 0; command printf '\\036CAP_END\\037'","timeout_ms":3000,"max_bytes":512,"output_limit":16384,"response_mode":"detailed"})
            check("receive cap never claims shell completion",capped["stop_reason"]=="max_bytes" and capped["bytes_read"]==512 and not capped["completed"] and capped["remote_may_be_running"])
            tail = call("serial_read", {"encoding":"hex","until":"sequence","terminator":end.hex(),"timeout_ms":3000,"max_bytes":16384,"output_limit":65536})
            combined = bytes.fromhex(capped["hex"])+bytes.fromhex(tail["data"])
            check("receive cap continuation preserves every output byte",tail["matched"] and combined.split(begin,1)[1].split(end,1)[0]==b"0"*2048)
            check("unconsumed completion marker cannot finish next exec",execute("command printf cap-recovered")["data"]=="cap-recovered")
            # Emit all byte values through the board's normal terminal. ONLCR
            # expands LF to CRLF; hex reads must preserve that actual byte stream.
            fixture = remote+"/stream.bin"
            payload = bytes(range(256))*4
            octal = "".join(f"\\{byte:03o}" for byte in payload)
            execute(f"command printf {quote(octal)} > {quote(fixture)}")
            expected = payload.replace(b"\n",b"\r\n")
            begin, end = b"\x1eSTREAM_BEGIN\x1f", b"\x1eSTREAM_END\x1f"
            call("serial_run_command", {"command":f"command printf '\\036STREAM_BEGIN\\037'; cat {quote(fixture)}; command printf '\\036STREAM_END\\037'","until":begin.decode(),"strip_ansi":False,"timeout_ms":3000})
            collected = bytearray()
            sizes = [1,2,7,31,127,128,255,256]
            sizes.append(len(expected)-sum(sizes))
            try:
                for index,size in enumerate(sizes):
                    if index==1:
                        call("serial_configure", {"profile":"binary"})
                    elif index==2:
                        call("serial_configure", {"profile":"terminal"})
                    arguments = {"max_bytes":size,"timeout_ms":3000,"output_limit":8192}
                    if index!=1:
                        arguments["encoding"]="hex"
                    part = call("serial_read",arguments)
                    assert part["stop_reason"]=="max_bytes" and part["bytes_read"]==size,part
                    collected.extend(bytes.fromhex(part["data"]))
            finally:
                call("serial_configure", {"profile":"terminal"})
            check("small reads and profile switches preserve all 256 byte values",bytes(collected)==expected)
            tail = call("serial_read", {"encoding":"hex","until":"sequence","terminator":end.hex(),"timeout_ms":3000})
            check("exact length reads leave following frame intact",tail["matched"] and bytes.fromhex(tail["data"])==end)
            check("shell usable after binary stream framing",execute("command printf stream-recovered")["data"]=="stream-recovered")
            begin, end = b"\x1eCANCEL_BEGIN\x1f", b"\x1eCANCEL_END\x1f"
            call("serial_run_command", {"command":"command printf '\\036CANCEL_BEGIN\\037cancel-prefix'; sleep 2; command printf 'cancel-tail\\036CANCEL_END\\037'","until":begin.decode(),"timeout_ms":3000})
            with concurrent.futures.ThreadPoolExecutor() as pool:
                future = pool.submit(call,"serial_read", {"encoding":"hex","timeout_ms":10000,"max_bytes":4096})
                deadline = time.monotonic()+3
                while True:
                    state = call("serial_status")
                    if any(port["port"]==args.port and port.get("busy") for port in state["ports"]):
                        break
                    assert not future.done() and time.monotonic()<deadline
                    time.sleep(0.01)
                time.sleep(0.1)
                call("serial_cancel")
                partial = future.result(timeout=3)
            check("cancelled read returns received prefix",partial["stop_reason"]=="cancelled" and not partial["matched"] and bytes.fromhex(partial["data"])==b"cancel-prefix")
            tail = call("serial_read", {"encoding":"hex","until":"sequence","terminator":end.hex(),"timeout_ms":5000})
            check("cancelled read leaves later remote output intact",tail["matched"] and bytes.fromhex(partial["data"])+bytes.fromhex(tail["data"])==b"cancel-prefixcancel-tail"+end)
            check("shell usable after read cancellation without control key",execute("command printf cancel-read-recovered")["data"]=="cancel-read-recovered")
            repeated = True
            for index in range(20):
                result = call("serial_exec", {"command":f"printf 'repeat-{index}-中文'; exit {index%3}","timeout_ms":1500})
                repeated = repeated and result.get("completed") and result.get("exit_code")==index%3 and result["data"]==f"repeat-{index}-中文"
            check("20 repeated calls keep output and exit code isolated",repeated)
            timed = call("serial_exec", {"command":"printf stale-start; sleep 1; printf stale-end","timeout_ms":100})
            check("late completion marker cannot finish next request",not timed["completed"] and execute("printf fresh-output")["data"]=="fresh-output")
            timed = call("serial_exec", {"command":f"printf X >> {quote(remote+'/once.txt')}; sleep 1","timeout_ms":100})
            check("timeout never replays command side effect",not timed["completed"] and execute(f"wc -c < {quote(remote+'/once.txt')}")["data"].strip()=="1")
            check("two second silence still waits for completion",execute("sleep 2; printf after-quiet-period",timeout_ms=4000)["data"]=="after-quiet-period")
            call("serial_run_command", {"command":f"cd {quote(remote)}","timeout_ms":1000,"idle_timeout_ms":100})
            try:
                for protocol in ["xmodem","xmodem-1k","ymodem","zmodem"]:
                    protocol_dir = remote+"/filenames-"+protocol
                    execute(f"mkdir -p {quote(protocol_dir)}")
                    call("serial_run_command", {"command":f"cd {quote(protocol_dir)}","timeout_ms":1000,"idle_timeout_ms":100})
                    for index,filename in enumerate(["-leading.bin","-","--help"]):
                        payload = f"{protocol}:{filename}".encode()+bytes(range(32))+b"\x00\xff"
                        source = pathlib.Path(local)/f"dash-{protocol}-{index}.bin"
                        source.write_bytes(payload)
                        sent = call("serial_send_file", {"protocol":protocol,"local_path":str(source),"remote_path":filename,"timeout_ms":2000})
                        check(f"{protocol} upload literal filename {filename}",sent["shell_ready"] and sent["remote_exit_code"]==0)
                        destination = pathlib.Path(local)/f"dash-{protocol}-{index}.received"
                        received = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"remote_path":filename,"timeout_ms":2000})
                        actual = destination.read_bytes()
                        check(f"{protocol} download literal filename {filename}",received["shell_ready"] and actual[:len(payload)]==payload and all(byte==0x1a for byte in actual[len(payload):]))
            finally:
                call("serial_run_command", {"command":f"cd {quote(cwd)}","timeout_ms":1000,"idle_timeout_ms":100})
            for protocol in ["ymodem","zmodem"]:
                flags = "--ymodem " if protocol=="ymodem" else ""
                destination = pathlib.Path(local)/f"multiple-{protocol}.bin"
                started = time.monotonic()
                result = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"command":f"lsz {flags}{quote(remote+'/first.txt')} {quote(remote+'/second.txt')}","shell_command":True,"timeout_ms":1000},error=True)
                reason = "batch terminator" if protocol=="ymodem" else "multiple files"
                check(f"{protocol} rejects batch and keeps reason",reason in result["error"] and time.monotonic()-started<8,error=result["error"])
                check(f"{protocol} batch failure leaves no local partial",not destination.exists())
                check(f"{protocol} peer exits after rejection",execute("printf batch-recovered")["data"]=="batch-recovered")
                bad = pathlib.Path(local)/f"invalid-{protocol}?.bin"
                # Windows filename error after protocol completion, rather than preflight.
                if sys.platform=="win32":
                    result = call("serial_receive_file", {"protocol":protocol,"local_path":str(bad),"remote_path":remote+"/first.txt","timeout_ms":1000},error=True)
                    check(f"{protocol} local save failure is reported", "failed to save" in result["error"] and not bad.exists())
                    check(f"{protocol} shell survives local save failure",execute("printf save-recovered")["data"]=="save-recovered")
                sentinel = pathlib.Path(local)/f"existing-{protocol}.bin"
                sentinel.write_bytes(b"keep this local file")
                marker = remote+f"/should-not-run-{protocol}"
                call("serial_receive_file", {"protocol":protocol,"local_path":str(sentinel),"command":f"printf unexpected > {quote(marker)}; lsz {flags}{quote(remote+'/first.txt')}","shell_command":True,"timeout_ms":1000},error=True)
                check(f"{protocol} existing destination blocks remote command",sentinel.read_bytes()==b"keep this local file" and execute(f"test ! -e {quote(marker)}")["exit_code"]==0)
                destination = pathlib.Path(local)/f"success-{protocol}.bin"
                call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"remote_path":remote+"/first.txt","timeout_ms":1000})
                check(f"{protocol} next download succeeds",destination.read_bytes()==b"first file\n")
                payload = bytes(range(256))*4+b"\x00"
                source = pathlib.Path(local)/f"tty-source-{protocol}.bin"
                source.write_bytes(payload)
                prefix = "test -t 0 || exit 91; : '" + "A"*160 + "'; "
                result = call("serial_send_file", {"protocol":protocol,"local_path":str(source),"remote_path":f"tty-{protocol}.bin","command":prefix+f"cd {quote(remote)} && lrz "+("--ymodem -y" if protocol=="ymodem" else "-y"),"shell_command":True,"timeout_ms":2000})
                check(f"{protocol} long upload command keeps terminal stdin",result["shell_ready"] and result["remote_exit_code"]==0)
                destination = pathlib.Path(local)/f"tty-download-{protocol}.bin"
                result = call("serial_receive_file", {"protocol":protocol,"local_path":str(destination),"command":prefix+f"lsz {flags}{quote(remote+'/tty-'+protocol+'.bin')}","shell_command":True,"timeout_ms":2000})
                check(f"{protocol} long download command keeps terminal stdin",result["shell_ready"] and destination.read_bytes()==payload)
                for mutation in ["create-target","remove-parent"]:
                    parent = pathlib.Path(local)/f"race-{protocol}-{mutation}"
                    parent.mkdir()
                    destination = parent/"result.bin"
                    marker = remote+f"/race-started-{protocol}-{mutation}"
                    with concurrent.futures.ThreadPoolExecutor() as pool:
                        future = pool.submit(call,"serial_receive_file", {"protocol":protocol,"local_path":str(destination),"command":f"printf started > {quote(marker)}; sleep 1; lsz {flags}{quote(remote+'/first.txt')}","shell_command":True,"timeout_ms":2000},True)
                        deadline = time.monotonic()+3
                        while True:
                            state = call("serial_status")
                            if any(port["port"]==args.port and port.get("busy") for port in state["ports"]):
                                break
                            assert not future.done() and time.monotonic()<deadline
                            time.sleep(0.01)
                        time.sleep(0.15)
                        if mutation=="create-target":
                            with destination.open("xb") as file:
                                file.write(b"created during transfer")
                        else:
                            parent.rmdir()
                        failure = future.result(timeout=10)
                    execute(f"test -f {quote(marker)}")
                    preserved = destination.read_bytes()==b"created during transfer" if mutation=="create-target" else not parent.exists()
                    check(f"{protocol} {mutation} during receive is handled", "failed to save" in failure["error"] and preserved)
                    check(f"{protocol} shell ready after {mutation} race",execute("printf race-recovered")["data"]=="race-recovered")
            unicode = execute("printf '中文😀ABCD'",output_limit=4,response_mode="detailed")
            check("minimum UTF-8 page size makes progress",page_all(unicode["output_id"],limit=4)=="中文😀ABCD")
            for field in ["data","raw_data","hex"]:
                expected = page_all(unicode["output_id"],field)
                for mode in ["compact","detailed"]:
                    actual = page_all(unicode["output_id"],field,limit=64,output_limit=4,response_mode=mode)
                    check(f"{mode} {field} pagination respects smaller output limit",actual==expected)
            invalid = call("serial_get_output", {"output_id":unicode["output_id"],"offset":1,"limit":4},error=True)
            check("invalid UTF-8 offset is rejected", "UTF-8 byte boundary" in invalid["error"])
            detailed = call("serial_exec", {"command":"printf '\\033[31mcolor\\033[0m\\n'","output_limit":4,"response_mode":"detailed"},envelope=True)
            value = detailed["structuredContent"]
            check("detailed text and structured result agree",json.loads(detailed["content"][0]["text"])==value)
            output_id = value["output_id"]
            clean, raw, hex_data = (page_all(output_id,field) for field in ["data","raw_data","hex"])
            check("all retained representations page without data loss",clean=="color\r\n" and b"\x1b[31m" in bytes.fromhex(hex_data) and bytes.fromhex(hex_data)==raw.encode())
            outputs = [execute(f"printf cache-{index:02}-ABCDEFGHIJK",output_limit=4)["output_id"] for index in range(17)]
            oldest = call("serial_get_output", {"output_id":outputs[0],"limit":4},error=True)
            check("cache evicts oldest after 16 results", "expired" in oldest["error"] and page_all(outputs[-1])=="cache-16-ABCDEFGHIJK")
            call("serial_configure", {"profile":"binary"})
            call("serial_exec", {"command":"printf should-not-execute"},error=True)
            call("serial_configure", {"profile":"terminal"})
            check("binary profile rejects shell then terminal recovers",execute("printf profile-ready")["data"]=="profile-ready")
            call("serial_close")
            call("serial_open", {"profile":"terminal"})
            check("close and reopen preserves remote shell cwd",execute("pwd")["data"].strip()==cwd)
        finally:
            try:
                if remote and process.poll() is None:
                    call("serial_configure", {"profile":"terminal"})
                    call("serial_run_command", {"command":f"cd {quote(cwd)}","timeout_ms":1000,"idle_timeout_ms":100})
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
        check("remote temporary directory removed",cleanup)
        report = {"tested_at":datetime.datetime.now(datetime.timezone.utc).isoformat(),"identity":identity,"version":subprocess.check_output([str(pathlib.Path(args.exe).resolve()),"--version"],text=True).strip(),"exe":str(pathlib.Path(args.exe).resolve()),"exe_sha256":hashlib.sha256(pathlib.Path(args.exe).read_bytes()).hexdigest(),"checks":checks,"passed":len(checks),"cleanup":cleanup}
        args.report.parent.mkdir(parents=True,exist_ok=True)
        args.report.write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding="utf-8")
        print(json.dumps(report,ensure_ascii=False,indent=2))


if __name__=="__main__":
    main()
