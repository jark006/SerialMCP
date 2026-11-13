use serde_json::{Value, json};

pub(super) fn tool_definitions() -> Vec<Value> {
    let mut tools = vec![
        json!({
            "name": "serial_list_ports",
            "description": "Get a live snapshot of available Windows COM ports and USB identity metadata. Call this before opening a port or when a board may have been replugged.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_status",
            "description": "Re-enumerate hardware and report tracked ports as available, open, faulted, or disconnected. A disconnected active port remains selected so it can be reconnected without repeating its name.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_open",
            "description": "Open and select a COM port for an embedded UART console. Defaults to 115200 8N1 with no flow control. Use serial_reconnect instead if the port is already tracked as disconnected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name, e.g. COM3 or \\\\.\\COM10"},
                    "baud_rate": {"type": "integer", "minimum": 1, "maximum": 4000000, "default": 115200},
                    "data_bits": {"type": "integer", "enum": [5, 6, 7, 8], "default": 8},
                    "stop_bits": {"type": "integer", "enum": [1, 2], "default": 1},
                    "parity": {"type": "string", "enum": ["none", "odd", "even"], "default": "none"},
                    "flow_control": {"type": "string", "enum": ["none", "software", "hardware"], "default": "none"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 1000}
                },
                "required": ["port"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_reconnect",
            "description": "Reopen a tracked port after USB hot-plug using its saved serial settings. Refuses a different USB identity unless force=true. Does not replay any failed write.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "Tracked COM port; defaults to the active port"},
                    "force": {"type": "boolean", "default": false, "description": "Allow reconnect when USB identity metadata changed"}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_close",
            "description": "Close and forget an open, faulted, or disconnected COM port. If omitted, port defaults to the active port.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_configure",
            "description": "Change baud rate, data bits, stop bits, parity, flow control, or read timeout on an open port.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "baud_rate": {"type": "integer", "minimum": 1, "maximum": 4000000},
                    "data_bits": {"type": "integer", "enum": [5, 6, 7, 8]},
                    "stop_bits": {"type": "integer", "enum": [1, 2]},
                    "parity": {"type": "string", "enum": ["none", "odd", "even"]},
                    "flow_control": {"type": "string", "enum": ["none", "software", "hardware"]},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_write",
            "description": "Write text or hex bytes to an open port. Use serial_run_command for a Linux shell command that needs a collected response. Failed writes are never retried automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "data": {"type": "string", "description": "Payload; text by default, or hex with optional spaces/0x prefix"},
                    "encoding": {"type": "string", "enum": ["text", "hex"], "default": "text"},
                    "line_ending": {"type": "string", "enum": ["none", "lf", "cr", "crlf"], "default": "none"}
                },
                "required": ["data"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_read",
            "description": "Read raw console bytes until an overall timeout, a line ending, an exact text sequence, or max_bytes. Returns text/hex and an explicit stop_reason.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 1000},
                    "max_bytes": {"type": "integer", "minimum": 1, "maximum": 1048576, "default": 4096},
                    "encoding": {"type": "string", "enum": ["text", "hex"], "default": "text"},
                    "until": {"type": "string", "enum": ["timeout", "line", "sequence"], "default": "timeout"},
                    "terminator": {"type": "string", "minLength": 1, "maxLength": 1024, "description": "Exact UTF-8 sequence required when until=sequence"}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_run_command",
            "description": "Recommended tool for an embedded Linux shell: atomically send one command and collect its response. With until set, waits for that exact prompt/token; otherwise waits for the first byte and stops after the response becomes idle.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "command": {"type": "string", "maxLength": 65536, "description": "Shell command or terminal input"},
                    "line_ending": {"type": "string", "enum": ["none", "lf", "cr", "crlf"], "default": "lf"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 5000, "description": "Overall response deadline, including time to first byte"},
                    "idle_timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 300, "description": "Stop after this quiet period once data arrives; ignored when until is set"},
                    "max_bytes": {"type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536},
                    "until": {"type": "string", "minLength": 1, "maxLength": 1024, "description": "Exact prompt or token to wait for, such as '# ' or 'login:'"},
                    "clear_input": {"type": "boolean", "default": true, "description": "Discard unread input before sending so stale prompts cannot complete this command; set false to preserve boot/login output"},
                    "strip_ansi": {"type": "boolean", "default": true, "description": "Remove ANSI terminal escapes from data while preserving raw_data and hex"}
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_check_modem_tools",
            "description": "Probe the embedded Linux shell for lrz/lsz and compatible rz/sz/rx/sx/rb/sb modem commands. The probe is atomic and returns a boolean map of executable files found in PATH (existence alone is insufficient).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 10000}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_send_file",
            "description": "Atomically start a receiver on the embedded target and upload one local file over XMODEM, XMODEM-1K, YMODEM, or ZMODEM. Defaults to ZMODEM and uses lrz (override command for rz). Set start_command=false only when the target receiver is already waiting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "protocol": {"type": "string", "enum": ["xmodem", "xmodem-1k", "ymodem", "zmodem"], "default": "zmodem"},
                    "local_path": {"type": "string", "minLength": 1, "description": "Path on the Windows host running serial-mcp"},
                    "remote_path": {"type": "string", "minLength": 1, "description": "Target filename. Required for automatic XMODEM startup; used as transmitted filename for Y/ZMODEM."},
                    "command": {"type": "string", "minLength": 1, "description": "Custom target receive command; overrides rx/rb/rz default"},
                    "start_command": {"type": "boolean", "default": true, "description": "Send a target receive command before starting the protocol"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 10000, "description": "Timeout for each protocol response"}
                },
                "required": ["local_path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_receive_file",
            "description": "Atomically start a sender on the embedded target and download one file to the Windows host over XMODEM, XMODEM-1K, YMODEM, or ZMODEM. Defaults to ZMODEM and uses lsz (override command for sz). Set start_command=false only when the target sender is already running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "protocol": {"type": "string", "enum": ["xmodem", "xmodem-1k", "ymodem", "zmodem"], "default": "zmodem"},
                    "local_path": {"type": "string", "minLength": 1, "description": "Destination path on the Windows host running serial-mcp"},
                    "remote_path": {"type": "string", "minLength": 1, "description": "Source path on the embedded target; required for automatic startup"},
                    "command": {"type": "string", "minLength": 1, "description": "Custom target send command; overrides sx/sb/sz default"},
                    "start_command": {"type": "boolean", "default": true, "description": "Send a target send command before starting the protocol"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 10000, "description": "Timeout for each protocol response"}
                },
                "required": ["local_path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_flush",
            "description": "Discard pending input and/or output buffers on an open COM port.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "direction": {"type": "string", "enum": ["input", "output", "both"], "default": "both"}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "serial_set_control",
            "description": "Set DTR/RTS modem-control lines or send a serial break signal. Useful for board reset sequences.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "dtr": {"type": "boolean", "description": "Set Data Terminal Ready line"},
                    "rts": {"type": "boolean", "description": "Set Request To Send line"},
                    "break_ms": {"type": "integer", "minimum": 0, "maximum": 10000, "description": "Assert break for this many milliseconds"}
                },
                "required": [],
                "additionalProperties": false
            }
        }),
    ];
    tools.extend(extra_tools());
    for tool in &mut tools {
        let name = tool["name"].as_str().unwrap().to_string();
        let properties = tool["inputSchema"]["properties"].as_object_mut().unwrap();
        properties.insert("response_mode".into(), json!({"type":"string", "enum":["compact","detailed"], "default":"compact", "description":"Compact: one bounded JSON text. Detailed: adds raw/hex and structuredContent."}));
        properties.insert("output_limit".into(), json!({"type":"integer", "minimum":4, "maximum":1048576, "default":8192, "description":"UTF-8 bytes per output field; truncated output is cached for serial_get_output."}));
        if matches!(name.as_str(), "serial_open" | "serial_configure") {
            properties.insert("profile".into(), json!({"type":"string", "enum":["terminal","binary"], "description":"terminal: text, shell, login, keys; binary: hex by default, no implicit terminal commands."}));
        }
        if matches!(name.as_str(), "serial_read" | "serial_write") {
            properties.insert("encoding".into(), json!({"type":"string", "enum":["text","hex"], "description":"Defaults to text for terminal profile, hex for binary profile."}));
        }
        if name == "serial_read" {
            properties.insert("terminator_encoding".into(), json!({"type":"string", "enum":["text","hex"], "description":"Terminator encoding; defaults to effective encoding."}));
            properties.insert("idle_timeout_ms".into(), json!({"type":"integer", "minimum":1, "maximum":600000, "description":"Optional quiet window after first data; no greater than timeout_ms."}));
            properties.get_mut("terminator").unwrap()["description"] = json!(
                "Exact sequence in terminator_encoding; required when until=sequence. Unread suffix is preserved."
            );
        }
        if matches!(name.as_str(), "serial_send_file" | "serial_receive_file") {
            for field in ["local_path", "remote_path", "command"] {
                let description = properties[field]["description"]
                    .as_str()
                    .unwrap()
                    .to_string();
                properties.get_mut(field).unwrap()["description"] =
                    json!(format!("{description} NUL is not allowed."));
            }
            properties.insert("shell_command".into(), json!({"type":"boolean", "description":"Wrap a POSIX shell command and confirm its exit before releasing the port. Default true for generated commands, false for custom commands. Set true with custom lrz/lsz; keep false for bootloader commands. Requires start_command=true."}));
        }
        if name == "serial_run_command" {
            properties.get_mut("clear_input").unwrap()["default"] = json!(false);
            properties.get_mut("clear_input").unwrap()["description"] = json!(
                "Explicitly discard old input before writing. Default false preserves boot/login output; use unique markers when completion matters."
            );
            tool["description"] = json!(
                "Atomically send terminal input and wait for a prompt/until or quiet window. For login, bootloaders, or interactive commands. Idle/prompt matching does not prove shell completion; use serial_exec for POSIX shell exit codes. terminal profile only."
            );
        }
        if name == "serial_status" {
            tool["description"] = json!(
                "Live enumeration with open/faulted/disconnected/available states, active port, profile, and busy operation snapshots. Returns promptly while reads/transfers run."
            );
        }
        if name == "serial_open" {
            tool["description"] = json!(
                "Open and select a Windows COM port, default 115200 8N1. Choose terminal profile (default) for Linux/login/bootloader, binary for MCU byte protocols. Opening a port may toggle driver control lines."
            );
        }
        if name == "serial_read" {
            tool["description"] = json!(
                "Read bytes until deadline, line, sequence, or max_bytes. Binary profile defaults to hex. Sequence supports hex terminators. Bytes after a match are preserved. A timeout returns partial data, not proof of completion."
            );
        }
        if name == "serial_send_file" || name == "serial_receive_file" {
            let old = tool["description"].as_str().unwrap().to_string();
            tool["description"] = json!(format!(
                "{old} Binary profile requires an explicit command or start_command=false."
            ));
        }
        tool["annotations"] = json!({"readOnlyHint":matches!(name.as_str(),"serial_list_ports"|"serial_status"|"serial_get_output"), "destructiveHint":!matches!(name.as_str(),"serial_list_ports"|"serial_status"|"serial_get_output"), "idempotentHint":false, "openWorldHint":true});
    }
    tools
}

fn extra_tools() -> Vec<Value> {
    let port = json!({"type":"string", "description":"Windows COM port; omitted uses process-wide active selection."});
    let timeout = json!({"type":"integer", "minimum":1, "maximum":600000, "default":1000});
    let max_bytes = json!({"type":"integer", "minimum":1, "maximum":1048576, "default":4096});
    let encoding = json!({"type":"string", "enum":["text","hex"]});
    vec![
        json!({"name":"serial_select", "description":"Select an already tracked port as the default; does not open it or send data.", "inputSchema":{"type":"object","properties":{"port":port},"required":["port"],"additionalProperties":false}}),
        json!({"name":"serial_cancel", "description":"Request cancellation of the active read/run_command/exec/exchange/probe on a port. Returns partial data in the original request. Does not send Ctrl+C or cancel a file transfer/driver write. Wait for original request to finish before sending keys.", "inputSchema":{"type":"object","properties":{"port":port},"additionalProperties":false}}),
        json!({"name":"serial_send_key", "description":"Send one terminal key without an appended newline. terminal profile only. Ctrl+C interrupts only if the target terminal interprets it; this differs from a serial break signal.", "inputSchema":{"type":"object","properties":{"port":port,"key":{"type":"string","enum":["ctrl_c","ctrl_d","ctrl_z","ctrl_l","escape","tab","enter","backspace","up","down","left","right"]}},"required":["key"],"additionalProperties":false}}),
        json!({"name":"serial_exec", "description":"Execute a POSIX shell command in a subshell with unique non-echoed completion markers and exit_code. Requires an already logged-in shell and terminal profile. stdin defaults to EOF; explicit pipes/redirections work. Use serial_run_command for interactive input. cwd/environment changes do not persist. Timeout/max_bytes/cancelled may leave the remote command running; never automatically replay it.", "inputSchema":{"type":"object","properties":{"port":port,"command":{"type":"string","minLength":1,"maxLength":65536},"timeout_ms":{"type":"integer","minimum":1,"maximum":600000,"default":5000},"max_bytes":{"type":"integer","minimum":1,"maximum":1048576,"default":65536},"clear_input":{"type":"boolean","default":false}},"required":["command"],"additionalProperties":false}}),
        json!({"name":"serial_exchange", "description":"Atomically write one request and collect byte response. Defaults to profile encoding and no line ending; input is preserved unless clear_input=true. Set max_bytes to exact frame length, terminator for delimiter framing, or idle_timeout_ms for silence framing. Never adds shell syntax or retries writes.", "inputSchema":{"type":"object","properties":{"port":port,"data":{"type":"string","minLength":1},"encoding":encoding,"response_encoding":encoding,"line_ending":{"type":"string","enum":["none","lf","cr","crlf"],"default":"none"},"timeout_ms":timeout,"max_bytes":max_bytes,"terminator":{"type":"string","minLength":1,"description":"Exact delimiter, interpreted using terminator_encoding (defaults to response_encoding)."},"terminator_encoding":encoding,"idle_timeout_ms":{"type":"integer","minimum":1,"maximum":600000},"clear_input":{"type":"boolean","default":false}},"required":["data"],"additionalProperties":false}}),
        json!({"name":"serial_get_output", "description":"Page a truncated result from process memory without reading the port or repeating its command. Last 16 truncated results, up to 16 MiB total; cleared on restart. Offsets/limits use UTF-8 bytes; follow next_offset.", "inputSchema":{"type":"object","properties":{"output_id":{"type":"string"},"field":{"type":"string","enum":["data","raw_data","hex"],"default":"data"},"offset":{"type":"integer","minimum":0,"default":0},"limit":{"type":"integer","minimum":4,"maximum":1048576,"default":4096}},"required":["output_id"],"additionalProperties":false}}),
    ]
}
