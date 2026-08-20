use crate::serial::{
    DEFAULT_BAUD_RATE, DEFAULT_DATA_BITS, DEFAULT_FLOW_CONTROL, DEFAULT_PARITY, DEFAULT_STOP_BITS,
    DEFAULT_TIMEOUT_MS, PartialSettings, PortSettings, SerialHub,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", MCP_PROTOCOL_VERSION];
pub const SERVER_NAME: &str = "serial-mcp";

#[derive(Debug)]
struct ToolError {
    kind: String,
    message: String,
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
    if message.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
        return Some(rpc_error(
            id.unwrap_or(Value::Null),
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
            "instructions": "Serial console for embedded Linux on Windows. Recommended workflow: serial_list_ports, serial_open, then serial_check_modem_tools or serial_send_file/serial_receive_file for atomic X/Y/ZMODEM transfers. After USB hot-plug, call serial_status and serial_reconnect. Use serial_read/serial_write for raw byte-level interaction."
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
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return rpc_error(id, -32602, "tool arguments must be an object");
    }
    if !tool_names().contains(&name) {
        return rpc_error(id, -32602, "unknown tool");
    }

    let result: Result<Value, ToolError> = match name {
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
                    },
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_write" => decode::<WriteArgs>(&arguments)
            .and_then(validate_write_args)
            .and_then(|args| {
                let encoding = args.encoding.clone();
                let mut data = encode_data(&args.data, &encoding)?;
                append_line_ending(&mut data, &args.line_ending);
                hub.write_bytes(args.port.as_deref(), &data, &encoding)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_read" => decode::<ReadArgs>(&arguments)
            .and_then(validate_read_args)
            .and_then(|args| {
                hub.read_bytes(
                    args.port.as_deref(),
                    args.timeout_ms,
                    args.max_bytes,
                    &args.encoding,
                    &args.until,
                    args.terminator.as_deref().map(str::as_bytes),
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_run_command" => decode::<RunCommandArgs>(&arguments)
            .and_then(validate_run_command_args)
            .and_then(|args| {
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
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_check_modem_tools" => decode::<ModemToolCheckArgs>(&arguments)
            .and_then(validate_modem_tool_check_args)
            .and_then(|args| {
                hub.check_modem_tools(args.port.as_deref(), args.timeout_ms)
                    .map(|value| json!(value))
                    .map_err(ToolError::tool)
            }),
        "serial_send_file" => decode::<FileTransferArgs>(&arguments)
            .and_then(validate_send_file_args)
            .and_then(|args| {
                let command = transfer_command(&args, true)?;
                hub.send_file(
                    args.port.as_deref(),
                    &args.protocol,
                    &args.local_path,
                    args.remote_path.as_deref(),
                    command.as_deref(),
                    args.timeout_ms,
                )
                .map(|value| json!(value))
                .map_err(ToolError::tool)
            }),
        "serial_receive_file" => decode::<FileTransferArgs>(&arguments)
            .and_then(validate_receive_file_args)
            .and_then(|args| {
                let command = transfer_command(&args, false)?;
                hub.receive_file(
                    args.port.as_deref(),
                    &args.protocol,
                    &args.local_path,
                    args.remote_path.as_deref(),
                    command.as_deref(),
                    args.timeout_ms,
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
        Ok(value) => rpc_result(id, successful_tool_result(name, value)),
        Err(error) if error.kind == "invalid_params" => rpc_error(id, -32602, &error.message),
        Err(error) => rpc_result(id, error_tool_result(&error.message)),
    }
}

fn successful_tool_result(name: &str, value: Value) -> Value {
    if matches!(name, "serial_read" | "serial_run_command") {
        let text = value
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or_default();
        json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": value,
            "isError": false
        })
    } else {
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string());
        json!({
            "content": [{"type": "text", "text": text}],
            "structuredContent": value,
            "isError": false
        })
    }
}

fn error_tool_result(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "structuredContent": {"error": message},
        "isError": true
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    port: Option<String>,
    data: String,
    #[serde(default = "default_encoding")]
    encoding: String,
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
    #[serde(default = "default_encoding")]
    encoding: String,
    #[serde(default = "default_until")]
    until: String,
    terminator: Option<String>,
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
    #[serde(default = "default_true")]
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
fn default_encoding() -> String {
    "text".to_string()
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
    validate_encoding(&args.encoding)?;
    validate_line_ending(&args.line_ending)?;
    Ok(args)
}

fn validate_read_args(args: ReadArgs) -> Result<ReadArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    validate_timeout(args.timeout_ms)?;
    validate_max_bytes(args.max_bytes)?;
    validate_encoding(&args.encoding)?;
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
    if let Some(command) = args.command.as_deref() {
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
                    shell_quote(args.remote_path.as_deref().unwrap_or_default())
                ),
                "rx",
                &shell_quote(args.remote_path.as_deref().unwrap_or_default()),
            ),
            "ymodem" => modem_command("lrz", "--ymodem -y", "rb", ""),
            "zmodem" => modem_command("lrz", "-y", "rz", "-y"),
            _ => return Err(ToolError::invalid_params("unsupported modem protocol")),
        }))
    } else {
        let remote_path = shell_quote(args.remote_path.as_deref().unwrap_or_default());
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

fn tool_names() -> &'static [&'static str] {
    &[
        "serial_list_ports",
        "serial_status",
        "serial_open",
        "serial_reconnect",
        "serial_close",
        "serial_configure",
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

fn tool_definitions() -> Vec<Value> {
    vec![
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
            "description": "Probe the embedded Linux shell for lrz/lsz and compatible rz/sz/rx/sx/rb/sb modem commands. The probe is atomic and returns a boolean map.",
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
            "description": "Atomically start a receiver on the embedded target and upload one local file over XMODEM, XMODEM-1K, YMODEM, or ZMODEM. Defaults to ZMODEM and selects lrz before falling back to rz. Set start_command=false only when the target receiver is already waiting.",
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
            "description": "Atomically start a sender on the embedded target and download one file to the Windows host over XMODEM, XMODEM-1K, YMODEM, or ZMODEM. Defaults to ZMODEM and selects lsz before falling back to sz. Set start_command=false only when the target sender is already running.",
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
    fn transfer_commands_use_protocol_defaults_and_quote_paths() {
        let args = FileTransferArgs {
            port: None,
            protocol: "zmodem".to_string(),
            local_path: "C:\\firmware.bin".to_string(),
            remote_path: Some("/tmp/it's.bin".to_string()),
            command: None,
            start_command: true,
            timeout_ms: 10_000,
        };
        let receive_command = transfer_command(&args, true).unwrap().unwrap();
        assert_eq!(receive_command, "lrz -y");

        let send_command = transfer_command(&args, false).unwrap().unwrap();
        assert_eq!(send_command, "lsz '/tmp/it'\\''s.bin'");
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
            timeout_ms: 10_000,
        };
        assert!(validate_receive_file_args(manual).is_ok());
    }
}
