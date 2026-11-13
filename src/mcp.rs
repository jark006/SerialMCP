mod schema;
use crate::serial::{
    DEFAULT_BAUD_RATE, DEFAULT_DATA_BITS, DEFAULT_FLOW_CONTROL, DEFAULT_PARITY, DEFAULT_STOP_BITS,
    DEFAULT_TIMEOUT_MS, PartialSettings, PortSettings, Profile, SerialHub,
};
use crate::{
    output::{self, ResponseMode},
    terminal::{self, ShellCommand},
};
use schema::tool_definitions;
use serde::Deserialize;
use serde_json::{Value, json};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", MCP_PROTOCOL_VERSION];
pub const SERVER_NAME: &str = "serial-mcp";

#[derive(Debug)]
pub(crate) struct ToolError {
    kind: String,
    pub(crate) message: String,
}

impl ToolError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            kind: "invalid_params".to_string(),
            message: message.into(),
        }
    }

    fn tool(message: impl Into<String>) -> Self {
        Self {
            kind: "tool_error".to_string(),
            message: message.into(),
        }
    }
}

pub fn handle_message(message: Value, hub: &mut SerialHub) -> Option<Value> {
    let id = message.get("id").cloned();
    if !valid_request(&message) {
        return Some(rpc_error(
            id.filter(|id| id.is_null() || id.is_string() || id.is_number())
                .unwrap_or(Value::Null),
            -32600,
            "invalid JSON-RPC request",
        ));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
            -32600,
            "method must be a string",
        ));
    };
    match method {
        "initialize" => initialize(id, message.get("params")),
        "ping" => id.map(|id| rpc_result(id, json!({}))),
        "tools/list" => id.map(|id| rpc_result(id, json!({"tools": tool_definitions()}))),
        "tools/call" => id.map(|id| call_tool(id, message.get("params"), hub)),
        "notifications/initialized" | "notifications/cancelled" => None,
        _ => id.map(|id| rpc_error(id, -32601, "method not found")),
    }
}

fn initialize(id: Option<Value>, params: Option<&Value>) -> Option<Value> {
    let id = id?;
    let requested_version = params
        .and_then(Value::as_object)
        .and_then(|params| params.get("protocolVersion"))
        .and_then(Value::as_str);
    let valid = params.and_then(Value::as_object).is_some_and(|params| {
        params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_some()
            && params
                .get("capabilities")
                .and_then(Value::as_object)
                .is_some()
            && params
                .get("clientInfo")
                .and_then(Value::as_object)
                .is_some_and(|info| {
                    info.get("name").and_then(Value::as_str).is_some()
                        && info.get("version").and_then(Value::as_str).is_some()
                })
    });
    if !valid {
        return Some(rpc_error(id, -32602, "invalid initialize params"));
    }
    let protocol_version = requested_version
        .filter(|version| SUPPORTED_PROTOCOL_VERSIONS.contains(version))
        .unwrap_or(MCP_PROTOCOL_VERSION);
    Some(rpc_result(
        id,
        json!({
            "protocolVersion": protocol_version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            "instructions": "UART tools on Windows: list ports, open with profile=terminal for Linux/login/bootloader or profile=binary for MCU frames. Use serial_exec for POSIX shell exit codes, serial_run_command for prompts/login, serial_exchange for atomic byte requests, serial_send_key for Ctrl+C/arrows. Responses are compact by default; response_mode=detailed adds raw data. Long output is retained for serial_get_output. Ports operate independently; the same busy port fails immediately. serial_cancel stops an active read but does not interrupt the remote program. Never retry writes automatically after errors. All clients share ports and active selection; specify port with multiple boards."
        }),
    ))
}

fn call_tool(id: Value, params: Option<&Value>, hub: &mut SerialHub) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return rpc_error(id, -32602, "tools/call params must be an object");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return rpc_error(id, -32602, "tool name must be a string");
    };
    let mut arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "tool arguments must be an object");
    }
    if !tool_names().contains(&name) {
        return rpc_error(id, -32602, "unknown tool");
    }

    let options = match response_options(&mut arguments) {
        Ok(options) => options,
        Err(error) => return rpc_error(id, -32602, &error.message),
    };

    let result: Result<Value, ToolError> = match name {
        "serial_get_output" => decode::<OutputArgs>(&arguments).and_then(|args| {
            if !matches!(args.field.as_str(), "data" | "hex" | "raw_data") {
                return Err(ToolError::invalid_params(
                    "field must be data, hex, or raw_data",
                ));
            }
            if !(4..=1_048_576).contains(&args.limit) {
                return Err(ToolError::invalid_params(
                    "limit must be between 4 and 1048576",
                ));
            }
            output::page(
                &args.output_id,
                &args.field,
                args.offset,
                args.limit.min(options.1),
            )
            .map_err(ToolError::tool)
        }),
        "serial_cancel" => Err(ToolError::tool(
            "no cancellable operation is running on this port",
        )),
        "serial_select" => decode::<SelectArgs>(&arguments).and_then(|args| {
            validate_port_string(&args.port)?;
            hub.select_port(&args.port)
                .map(|value| json!(value))
                .map_err(ToolError::tool)
        }),
        "serial_send_key" => decode::<KeyArgs>(&arguments).and_then(|args| {
            if let Some(port) = &args.port {
                validate_port_string(port)?;
            }
            let bytes = terminal::key_bytes(&args.key).ok_or_else(|| {
                ToolError::invalid_params("unsupported key; see inputSchema enum")
            })?;
            require_terminal(hub, args.port.as_deref())?;
            hub.write_bytes(args.port.as_deref(), bytes, "hex")
                .map(|value| json!(value))
                .map_err(ToolError::tool)
        }),
        "serial_exec" => decode::<ExecArgs>(&arguments).and_then(|args| {
            if let Some(port) = &args.port {
                validate_port_string(port)?;
            }
            validate_timeout(args.timeout_ms)?;
            validate_max_bytes(args.max_bytes)?;
            if args.command.is_empty() || args.command.len() > 65_536 || args.command.contains('\0')
            {
                return Err(ToolError::invalid_params(
                    "command must contain 1..65536 bytes without NUL",
                ));
            }
            require_terminal(hub, args.port.as_deref())?;
            let shell = ShellCommand::for_exec(&args.command);
            let result = hub
                .run_command(
                    args.port.as_deref(),
                    &shell.payload,
                    args.timeout_ms,
                    300,
                    args.max_bytes,
                    Some(&shell.end),
                    args.clear_input,
                    false,
                    "text",
                    true,
                )
                .map_err(ToolError::tool)?;
            let (data, exit_code) = shell.parse(result.raw_data.as_bytes());
            let mut value = json!(result);
            value["data"] = json!(String::from_utf8_lossy(&terminal::strip_ansi_bytes(&data)));
            value["exit_code"] = json!(exit_code);
            value["completed"] = json!(exit_code.is_some());
            value["remote_may_be_running"] = json!(exit_code.is_none());
            if exit_code.is_some() {
                value["stop_reason"] = json!("completed");
            }
            Ok(value)
        }),
        "serial_exchange" => decode::<ExchangeArgs>(&arguments).and_then(|args| {
            if let Some(port) = &args.port {
                validate_port_string(port)?;
            }
            validate_timeout(args.timeout_ms)?;
            validate_max_bytes(args.max_bytes)?;
            validate_line_ending(&args.line_ending)?;
            if let Some(encoding) = &args.terminator_encoding {
                validate_encoding(encoding)?;
            }
            if args.terminator_encoding.is_some() && args.terminator.is_none() {
                return Err(ToolError::invalid_params(
                    "terminator_encoding requires terminator",
                ));
            }
            if args.data.is_empty() {
                return Err(ToolError::invalid_params("data must not be empty"));
            }
            let encoding = effective_encoding(args.encoding.as_deref(), hub, args.port.as_deref())?;
            validate_encoding(encoding)?;
            let response_encoding = args.response_encoding.as_deref().unwrap_or(encoding);
            validate_encoding(response_encoding)?;
            if let Some(idle) = args.idle_timeout_ms {
                validate_timeout(idle)?;
                if idle > args.timeout_ms {
                    return Err(ToolError::invalid_params(
                        "idle_timeout_ms must not exceed timeout_ms",
                    ));
                }
            }
            let mut payload = encode_data(&args.data, encoding)?;
            append_line_ending(&mut payload, &args.line_ending);
            let terminator = args
                .terminator
                .as_deref()
                .map(|text| {
                    encode_data(
                        text,
                        args.terminator_encoding
                            .as_deref()
                            .unwrap_or(response_encoding),
                    )
                })
                .transpose()?;
            if let Some(bytes) = &terminator
                && (bytes.is_empty() || bytes.len() > 1024)
            {
                return Err(ToolError::invalid_params(
                    "terminator must contain 1..1024 bytes",
                ));
            }
            // No idle timeout unless explicitly requested. Exact byte counts use max_bytes.
            let idle = args.idle_timeout_ms.unwrap_or(args.timeout_ms);
            hub.run_command(
                args.port.as_deref(),
                &payload,
                args.timeout_ms,
                idle,
                args.max_bytes,
                terminator.as_deref(),
                args.clear_input,
                false,
                response_encoding,
                false,
            )
            .map(|value| {
                let mut value = json!(value);
                value["encoding"] = json!(response_encoding);
                value
            })
            .map_err(ToolError::tool)
        }),
        "serial_list_ports" => decode::<EmptyArgs>(&arguments).and_then(|_| {
            SerialHub::list_ports()
                .map(|value| json!(value))
                .map_err(ToolError::tool)
        }),
        "serial_open" => decode::<OpenArgs>(&arguments)
            .and_then(validate_open_args)
            .and_then(|args| {
                hub.open_port(
                    &args.port,
                    PortSettings {
                        baud_rate: args.baud_rate,
                        data_bits: args.data_bits,
                        stop_bits: args.stop_bits,
                        parity: args.parity,
                        flow_control: args.flow_control,
                        timeout_ms: args.timeout_ms,
                        profile: args.profile,
                    },
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_reconnect" => decode::<ReconnectArgs>(&arguments)
            .and_then(validate_reconnect_args)
            .and_then(|args| {
                hub.reconnect_port(args.port.as_deref(), args.force)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_close" => decode::<PortArg>(&arguments)
            .and_then(validate_port_arg)
            .and_then(|args| {
                hub.close_port(args.port.as_deref())
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_configure" => decode::<ConfigureArgs>(&arguments)
            .and_then(validate_configure_args)
            .and_then(|args| {
                hub.configure_port(
                    args.port.as_deref(),
                    PartialSettings {
                        baud_rate: args.baud_rate,
                        data_bits: args.data_bits,
                        stop_bits: args.stop_bits,
                        parity: args.parity,
                        flow_control: args.flow_control,
                        timeout_ms: args.timeout_ms,
                        profile: args.profile,
                    },
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_write" => decode::<WriteArgs>(&arguments)
            .and_then(validate_write_args)
            .and_then(|args| {
                let encoding =
                    effective_encoding(args.encoding.as_deref(), hub, args.port.as_deref())?;
                let mut data = encode_data(&args.data, encoding)?;
                append_line_ending(&mut data, &args.line_ending);
                hub.write_bytes(args.port.as_deref(), &data, encoding)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_read" => decode::<ReadArgs>(&arguments)
            .and_then(validate_read_args)
            .and_then(|args| {
                let encoding =
                    effective_encoding(args.encoding.as_deref(), hub, args.port.as_deref())?;
                let terminator = args
                    .terminator
                    .as_deref()
                    .map(|text| {
                        encode_data(
                            text,
                            args.terminator_encoding.as_deref().unwrap_or(encoding),
                        )
                    })
                    .transpose()?;
                hub.read_bytes(
                    args.port.as_deref(),
                    args.timeout_ms,
                    args.max_bytes,
                    encoding,
                    &args.until,
                    terminator.as_deref(),
                    args.idle_timeout_ms,
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_run_command" => decode::<RunCommandArgs>(&arguments)
            .and_then(validate_run_command_args)
            .and_then(|args| {
                require_terminal(hub, args.port.as_deref())?;
                let mut payload = args.command.into_bytes();
                append_line_ending(&mut payload, &args.line_ending);
                hub.run_command(
                    args.port.as_deref(),
                    &payload,
                    args.timeout_ms,
                    args.idle_timeout_ms,
                    args.max_bytes,
                    args.until.as_deref().map(str::as_bytes),
                    args.clear_input,
                    args.strip_ansi,
                    "text",
                    false,
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_check_modem_tools" => decode::<ModemToolCheckArgs>(&arguments)
            .and_then(validate_modem_tool_check_args)
            .and_then(|args| {
                require_terminal(hub, args.port.as_deref())?;
                hub.check_modem_tools(args.port.as_deref(), args.timeout_ms)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_send_file" => decode::<FileTransferArgs>(&arguments)
            .and_then(validate_send_file_args)
            .and_then(|args| {
                if args.start_command
                    && (args.command.is_none() || args.shell_command == Some(true))
                {
                    require_terminal(hub, args.port.as_deref())?;
                }
                let command = transfer_command(&args, true)?;
                hub.send_file(
                    args.port.as_deref(),
                    &args.protocol,
                    &args.local_path,
                    args.remote_path.as_deref(),
                    command.as_deref(),
                    args.timeout_ms,
                    args.shell_command.unwrap_or(args.command.is_none()),
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_receive_file" => decode::<FileTransferArgs>(&arguments)
            .and_then(validate_receive_file_args)
            .and_then(|args| {
                if args.start_command
                    && (args.command.is_none() || args.shell_command == Some(true))
                {
                    require_terminal(hub, args.port.as_deref())?;
                }
                let command = transfer_command(&args, false)?;
                hub.receive_file(
                    args.port.as_deref(),
                    &args.protocol,
                    &args.local_path,
                    args.remote_path.as_deref(),
                    command.as_deref(),
                    args.timeout_ms,
                    args.shell_command.unwrap_or(args.command.is_none()),
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_status" => decode::<EmptyArgs>(&arguments).map(|_| json!(hub.status())),
        "serial_flush" => decode::<FlushArgs>(&arguments)
            .and_then(validate_flush_args)
            .and_then(|args| {
                hub.flush(args.port.as_deref(), &args.direction)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_set_control" => decode::<SetControlArgs>(&arguments)
            .and_then(validate_set_control_args)
            .and_then(|args| {
                hub.set_control(args.port.as_deref(), args.dtr, args.rts, args.break_ms)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        _ => unreachable!("tool name was already checked"),
    };

    match result {
        Ok(value) => rpc_result(id, output::success(name, value, options.0, options.1)),
        Err(error) if error.kind == "invalid_params" => rpc_error(id, -32602, &error.message),
        Err(error) => rpc_result(id, error_tool_result(&error.message)),
    }
}

pub(crate) fn valid_request(message: &Value) -> bool {
    message.as_object().is_some_and(|object| {
        object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && object.get("method").is_some_and(Value::is_string)
            && object
                .get("id")
                .is_none_or(|id| id.is_null() || id.is_string() || id.is_number())
            && object
                .get("params")
                .is_none_or(|params| params.is_object() || params.is_array())
    })
}

fn effective_encoding<'a>(
    encoding: Option<&'a str>,
    hub: &SerialHub,
    port: Option<&str>,
) -> Result<&'a str, ToolError> {
    match encoding {
        Some(value) => Ok(value),
        None => hub
            .profile(port)
            .map(|profile| profile.encoding())
            .map_err(ToolError::tool),
    }
}

fn require_terminal(hub: &SerialHub, port: Option<&str>) -> Result<(), ToolError> {
    if hub.profile(port).map_err(ToolError::tool)? == Profile::Binary {
        Err(ToolError::tool(
            "this operation requires profile=terminal; use serial_exchange/read/write for binary devices",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn response_options(arguments: &mut Value) -> Result<(ResponseMode, usize), ToolError> {
    let mode = arguments
        .as_object_mut()
        .and_then(|object| object.remove("response_mode"))
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| ToolError::invalid_params("response_mode must be compact or detailed"))?
        .unwrap_or_default();
    let limit = arguments
        .as_object_mut()
        .and_then(|object| object.remove("output_limit"))
        .map(serde_json::from_value::<usize>)
        .transpose()
        .map_err(|_| ToolError::invalid_params("output_limit must be an integer"))?
        .unwrap_or(output::DEFAULT_OUTPUT_LIMIT);
    if !(4..=1_048_576).contains(&limit) {
        return Err(ToolError::invalid_params(
            "output_limit must be between 4 and 1048576",
        ));
    }
    Ok((mode, limit))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectArgs {
    port: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    port: Option<String>,
    key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputArgs {
    output_id: String,
    #[serde(default = "default_output_field")]
    field: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_max_bytes")]
    limit: usize,
}
fn default_output_field() -> String {
    "data".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    port: Option<String>,
    command: String,
    #[serde(default = "default_command_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_command_max_bytes")]
    max_bytes: usize,
    #[serde(default)]
    clear_input: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeArgs {
    port: Option<String>,
    data: String,
    encoding: Option<String>,
    response_encoding: Option<String>,
    #[serde(default = "default_no_line_ending")]
    line_ending: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_max_bytes")]
    max_bytes: usize,
    terminator: Option<String>,
    terminator_encoding: Option<String>,
    idle_timeout_ms: Option<u64>,
    #[serde(default)]
    clear_input: bool,
}

pub(crate) fn error_tool_result(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "structuredContent": {"error": message},
        "isError": true
    })
}

pub(crate) fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

pub(crate) fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn decode<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| ToolError::invalid_params(format!("invalid arguments: {error}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortArg {
    port: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    port: String,
    #[serde(default = "default_baud_rate")]
    baud_rate: u32,
    #[serde(default = "default_data_bits")]
    data_bits: u8,
    #[serde(default = "default_stop_bits")]
    stop_bits: u8,
    #[serde(default = "default_parity")]
    parity: String,
    #[serde(default = "default_flow_control")]
    flow_control: String,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    profile: Profile,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconnectArgs {
    port: Option<String>,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureArgs {
    port: Option<String>,
    baud_rate: Option<u32>,
    data_bits: Option<u8>,
    stop_bits: Option<u8>,
    parity: Option<String>,
    flow_control: Option<String>,
    timeout_ms: Option<u64>,
    profile: Option<Profile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    port: Option<String>,
    data: String,
    encoding: Option<String>,
    #[serde(default = "default_no_line_ending")]
    line_ending: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    port: Option<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_max_bytes")]
    max_bytes: usize,
    encoding: Option<String>,
    #[serde(default = "default_until")]
    until: String,
    terminator: Option<String>,
    terminator_encoding: Option<String>,
    idle_timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCommandArgs {
    port: Option<String>,
    command: String,
    #[serde(default = "default_command_line_ending")]
    line_ending: String,
    #[serde(default = "default_command_timeout_ms")]
    timeout_ms: u64,
    #[serde(default = "default_idle_timeout_ms")]
    idle_timeout_ms: u64,
    #[serde(default = "default_command_max_bytes")]
    max_bytes: usize,
    until: Option<String>,
    #[serde(default)]
    clear_input: bool,
    #[serde(default = "default_true")]
    strip_ansi: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FlushArgs {
    port: Option<String>,
    #[serde(default = "default_direction")]
    direction: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetControlArgs {
    port: Option<String>,
    dtr: Option<bool>,
    rts: Option<bool>,
    break_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTransferArgs {
    port: Option<String>,
    #[serde(default = "default_modem_protocol")]
    protocol: String,
    local_path: String,
    remote_path: Option<String>,
    command: Option<String>,
    #[serde(default = "default_true")]
    start_command: bool,
    shell_command: Option<bool>,
    #[serde(default = "default_transfer_timeout_ms")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModemToolCheckArgs {
    port: Option<String>,
    #[serde(default = "default_transfer_timeout_ms")]
    timeout_ms: u64,
}

fn default_baud_rate() -> u32 {
    DEFAULT_BAUD_RATE
}
fn default_data_bits() -> u8 {
    DEFAULT_DATA_BITS
}
fn default_stop_bits() -> u8 {
    DEFAULT_STOP_BITS
}
fn default_parity() -> String {
    DEFAULT_PARITY.to_string()
}
fn default_flow_control() -> String {
    DEFAULT_FLOW_CONTROL.to_string()
}
fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}
fn default_until() -> String {
    "timeout".to_string()
}
fn default_no_line_ending() -> String {
    "none".to_string()
}
fn default_command_line_ending() -> String {
    "lf".to_string()
}
fn default_command_timeout_ms() -> u64 {
    5_000
}
fn default_idle_timeout_ms() -> u64 {
    300
}
fn default_max_bytes() -> usize {
    4096
}
fn default_command_max_bytes() -> usize {
    65_536
}
fn default_direction() -> String {
    "both".to_string()
}
fn default_true() -> bool {
    true
}
fn default_modem_protocol() -> String {
    "zmodem".to_string()
}
fn default_transfer_timeout_ms() -> u64 {
    10_000
}

fn validate_open_args(args: OpenArgs) -> Result<OpenArgs, ToolError> {
    validate_port_string(&args.port)?;
    validate_baud(args.baud_rate)?;
    validate_data_bits(args.data_bits)?;
    validate_stop_bits(args.stop_bits)?;
    validate_parity(&args.parity)?;
    validate_flow_control(&args.flow_control)?;
    validate_timeout(args.timeout_ms)?;
    Ok(args)
}

fn validate_configure_args(args: ConfigureArgs) -> Result<ConfigureArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    if let Some(value) = args.baud_rate {
        validate_baud(value)?;
    }
    if let Some(value) = args.data_bits {
        validate_data_bits(value)?;
    }
    if let Some(value) = args.stop_bits {
        validate_stop_bits(value)?;
    }
    if let Some(value) = args.parity.as_deref() {
        validate_parity(value)?;
    }
    if let Some(value) = args.flow_control.as_deref() {
        validate_flow_control(value)?;
    }
    if let Some(value) = args.timeout_ms {
        validate_timeout(value)?;
    }
    if args.baud_rate.is_none()
        && args.data_bits.is_none()
        && args.stop_bits.is_none()
        && args.parity.is_none()
        && args.flow_control.is_none()
        && args.timeout_ms.is_none()
        && args.profile.is_none()
    {
        return Err(ToolError::invalid_params(
            "at least one serial setting is required",
        ));
    }
    Ok(args)
}

fn validate_port_arg(args: PortArg) -> Result<PortArg, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    Ok(args)
}

fn validate_reconnect_args(args: ReconnectArgs) -> Result<ReconnectArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    Ok(args)
}

fn validate_write_args(args: WriteArgs) -> Result<WriteArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    if args.data.is_empty() {
        return Err(ToolError::invalid_params("data must not be empty"));
    }
    if let Some(encoding) = &args.encoding {
        validate_encoding(encoding)?;
    }
    validate_line_ending(&args.line_ending)?;
    Ok(args)
}

fn validate_read_args(args: ReadArgs) -> Result<ReadArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    validate_timeout(args.timeout_ms)?;
    if args.terminator_encoding.is_some() && args.terminator.is_none() {
        return Err(ToolError::invalid_params(
            "terminator_encoding requires terminator",
        ));
    }
    validate_max_bytes(args.max_bytes)?;
    if let Some(idle) = args.idle_timeout_ms {
        validate_timeout(idle)?;
        if idle > args.timeout_ms {
            return Err(ToolError::invalid_params(
                "idle_timeout_ms must not exceed timeout_ms",
            ));
        }
    }
    if let Some(encoding) = &args.terminator_encoding {
        validate_encoding(encoding)?;
    }
    if let Some(encoding) = &args.encoding {
        validate_encoding(encoding)?;
    }
    if !matches!(args.until.as_str(), "timeout" | "line" | "sequence") {
        return Err(ToolError::invalid_params(
            "until must be timeout, line, or sequence",
        ));
    }
    match (args.until.as_str(), args.terminator.as_deref()) {
        ("sequence", Some(terminator)) => validate_terminator(terminator)?,
        ("sequence", None) => {
            return Err(ToolError::invalid_params(
                "terminator is required when until is sequence",
            ));
        }
        (_, Some(_)) => {
            return Err(ToolError::invalid_params(
                "terminator is only valid when until is sequence",
            ));
        }
        _ => {}
    }
    Ok(args)
}

fn validate_run_command_args(args: RunCommandArgs) -> Result<RunCommandArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    validate_line_ending(&args.line_ending)?;
    if args.command.is_empty() && args.line_ending == "none" {
        return Err(ToolError::invalid_params(
            "command and line ending must not both be empty",
        ));
    }
    if args.command.len() > 65_536 {
        return Err(ToolError::invalid_params(
            "command must not exceed 65536 bytes",
        ));
    }
    validate_timeout(args.timeout_ms)?;
    validate_timeout(args.idle_timeout_ms)?;
    if args.idle_timeout_ms > args.timeout_ms {
        return Err(ToolError::invalid_params(
            "idle_timeout_ms must not exceed timeout_ms",
        ));
    }
    validate_max_bytes(args.max_bytes)?;
    if let Some(until) = args.until.as_deref() {
        validate_terminator(until)?;
    }
    Ok(args)
}

fn validate_flush_args(args: FlushArgs) -> Result<FlushArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    if !matches!(args.direction.as_str(), "input" | "output" | "both") {
        return Err(ToolError::invalid_params(
            "direction must be input, output, or both",
        ));
    }
    Ok(args)
}

fn validate_set_control_args(args: SetControlArgs) -> Result<SetControlArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    if args.dtr.is_none() && args.rts.is_none() && args.break_ms.is_none() {
        return Err(ToolError::invalid_params(
            "at least one of dtr, rts, or break_ms is required",
        ));
    }
    if let Some(value) = args.break_ms
        && value > 10_000
    {
        return Err(ToolError::invalid_params(
            "break_ms must be between 0 and 10000",
        ));
    }
    Ok(args)
}

fn validate_send_file_args(args: FileTransferArgs) -> Result<FileTransferArgs, ToolError> {
    validate_file_transfer_args(&args)?;
    if args.start_command
        && args.command.is_none()
        && matches!(args.protocol.as_str(), "xmodem" | "xmodem-1k")
        && args.remote_path.as_deref().is_none_or(str::is_empty)
    {
        return Err(ToolError::invalid_params(
            "remote_path is required for an automatic XMODEM receive command",
        ));
    }
    Ok(args)
}

fn validate_receive_file_args(args: FileTransferArgs) -> Result<FileTransferArgs, ToolError> {
    validate_file_transfer_args(&args)?;
    if args.start_command
        && args.command.is_none()
        && args.remote_path.as_deref().is_none_or(str::is_empty)
    {
        return Err(ToolError::invalid_params(
            "remote_path is required for an automatic send command",
        ));
    }
    Ok(args)
}

fn validate_file_transfer_args(args: &FileTransferArgs) -> Result<(), ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    if args.local_path.trim().is_empty() {
        return Err(ToolError::invalid_params("local_path must not be empty"));
    }
    if args.local_path.contains('\0')
        || args
            .remote_path
            .as_ref()
            .is_some_and(|path| path.contains('\0'))
    {
        return Err(ToolError::invalid_params("file paths must not contain NUL"));
    }
    if let Some(remote_path) = args.remote_path.as_deref()
        && remote_path.trim().is_empty()
    {
        return Err(ToolError::invalid_params("remote_path must not be empty"));
    }
    if !matches!(
        args.protocol.as_str(),
        "xmodem" | "xmodem-1k" | "ymodem" | "zmodem"
    ) {
        return Err(ToolError::invalid_params(
            "protocol must be xmodem, xmodem-1k, ymodem, or zmodem",
        ));
    }
    if args.shell_command == Some(true) && !args.start_command {
        return Err(ToolError::invalid_params(
            "shell_command=true requires start_command=true",
        ));
    }
    if let Some(command) = args.command.as_deref() {
        if command.contains('\0') {
            return Err(ToolError::invalid_params("command must not contain NUL"));
        }
        if command.trim().is_empty() {
            return Err(ToolError::invalid_params("command must not be empty"));
        }
        if !args.start_command {
            return Err(ToolError::invalid_params(
                "command cannot be used when start_command is false",
            ));
        }
    }
    validate_timeout(args.timeout_ms)
}

fn validate_modem_tool_check_args(
    args: ModemToolCheckArgs,
) -> Result<ModemToolCheckArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    validate_timeout(args.timeout_ms)?;
    Ok(args)
}

fn transfer_command(args: &FileTransferArgs, sending: bool) -> Result<Option<String>, ToolError> {
    if !args.start_command {
        return Ok(None);
    }
    if let Some(command) = args.command.as_ref() {
        return Ok(Some(command.clone()));
    }
    if sending {
        Ok(Some(match args.protocol.as_str() {
            "xmodem" | "xmodem-1k" => modem_command(
                "lrz",
                &format!(
                    "-X -c -y {}",
                    remote_file_arg(args.remote_path.as_deref().unwrap_or_default())
                ),
                "rx",
                &remote_file_arg(args.remote_path.as_deref().unwrap_or_default()),
            ),
            "ymodem" => modem_command("lrz", "--ymodem -y", "rb", ""),
            "zmodem" => modem_command("lrz", "-y", "rz", "-y"),
            _ => return Err(ToolError::invalid_params("unsupported modem protocol")),
        }))
    } else {
        let remote_path = remote_file_arg(args.remote_path.as_deref().unwrap_or_default());
        Ok(Some(match args.protocol.as_str() {
            "xmodem" => modem_command("lsz", &format!("-X {remote_path}"), "sx", &remote_path),
            "xmodem-1k" => modem_command(
                "lsz",
                &format!("-X -k {remote_path}"),
                "sx",
                &format!("-k {remote_path}"),
            ),
            "ymodem" => modem_command(
                "lsz",
                &format!("--ymodem {remote_path}"),
                "sb",
                &remote_path,
            ),
            "zmodem" => modem_command("lsz", &remote_path, "sz", &remote_path),
            _ => return Err(ToolError::invalid_params("unsupported modem protocol")),
        }))
    }
}

fn modem_command(
    preferred: &str,
    preferred_args: &str,
    _fallback: &str,
    _fallback_args: &str,
) -> String {
    format!("{preferred} {preferred_args}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_file_arg(path: &str) -> String {
    // Shell quoting does not stop getopt interpreting a filename as an option.
    // A relative ./ prefix also keeps a literal '-' from meaning standard input.
    if path.starts_with('-') {
        shell_quote(&format!("./{path}"))
    } else {
        shell_quote(path)
    }
}

fn validate_port_string(port: &str) -> Result<(), ToolError> {
    if port.trim().is_empty() {
        Err(ToolError::invalid_params("port must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_baud(value: u32) -> Result<(), ToolError> {
    if value == 0 || value > 4_000_000 {
        Err(ToolError::invalid_params(
            "baud_rate must be between 1 and 4000000",
        ))
    } else {
        Ok(())
    }
}

fn validate_data_bits(value: u8) -> Result<(), ToolError> {
    if !(5..=8).contains(&value) {
        Err(ToolError::invalid_params("data_bits must be 5, 6, 7, or 8"))
    } else {
        Ok(())
    }
}

fn validate_stop_bits(value: u8) -> Result<(), ToolError> {
    if !matches!(value, 1 | 2) {
        Err(ToolError::invalid_params("stop_bits must be 1 or 2"))
    } else {
        Ok(())
    }
}

fn validate_parity(value: &str) -> Result<(), ToolError> {
    if matches!(value.to_ascii_lowercase().as_str(), "none" | "odd" | "even") {
        Ok(())
    } else {
        Err(ToolError::invalid_params(
            "parity must be none, odd, or even",
        ))
    }
}

fn validate_flow_control(value: &str) -> Result<(), ToolError> {
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "software" | "hardware"
    ) {
        Ok(())
    } else {
        Err(ToolError::invalid_params(
            "flow_control must be none, software, or hardware",
        ))
    }
}

fn validate_timeout(value: u64) -> Result<(), ToolError> {
    if value == 0 || value > 600_000 {
        Err(ToolError::invalid_params(
            "timeout_ms must be between 1 and 600000",
        ))
    } else {
        Ok(())
    }
}

fn validate_encoding(value: &str) -> Result<(), ToolError> {
    if matches!(value, "text" | "hex") {
        Ok(())
    } else {
        Err(ToolError::invalid_params("encoding must be text or hex"))
    }
}

fn validate_line_ending(value: &str) -> Result<(), ToolError> {
    if matches!(value, "none" | "lf" | "cr" | "crlf") {
        Ok(())
    } else {
        Err(ToolError::invalid_params(
            "line_ending must be none, lf, cr, or crlf",
        ))
    }
}

fn validate_max_bytes(value: usize) -> Result<(), ToolError> {
    if value == 0 || value > 1_048_576 {
        Err(ToolError::invalid_params(
            "max_bytes must be between 1 and 1048576",
        ))
    } else {
        Ok(())
    }
}

fn validate_terminator(value: &str) -> Result<(), ToolError> {
    if value.is_empty() || value.len() > 1_024 {
        Err(ToolError::invalid_params(
            "terminator must contain between 1 and 1024 bytes",
        ))
    } else {
        Ok(())
    }
}

fn encode_data(data: &str, encoding: &str) -> Result<Vec<u8>, ToolError> {
    match encoding {
        "text" => Ok(data.as_bytes().to_vec()),
        "hex" => parse_hex(data).map_err(ToolError::invalid_params),
        _ => Err(ToolError::invalid_params("encoding must be text or hex")),
    }
}

fn append_line_ending(data: &mut Vec<u8>, line_ending: &str) {
    match line_ending {
        "lf" => data.push(b'\n'),
        "cr" => data.push(b'\r'),
        "crlf" => data.extend_from_slice(b"\r\n"),
        _ => {}
    }
}

fn parse_hex(data: &str) -> Result<Vec<u8>, String> {
    let compact: String = data.chars().filter(|ch| !ch.is_whitespace()).collect();
    let compact = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
        .unwrap_or(&compact);
    if compact.is_empty() {
        return Err("hex data must not be empty".to_string());
    }
    if !compact.is_ascii() {
        return Err("hex data must contain only ASCII hexadecimal digits".to_string());
    }
    if !compact.len().is_multiple_of(2) {
        return Err("hex data must contain an even number of digits".to_string());
    }
    let mut out = Vec::with_capacity(compact.len() / 2);
    for index in (0..compact.len()).step_by(2) {
        let byte = u8::from_str_radix(&compact[index..index + 2], 16)
            .map_err(|_| format!("invalid hex byte '{}'", &compact[index..index + 2]))?;
        out.push(byte);
    }
    Ok(out)
}

pub(crate) fn tool_names() -> &'static [&'static str] {
    &[
        "serial_list_ports",
        "serial_status",
        "serial_open",
        "serial_reconnect",
        "serial_close",
        "serial_configure",
        "serial_select",
        "serial_exchange",
        "serial_send_key",
        "serial_exec",
        "serial_get_output",
        "serial_cancel",
        "serial_write",
        "serial_read",
        "serial_run_command",
        "serial_check_modem_tools",
        "serial_send_file",
        "serial_receive_file",
        "serial_flush",
        "serial_set_control",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serial::SerialHub;

    fn message(method: &str, id: Option<i64>, params: Value) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("jsonrpc".to_string(), json!("2.0"));
        if let Some(id) = id {
            map.insert("id".to_string(), json!(id));
        }
        map.insert("method".to_string(), json!(method));
        if !params.is_null() {
            map.insert("params".to_string(), params);
        }
        Value::Object(map)
    }

    #[test]
    fn initialize_returns_server_info() {
        let mut hub = SerialHub::new();
        let response = handle_message(
            message(
                "initialize",
                Some(1),
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "codex", "version": "1.0"}
                }),
            ),
            &mut hub,
        )
        .expect("initialize should respond");

        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(response["result"]["serverInfo"]["name"], "serial-mcp");
    }

    #[test]
    fn initialize_negotiates_supported_older_version() {
        let mut hub = SerialHub::new();
        let response = handle_message(
            message(
                "initialize",
                Some(1),
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {"name": "codex", "version": "1.0"}
                }),
            ),
            &mut hub,
        )
        .unwrap();

        assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
    }

    #[test]
    fn notification_has_no_response() {
        let mut hub = SerialHub::new();
        let response = handle_message(
            message("notifications/initialized", None, json!({})),
            &mut hub,
        );
        assert!(response.is_none());
    }

    #[test]
    fn tools_list_contains_serial_tools() {
        let mut hub = SerialHub::new();
        let response = handle_message(message("tools/list", Some(1), json!({})), &mut hub).unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"serial_open"));
        assert!(names.contains(&"serial_reconnect"));
        assert!(names.contains(&"serial_write"));
        assert!(names.contains(&"serial_read"));
        assert!(names.contains(&"serial_run_command"));
        assert!(names.contains(&"serial_check_modem_tools"));
        assert!(names.contains(&"serial_send_file"));
        assert!(names.contains(&"serial_receive_file"));
    }

    #[test]
    fn hex_parser_accepts_spaces_and_prefix() {
        assert_eq!(parse_hex("0x48 65 6C 6C 6F").unwrap(), b"Hello");
        assert_eq!(parse_hex("0001FF").unwrap(), vec![0, 1, 255]);
        assert!(parse_hex("0").is_err());
        assert!(parse_hex("GG").is_err());
        assert!(parse_hex("你好").is_err());
    }

    #[test]
    fn schemas_cover_dispatch_and_share_output_options() {
        let definitions = tool_definitions();
        let names: std::collections::BTreeSet<_> = definitions
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), definitions.len());
        assert_eq!(names, tool_names().iter().copied().collect());
        for tool in definitions {
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(
                tool["inputSchema"]["properties"]["response_mode"]["default"],
                "compact"
            );
            assert_eq!(
                tool["inputSchema"]["properties"]["output_limit"]["default"],
                output::DEFAULT_OUTPUT_LIMIT
            );
            for required in tool["inputSchema"]["required"]
                .as_array()
                .into_iter()
                .flatten()
            {
                assert!(
                    tool["inputSchema"]["properties"]
                        .get(required.as_str().unwrap())
                        .is_some()
                );
            }
        }
    }

    #[test]
    fn invalid_arguments_fail_without_a_device() {
        let invalid = [
            ("serial_send_file", json!({"local_path":"bad\u{0}file.bin"})),
            (
                "serial_receive_file",
                json!({"local_path":"local.bin","remote_path":"a\u{0}b"}),
            ),
            (
                "serial_send_file",
                json!({"local_path":"local.bin","command":"rz\u{0}"}),
            ),
            ("serial_write", json!({"data":"GG","encoding":"hex"})),
            ("serial_write", json!({"data":"AA","encoding":"base64"})),
            ("serial_read", json!({"until":"sequence"})),
            ("serial_read", json!({"timeout_ms":1,"idle_timeout_ms":2})),
            ("serial_read", json!({"max_bytes":0})),
            ("serial_open", json!({"port":"COM3","profile":"linux"})),
            ("serial_configure", json!({})),
            ("serial_send_key", json!({"key":"unknown"})),
            ("serial_get_output", json!({"output_id":"none","limit":1})),
            ("serial_status", json!({"response_mode":"verbose"})),
            ("serial_status", json!({"output_limit":3})),
            ("serial_exec", json!({"command":"\u{0}"})),
        ];
        for (name, arguments) in invalid {
            let response = handle_message(
                message(
                    "tools/call",
                    Some(1),
                    json!({"name":name,"arguments":arguments}),
                ),
                &mut SerialHub::new(),
            )
            .unwrap();
            assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
        }
        let response = handle_message(
            json!({"jsonrpc":"2.0","id":[],"method":"tools/list"}),
            &mut SerialHub::new(),
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert!(response["id"].is_null());
    }

    #[test]
    fn transfer_commands_use_protocol_defaults_and_quote_paths() {
        let args = FileTransferArgs {
            port: None,
            protocol: "zmodem".to_string(),
            local_path: "C:\\firmware.bin".to_string(),
            remote_path: Some("/tmp/it's.bin".to_string()),
            command: None,
            start_command: true,
            shell_command: None,
            timeout_ms: 10_000,
        };
        let receive_command = transfer_command(&args, true).unwrap().unwrap();
        assert_eq!(receive_command, "lrz -y");

        let send_command = transfer_command(&args, false).unwrap().unwrap();
        assert_eq!(send_command, "lsz '/tmp/it'\\''s.bin'");
    }

    #[test]
    fn generated_file_commands_do_not_interpret_leading_dash_as_option() {
        for protocol in ["xmodem", "xmodem-1k", "ymodem", "zmodem"] {
            for path in ["-leading.bin", "-", "--help", "-it's 中文.bin"] {
                let args: FileTransferArgs = serde_json::from_value(
                    json!({"local_path":"fixture.bin", "remote_path":path, "protocol":protocol}),
                )
                .unwrap();
                let receive = transfer_command(&args, false).unwrap().unwrap();
                assert!(receive.ends_with(&shell_quote(&format!("./{path}"))));
                if protocol.starts_with("xmodem") {
                    assert!(
                        transfer_command(&args, true)
                            .unwrap()
                            .unwrap()
                            .ends_with(&shell_quote(&format!("./{path}")))
                    );
                }
            }
        }
    }

    #[test]
    fn transfer_paths_and_commands_reject_unrepresentable_nul() {
        for arguments in [
            json!({"local_path":"bad\u{0}file.bin"}),
            json!({"local_path":"local.bin","remote_path":"a\u{0}b"}),
            json!({"local_path":"local.bin","command":"rz\u{0}"}),
        ] {
            let args: FileTransferArgs = serde_json::from_value(arguments).unwrap();
            assert!(validate_file_transfer_args(&args).is_err());
        }
    }

    #[test]
    fn modem_tool_check_defaults_and_probe_parser() {
        let args = ModemToolCheckArgs {
            port: None,
            timeout_ms: 10_000,
        };
        assert!(validate_modem_tool_check_args(args).is_ok());
        let tools = crate::serial::parse_modem_tool_probe(b"lrz=1\nlsz=0\nrz=1\n");
        assert!(tools["lrz"]);
        assert!(!tools["lsz"]);
        assert!(tools["rz"]);
        assert!(!tools["sz"]);
    }

    #[test]
    fn receive_transfer_requires_remote_path_only_for_automatic_command() {
        let missing = FileTransferArgs {
            port: None,
            protocol: "zmodem".to_string(),
            local_path: "firmware.bin".to_string(),
            remote_path: None,
            command: None,
            start_command: true,
            shell_command: None,
            timeout_ms: 10_000,
        };
        assert!(validate_receive_file_args(missing).is_err());

        let manual = FileTransferArgs {
            port: None,
            protocol: "zmodem".to_string(),
            local_path: "firmware.bin".to_string(),
            remote_path: None,
            command: None,
            start_command: false,
            shell_command: None,
            timeout_ms: 10_000,
        };
        assert!(validate_receive_file_args(manual).is_ok());
    }
}
