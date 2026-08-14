use crate::serial::{
    DEFAULT_BAUD_RATE, DEFAULT_DATA_BITS, DEFAULT_FLOW_CONTROL, DEFAULT_PARITY, DEFAULT_STOP_BITS,
    DEFAULT_TIMEOUT_MS, PartialSettings, PortSettings, SerialHub,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SERVER_NAME: &str = "serial-mcp";

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
        return id.map(|id| rpc_error(id, -32600, "invalid JSON-RPC request"));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return id.map(|id| rpc_error(id, -32600, "method must be a string"));
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
    Some(rpc_result(
        id,
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Windows x64 serial-port console. Open a COM port, then read/write bytes for an embedded UART terminal."
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
                if args.append_newline {
                    data.push(b'\n');
                }
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
    if name == "serial_read" {
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
struct EmptyArgs {}

#[derive(Deserialize)]
struct PortArg {
    port: Option<String>,
}

#[derive(Deserialize)]
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
struct WriteArgs {
    port: Option<String>,
    data: String,
    #[serde(default = "default_encoding")]
    encoding: String,
    #[serde(default)]
    append_newline: bool,
}

#[derive(Deserialize)]
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
}

#[derive(Deserialize)]
struct FlushArgs {
    port: Option<String>,
    #[serde(default = "default_direction")]
    direction: String,
}

#[derive(Deserialize)]
struct SetControlArgs {
    port: Option<String>,
    dtr: Option<bool>,
    rts: Option<bool>,
    break_ms: Option<u64>,
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
fn default_max_bytes() -> usize {
    4096
}
fn default_direction() -> String {
    "both".to_string()
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
    Ok(args)
}

fn validate_port_arg(args: PortArg) -> Result<PortArg, ToolError> {
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
    Ok(args)
}

fn validate_read_args(args: ReadArgs) -> Result<ReadArgs, ToolError> {
    if let Some(port) = args.port.as_deref() {
        validate_port_string(port)?;
    }
    validate_timeout(args.timeout_ms)?;
    if args.max_bytes == 0 || args.max_bytes > 1_048_576 {
        return Err(ToolError::invalid_params(
            "max_bytes must be between 1 and 1048576",
        ));
    }
    validate_encoding(&args.encoding)?;
    if !matches!(args.until.as_str(), "timeout" | "line") {
        return Err(ToolError::invalid_params("until must be timeout or line"));
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

fn encode_data(data: &str, encoding: &str) -> Result<Vec<u8>, ToolError> {
    match encoding {
        "text" => Ok(data.as_bytes().to_vec()),
        "hex" => parse_hex(data).map_err(ToolError::invalid_params),
        _ => Err(ToolError::invalid_params("encoding must be text or hex")),
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
        "serial_open",
        "serial_close",
        "serial_configure",
        "serial_write",
        "serial_read",
        "serial_status",
        "serial_flush",
        "serial_set_control",
    ]
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "serial_list_ports",
            "description": "Enumerate available Windows COM ports with USB metadata (VID/PID, serial number, manufacturer, product).",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "serial_open",
            "description": "Open a COM port for an embedded UART console. Defaults to 115200 baud, 8 data bits, 1 stop bit, no parity, no flow control.",
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
                "required": ["port"]
            }
        }),
        json!({
            "name": "serial_close",
            "description": "Close an open COM port. If 'port' is omitted, closes the active port.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"}
                },
                "required": []
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
                "required": []
            }
        }),
        json!({
            "name": "serial_write",
            "description": "Write text or hex bytes to an open COM port. Text is the default for UART terminal input.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "data": {"type": "string", "description": "Payload; text by default, or hex with optional spaces/0x prefix"},
                    "encoding": {"type": "string", "enum": ["text", "hex"], "default": "text"},
                    "append_newline": {"type": "boolean", "default": false}
                },
                "required": ["data"]
            }
        }),
        json!({
            "name": "serial_read",
            "description": "Read bytes from an open COM port until timeout, a line ending, or max_bytes is reached. Returns decoded text (or hex) plus metadata.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "port": {"type": "string", "description": "COM port name; defaults to the active port"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 1000},
                    "max_bytes": {"type": "integer", "minimum": 1, "maximum": 1048576, "default": 4096},
                    "encoding": {"type": "string", "enum": ["text", "hex"], "default": "text"},
                    "until": {"type": "string", "enum": ["timeout", "line"], "default": "timeout"}
                },
                "required": []
            }
        }),
        json!({
            "name": "serial_status",
            "description": "Return the active port and current settings/open state for all discovered COM ports.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
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
                "required": []
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
                "required": []
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
        assert!(names.contains(&"serial_write"));
        assert!(names.contains(&"serial_read"));
    }

    #[test]
    fn hex_parser_accepts_spaces_and_prefix() {
        assert_eq!(parse_hex("0x48 65 6C 6C 6F").unwrap(), b"Hello");
        assert_eq!(parse_hex("0001FF").unwrap(), vec![0, 1, 255]);
        assert!(parse_hex("0").is_err());
        assert!(parse_hex("GG").is_err());
    }
}
