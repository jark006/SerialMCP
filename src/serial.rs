use serde::Serialize;
use serialport::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortType, StopBits,
};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::time::Duration;

pub const DEFAULT_BAUD_RATE: u32 = 115_200;
pub const DEFAULT_DATA_BITS: u8 = 8;
pub const DEFAULT_STOP_BITS: u8 = 1;
pub const DEFAULT_PARITY: &str = "none";
pub const DEFAULT_FLOW_CONTROL: &str = "none";
pub const DEFAULT_TIMEOUT_MS: u64 = 1_000;

#[derive(Debug, Clone, Serialize)]
pub struct PortDescription {
    pub port: String,
    pub description: String,
    pub port_type: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortStatus {
    pub port: String,
    pub description: String,
    pub port_type: String,
    pub vid: Option<String>,
    pub pid: Option<String>,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub open: bool,
    pub active: bool,
    pub baud_rate: Option<u32>,
    pub data_bits: Option<u8>,
    pub stop_bits: Option<u8>,
    pub parity: Option<String>,
    pub flow_control: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResponse {
    pub active_port: Option<String>,
    pub ports: Vec<PortStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteResult {
    pub port: String,
    pub bytes_written: usize,
    pub encoding: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadResult {
    pub port: String,
    pub bytes_read: usize,
    pub data: String,
    pub hex: String,
    pub encoding: String,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlResult {
    pub port: String,
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
    pub break_sent_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PortSettings {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: String,
    pub flow_control: String,
    pub timeout_ms: u64,
}

impl Default for PortSettings {
    fn default() -> Self {
        Self {
            baud_rate: DEFAULT_BAUD_RATE,
            data_bits: DEFAULT_DATA_BITS,
            stop_bits: DEFAULT_STOP_BITS,
            parity: DEFAULT_PARITY.to_string(),
            flow_control: DEFAULT_FLOW_CONTROL.to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

#[derive(Debug)]
struct OpenPort {
    handle: Box<dyn SerialPort>,
    description: String,
    port_type: String,
    vid: Option<String>,
    pid: Option<String>,
    serial_number: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
    settings: PortSettings,
}

pub struct SerialHub {
    ports: BTreeMap<String, OpenPort>,
    active: Option<String>,
}

impl SerialHub {
    pub fn new() -> Self {
        Self {
            ports: BTreeMap::new(),
            active: None,
        }
    }

    pub fn list_ports() -> Result<Vec<PortDescription>, String> {
        let ports = serialport::available_ports()
            .map_err(|error| format!("failed to enumerate COM ports: {error}"))?;
        let mut result = Vec::new();
        for info in ports {
            let (port_type, vid, pid, serial_number, manufacturer, product) = match info.port_type {
                SerialPortType::UsbPort(usb) => (
                    "usb".to_string(),
                    vid_pid(usb.vid),
                    vid_pid(usb.pid),
                    usb.serial_number,
                    usb.manufacturer,
                    usb.product,
                ),
                SerialPortType::PciPort => ("pci".to_string(), None, None, None, None, None),
                SerialPortType::BluetoothPort => {
                    ("bluetooth".to_string(), None, None, None, None, None)
                }
                SerialPortType::Unknown => ("unknown".to_string(), None, None, None, None, None),
            };
            let description =
                describe_port(&port_type, manufacturer.as_deref(), product.as_deref());
            result.push(PortDescription {
                port: info.port_name,
                description,
                port_type,
                vid,
                pid,
                serial_number,
                manufacturer,
                product,
            });
        }
        Ok(result)
    }

    pub fn open_port(&mut self, port: &str, settings: PortSettings) -> Result<PortStatus, String> {
        let normalized = normalize_port(port);
        if normalized.is_empty() {
            return Err("port must not be empty".to_string());
        }
        if self.ports.contains_key(&normalized) {
            return Err(format!("{port} is already open; close it first"));
        }

        let builder = serialport::new(port, settings.baud_rate)
            .data_bits(parse_data_bits(settings.data_bits)?)
            .stop_bits(parse_stop_bits(settings.stop_bits)?)
            .parity(parse_parity(&settings.parity)?)
            .flow_control(parse_flow_control(&settings.flow_control)?)
            .timeout(Duration::from_millis(settings.timeout_ms));

        let handle = builder
            .open()
            .map_err(|error| format!("failed to open {port}: {error}"))?;

        let metadata = Self::find_port_metadata(port);
        self.ports.insert(
            normalized.clone(),
            OpenPort {
                handle,
                description: metadata.description,
                port_type: metadata.port_type,
                vid: metadata.vid,
                pid: metadata.pid,
                serial_number: metadata.serial_number,
                manufacturer: metadata.manufacturer,
                product: metadata.product,
                settings,
            },
        );
        self.active = Some(normalized.clone());
        self.status_for(&normalized)
            .ok_or_else(|| "opened port vanished".to_string())
    }

    pub fn close_port(&mut self, port: Option<&str>) -> Result<PortStatus, String> {
        let normalized = self.resolve_port(port)?;
        let removed = self
            .ports
            .remove(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;
        if self.active.as_deref() == Some(normalized.as_str()) {
            self.active = None;
        }
        Ok(status_from_open_port_ref(&normalized, &removed, false))
    }

    pub fn configure_port(
        &mut self,
        port: Option<&str>,
        settings: PartialSettings,
    ) -> Result<PortStatus, String> {
        let normalized = self.resolve_port(port)?;
        let entry = self
            .ports
            .get_mut(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;

        if let Some(value) = settings.baud_rate {
            entry
                .handle
                .set_baud_rate(value)
                .map_err(|error| format!("failed to set baud rate: {error}"))?;
            entry.settings.baud_rate = value;
        }
        if let Some(value) = settings.data_bits {
            entry
                .handle
                .set_data_bits(parse_data_bits(value)?)
                .map_err(|error| format!("failed to set data bits: {error}"))?;
            entry.settings.data_bits = value;
        }
        if let Some(value) = settings.stop_bits {
            entry
                .handle
                .set_stop_bits(parse_stop_bits(value)?)
                .map_err(|error| format!("failed to set stop bits: {error}"))?;
            entry.settings.stop_bits = value;
        }
        if let Some(value) = settings.parity.as_deref() {
            entry
                .handle
                .set_parity(parse_parity(value)?)
                .map_err(|error| format!("failed to set parity: {error}"))?;
            entry.settings.parity = value.to_string();
        }
        if let Some(value) = settings.flow_control.as_deref() {
            entry
                .handle
                .set_flow_control(parse_flow_control(value)?)
                .map_err(|error| format!("failed to set flow control: {error}"))?;
            entry.settings.flow_control = value.to_string();
        }
        if let Some(value) = settings.timeout_ms {
            entry
                .handle
                .set_timeout(Duration::from_millis(value))
                .map_err(|error| format!("failed to set read timeout: {error}"))?;
            entry.settings.timeout_ms = value;
        }

        self.status_for(&normalized)
            .ok_or_else(|| "configured port vanished".to_string())
    }

    pub fn write_bytes(
        &mut self,
        port: Option<&str>,
        data: &[u8],
        encoding: &str,
    ) -> Result<WriteResult, String> {
        let normalized = self.resolve_port(port)?;
        let entry = self
            .ports
            .get_mut(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;
        if data.is_empty() {
            return Err("data must not be empty".to_string());
        }
        let bytes_written = entry
            .handle
            .write_all(data)
            .map(|()| data.len())
            .map_err(|error| format!("failed to write to {normalized}: {error}"))?;
        Ok(WriteResult {
            port: normalized,
            bytes_written,
            encoding: encoding.to_string(),
        })
    }

    pub fn read_bytes(
        &mut self,
        port: Option<&str>,
        timeout_ms: u64,
        max_bytes: usize,
        encoding: &str,
        until: &str,
    ) -> Result<ReadResult, String> {
        let normalized = self.resolve_port(port)?;
        let entry = self
            .ports
            .get_mut(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;

        entry
            .handle
            .set_timeout(Duration::from_millis(timeout_ms))
            .map_err(|error| format!("failed to set read timeout: {error}"))?;

        let mut collected = Vec::new();
        let mut timed_out = false;
        let mut last_err: Option<std::io::Error> = None;

        while collected.len() < max_bytes {
            let mut buf = [0u8; 1024];
            let want = (max_bytes - collected.len()).min(buf.len());
            match entry.handle.read(&mut buf[..want]) {
                Ok(0) => {
                    timed_out = true;
                    break;
                }
                Ok(n) => {
                    collected.extend_from_slice(&buf[..n]);
                    if until == "line" && contains_line_ending(&collected) {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    timed_out = true;
                    break;
                }
                Err(error) => {
                    last_err = Some(error);
                    break;
                }
            }
        }

        if let Some(error) = last_err {
            return Err(format!("failed to read from {normalized}: {error}"));
        }

        let data = match encoding {
            "hex" => to_hex(&collected),
            _ => String::from_utf8_lossy(&collected).into_owned(),
        };
        let hex = to_hex(&collected);

        Ok(ReadResult {
            port: normalized,
            bytes_read: collected.len(),
            data,
            hex,
            encoding: encoding.to_string(),
            timed_out,
        })
    }

    pub fn flush(
        &mut self,
        port: Option<&str>,
        direction: &str,
    ) -> Result<ValueFlushResult, String> {
        let normalized = self.resolve_port(port)?;
        let entry = self
            .ports
            .get_mut(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;
        let buffer = match direction {
            "input" => ClearBuffer::Input,
            "output" => ClearBuffer::Output,
            _ => ClearBuffer::All,
        };
        entry
            .handle
            .clear(buffer)
            .map_err(|error| format!("failed to flush {normalized}: {error}"))?;
        Ok(ValueFlushResult {
            port: normalized,
            direction: direction.to_string(),
        })
    }

    pub fn set_control(
        &mut self,
        port: Option<&str>,
        dtr: Option<bool>,
        rts: Option<bool>,
        break_ms: Option<u64>,
    ) -> Result<ControlResult, String> {
        let normalized = self.resolve_port(port)?;
        let entry = self
            .ports
            .get_mut(&normalized)
            .ok_or_else(|| format!("{normalized} is not open"))?;

        if let Some(level) = dtr {
            entry
                .handle
                .write_data_terminal_ready(level)
                .map_err(|error| format!("failed to set DTR: {error}"))?;
        }
        if let Some(level) = rts {
            entry
                .handle
                .write_request_to_send(level)
                .map_err(|error| format!("failed to set RTS: {error}"))?;
        }
        let mut break_sent_ms = None;
        if let Some(millis) = break_ms {
            entry
                .handle
                .set_break()
                .map_err(|error| format!("failed to assert break: {error}"))?;
            std::thread::sleep(Duration::from_millis(millis));
            entry
                .handle
                .clear_break()
                .map_err(|error| format!("failed to clear break: {error}"))?;
            break_sent_ms = Some(millis);
        }

        Ok(ControlResult {
            port: normalized,
            dtr,
            rts,
            break_sent_ms,
        })
    }

    pub fn status(&self) -> StatusResponse {
        let mut ports = Vec::new();
        for (normalized, open) in &self.ports {
            ports.push(status_from_open_port_ref(
                normalized,
                open,
                self.active.as_deref() == Some(normalized.as_str()),
            ));
        }
        let listed = Self::list_ports().unwrap_or_default();
        for desc in listed {
            let key = normalize_port(&desc.port);
            if !self.ports.contains_key(&key) {
                ports.push(PortStatus {
                    port: desc.port,
                    description: desc.description,
                    port_type: desc.port_type,
                    vid: desc.vid,
                    pid: desc.pid,
                    serial_number: desc.serial_number,
                    manufacturer: desc.manufacturer,
                    product: desc.product,
                    open: false,
                    active: false,
                    baud_rate: None,
                    data_bits: None,
                    stop_bits: None,
                    parity: None,
                    flow_control: None,
                    timeout_ms: None,
                });
            }
        }
        ports.sort_by(|a, b| a.port.cmp(&b.port));
        StatusResponse {
            active_port: self.active.clone(),
            ports,
        }
    }

    fn resolve_port(&self, port: Option<&str>) -> Result<String, String> {
        if let Some(port) = port {
            let normalized = normalize_port(port);
            if normalized.is_empty() {
                return Err("port must not be empty".to_string());
            }
            Ok(normalized)
        } else {
            self.active
                .clone()
                .ok_or_else(|| "no port selected; open a port first or pass 'port'".to_string())
        }
    }

    fn status_for(&self, normalized: &str) -> Option<PortStatus> {
        self.ports.get(normalized).map(|open| {
            status_from_open_port_ref(normalized, open, self.active.as_deref() == Some(normalized))
        })
    }

    fn find_port_metadata(port: &str) -> PortMetadata {
        if let Ok(ports) = serialport::available_ports() {
            for info in ports {
                if normalize_port(&info.port_name) == normalize_port(port) {
                    return metadata_from_port_info(info);
                }
            }
        }
        PortMetadata::unknown()
    }
}

#[derive(Debug)]
struct PortMetadata {
    description: String,
    port_type: String,
    vid: Option<String>,
    pid: Option<String>,
    serial_number: Option<String>,
    manufacturer: Option<String>,
    product: Option<String>,
}

impl PortMetadata {
    fn unknown() -> Self {
        Self {
            description: "Serial port".to_string(),
            port_type: "unknown".to_string(),
            vid: None,
            pid: None,
            serial_number: None,
            manufacturer: None,
            product: None,
        }
    }
}

fn metadata_from_port_info(info: serialport::SerialPortInfo) -> PortMetadata {
    match info.port_type {
        SerialPortType::UsbPort(usb) => PortMetadata {
            description: describe_port("usb", usb.manufacturer.as_deref(), usb.product.as_deref()),
            port_type: "usb".to_string(),
            vid: vid_pid(usb.vid),
            pid: vid_pid(usb.pid),
            serial_number: usb.serial_number,
            manufacturer: usb.manufacturer,
            product: usb.product,
        },
        SerialPortType::PciPort => PortMetadata {
            description: "PCI serial port".to_string(),
            port_type: "pci".to_string(),
            ..PortMetadata::unknown()
        },
        SerialPortType::BluetoothPort => PortMetadata {
            description: "Bluetooth serial port".to_string(),
            port_type: "bluetooth".to_string(),
            ..PortMetadata::unknown()
        },
        SerialPortType::Unknown => PortMetadata {
            description: "Serial port".to_string(),
            port_type: "unknown".to_string(),
            ..PortMetadata::unknown()
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ValueFlushResult {
    pub port: String,
    pub direction: String,
}

fn status_from_open_port_ref(normalized: &str, open: &OpenPort, active: bool) -> PortStatus {
    PortStatus {
        port: normalized.to_string(),
        description: open.description.clone(),
        port_type: open.port_type.clone(),
        vid: open.vid.clone(),
        pid: open.pid.clone(),
        serial_number: open.serial_number.clone(),
        manufacturer: open.manufacturer.clone(),
        product: open.product.clone(),
        open: true,
        active,
        baud_rate: Some(open.settings.baud_rate),
        data_bits: Some(open.settings.data_bits),
        stop_bits: Some(open.settings.stop_bits),
        parity: Some(open.settings.parity.clone()),
        flow_control: Some(open.settings.flow_control.clone()),
        timeout_ms: Some(open.settings.timeout_ms),
    }
}

#[derive(Debug, Clone, Default)]
pub struct PartialSettings {
    pub baud_rate: Option<u32>,
    pub data_bits: Option<u8>,
    pub stop_bits: Option<u8>,
    pub parity: Option<String>,
    pub flow_control: Option<String>,
    pub timeout_ms: Option<u64>,
}

fn normalize_port(port: &str) -> String {
    port.trim().to_ascii_uppercase()
}

fn describe_port(port_type: &str, manufacturer: Option<&str>, product: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(value) = manufacturer.filter(|value| !value.trim().is_empty()) {
        parts.push(value.trim().to_string());
    }
    if let Some(value) = product.filter(|value| !value.trim().is_empty()) {
        parts.push(value.trim().to_string());
    }
    if parts.is_empty() {
        format!("{port_type} serial port")
    } else {
        parts.join(" - ")
    }
}

fn contains_line_ending(data: &[u8]) -> bool {
    data.iter().any(|byte| matches!(byte, b'\n' | b'\r'))
}

fn vid_pid(value: u16) -> Option<String> {
    if value == 0 {
        None
    } else {
        Some(format!("{value:04X}"))
    }
}

fn to_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 3);
    for (index, byte) in data.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{byte:02X}"));
    }
    out
}

fn parse_data_bits(value: u8) -> Result<DataBits, String> {
    match value {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err("data_bits must be 5, 6, 7, or 8".to_string()),
    }
}

fn parse_stop_bits(value: u8) -> Result<StopBits, String> {
    match value {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err("stop_bits must be 1 or 2".to_string()),
    }
}

fn parse_parity(value: &str) -> Result<Parity, String> {
    match value.to_ascii_lowercase().as_str() {
        "none" => Ok(Parity::None),
        "odd" => Ok(Parity::Odd),
        "even" => Ok(Parity::Even),
        _ => Err("parity must be none, odd, or even".to_string()),
    }
}

fn parse_flow_control(value: &str) -> Result<FlowControl, String> {
    match value.to_ascii_lowercase().as_str() {
        "none" => Ok(FlowControl::None),
        "software" | "xon_xoff" | "xon/xoff" => Ok(FlowControl::Software),
        "hardware" | "rts_cts" | "rts/cts" => Ok(FlowControl::Hardware),
        _ => Err("flow_control must be none, software, or hardware".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_starts_without_active_port() {
        let hub = SerialHub::new();
        assert_eq!(hub.status().active_port, None);
    }

    #[test]
    fn normalize_port_trims_and_uppercases() {
        assert_eq!(normalize_port(" com3 "), "COM3");
        assert_eq!(normalize_port("\\\\.\\COM10"), "\\\\.\\COM10");
    }

    #[test]
    fn vid_pid_formats_usb_ids() {
        assert_eq!(vid_pid(0x1234).as_deref(), Some("1234"));
        assert_eq!(vid_pid(0), None);
    }

    #[test]
    fn serial_parameter_parsers_validate() {
        assert!(parse_data_bits(8).is_ok());
        assert!(parse_data_bits(4).is_err());
        assert!(parse_stop_bits(2).is_ok());
        assert!(parse_parity("none").is_ok());
        assert!(parse_parity("mark").is_err());
        assert!(parse_flow_control("hardware").is_ok());
    }
}
