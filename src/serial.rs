use serde::Serialize;
use serialport::{
    ClearBuffer, DataBits, ErrorKind as SerialErrorKind, FlowControl, Parity, SerialPort,
    SerialPortInfo, SerialPortType, StopBits,
};
use std::collections::BTreeMap;
use std::io;
use std::time::{Duration, Instant};

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
    pub state: String,
    pub present: Option<bool>,
    pub open: bool,
    pub active: bool,
    pub last_error: Option<String>,
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
    pub enumeration_error: Option<String>,
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
    pub stop_reason: String,
    pub matched: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub port: String,
    pub bytes_written: usize,
    pub bytes_read: usize,
    pub data: String,
    pub raw_data: String,
    pub hex: String,
    pub stop_reason: String,
    pub matched: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ControlResult {
    pub port: String,
    pub dtr: Option<bool>,
    pub rts: Option<bool>,
    pub break_sent_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

struct TrackedPort {
    handle: Option<Box<dyn SerialPort>>,
    metadata: PortMetadata,
    settings: PortSettings,
    present: Option<bool>,
    last_error: Option<String>,
}

pub struct SerialHub {
    ports: BTreeMap<String, TrackedPort>,
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
        Ok(ports.into_iter().map(describe_port_info).collect())
    }

    pub fn open_port(&mut self, port: &str, settings: PortSettings) -> Result<PortStatus, String> {
        let requested = port.trim();
        let normalized = normalize_port(requested);
        if normalized.is_empty() {
            return Err("port must not be empty".to_string());
        }
        if let Some(existing) = self.ports.get(&normalized) {
            return if existing.handle.is_some() {
                Err(format!("{normalized} is already open; close it first"))
            } else {
                Err(format!(
                    "{normalized} is disconnected; use serial_reconnect or close it first"
                ))
            };
        }

        let handle = open_serial_port(requested, &settings)
            .map_err(|error| format!("failed to open {requested}: {error}"))?;
        let metadata = Self::find_port_metadata(requested)
            .ok()
            .flatten()
            .unwrap_or_else(PortMetadata::unknown);
        self.ports.insert(
            normalized.clone(),
            TrackedPort {
                handle: Some(handle),
                metadata,
                settings,
                present: Some(true),
                last_error: None,
            },
        );
        self.active = Some(normalized.clone());
        self.status_for(&normalized)
            .ok_or_else(|| "opened port vanished".to_string())
    }

    pub fn reconnect_port(
        &mut self,
        port: Option<&str>,
        force_identity: bool,
    ) -> Result<PortStatus, String> {
        let normalized = self.resolve_port(port)?;
        let (settings, old_metadata) = self
            .ports
            .get(&normalized)
            .map(|tracked| (tracked.settings.clone(), tracked.metadata.clone()))
            .ok_or_else(|| format!("{normalized} is not tracked; open it first"))?;

        let (candidate_port, metadata) =
            match Self::find_reconnect_candidate(&normalized, &old_metadata)? {
                Some(candidate) => candidate,
                None => {
                    self.mark_disconnected(&normalized, "device is not currently present");
                    return Err(format!(
                        "cannot reconnect {normalized}: device is not present"
                    ));
                }
            };
        if !force_identity && let Some(reason) = identity_mismatch(&old_metadata, &metadata) {
            return Err(format!(
                "refusing to reconnect {normalized}: {reason}; pass force=true to override"
            ));
        }

        let candidate_key = normalize_port(&candidate_port);
        if candidate_key != normalized && self.ports.contains_key(&candidate_key) {
            return Err(format!(
                "cannot reconnect {normalized} as {candidate_key}: destination port is already tracked"
            ));
        }
        let mut tracked = self
            .ports
            .remove(&normalized)
            .ok_or_else(|| format!("{normalized} is not tracked"))?;
        tracked.handle.take();
        let handle = match open_serial_port(&candidate_port, &settings) {
            Ok(handle) => handle,
            Err(error) => {
                let moved_suffix = if candidate_key != normalized {
                    format!(" as {candidate_key}")
                } else {
                    String::new()
                };
                let message = format!("failed to reconnect {normalized}{moved_suffix}: {error}");
                tracked.present = Some(candidate_key == normalized);
                tracked.last_error = Some(message.clone());
                self.ports.insert(normalized, tracked);
                return Err(message);
            }
        };
        tracked.handle = Some(handle);
        tracked.metadata = metadata;
        tracked.present = Some(true);
        tracked.last_error = None;
        self.ports.insert(candidate_key.clone(), tracked);
        self.active = Some(candidate_key.clone());
        self.status_for(&candidate_key)
            .ok_or_else(|| "reconnected port vanished".to_string())
    }

    pub fn close_port(&mut self, port: Option<&str>) -> Result<PortStatus, String> {
        let normalized = self.resolve_port(port)?;
        let removed = self
            .ports
            .remove(&normalized)
            .ok_or_else(|| format!("{normalized} is not tracked"))?;
        if self.active.as_deref() == Some(normalized.as_str()) {
            self.active = None;
        }
        let mut status = status_from_tracked(&normalized, &removed, false);
        status.state = "closed".to_string();
        status.open = false;
        Ok(status)
    }

    pub fn configure_port(
        &mut self,
        port: Option<&str>,
        settings: PartialSettings,
    ) -> Result<PortStatus, String> {
        let normalized = self.resolve_port(port)?;
        let data_bits = settings.data_bits.map(parse_data_bits).transpose()?;
        let stop_bits = settings.stop_bits.map(parse_stop_bits).transpose()?;
        let parity = settings.parity.as_deref().map(parse_parity).transpose()?;
        let flow_control = settings
            .flow_control
            .as_deref()
            .map(parse_flow_control)
            .transpose()?;

        if let Some(value) = settings.baud_rate {
            if let Err(error) = self.open_handle_mut(&normalized)?.set_baud_rate(value) {
                let message = format!("failed to set baud rate: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.baud_rate = value;
        }
        if let (Some(raw), Some(value)) = (settings.data_bits, data_bits) {
            if let Err(error) = self.open_handle_mut(&normalized)?.set_data_bits(value) {
                let message = format!("failed to set data bits: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.data_bits = raw;
        }
        if let (Some(raw), Some(value)) = (settings.stop_bits, stop_bits) {
            if let Err(error) = self.open_handle_mut(&normalized)?.set_stop_bits(value) {
                let message = format!("failed to set stop bits: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.stop_bits = raw;
        }
        if let (Some(raw), Some(value)) = (settings.parity.as_deref(), parity) {
            if let Err(error) = self.open_handle_mut(&normalized)?.set_parity(value) {
                let message = format!("failed to set parity: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.parity = raw.to_string();
        }
        if let (Some(raw), Some(value)) = (settings.flow_control.as_deref(), flow_control) {
            if let Err(error) = self.open_handle_mut(&normalized)?.set_flow_control(value) {
                let message = format!("failed to set flow control: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.flow_control = raw.to_string();
        }
        if let Some(value) = settings.timeout_ms {
            if let Err(error) = self
                .open_handle_mut(&normalized)?
                .set_timeout(Duration::from_millis(value))
            {
                let message = format!("failed to set read timeout: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            self.tracked_mut(&normalized)?.settings.timeout_ms = value;
        }

        self.mark_success(&normalized);
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
        if data.is_empty() {
            return Err("data must not be empty".to_string());
        }
        let result = {
            let handle = self.open_handle_mut(&normalized)?;
            handle.write_all(data).and_then(|()| handle.flush())
        };
        if let Err(error) = result {
            let message = format!("failed to write to {normalized}: {error}");
            return Err(self.record_io_failure(&normalized, message, &error));
        }
        self.mark_success(&normalized);
        Ok(WriteResult {
            port: normalized,
            bytes_written: data.len(),
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
        terminator: Option<&[u8]>,
    ) -> Result<ReadResult, String> {
        let normalized = self.resolve_port(port)?;
        let condition = match until {
            "line" => StopCondition::Line,
            "sequence" => StopCondition::Sequence(
                terminator.ok_or_else(|| "terminator is required for sequence mode".to_string())?,
            ),
            _ => StopCondition::Timeout,
        };
        let result = {
            let configured_timeout =
                Duration::from_millis(self.tracked_mut(&normalized)?.settings.timeout_ms);
            let handle = self.open_handle_mut(&normalized)?;
            let collected = collect_from_port(
                handle,
                Duration::from_millis(timeout_ms),
                None,
                max_bytes,
                condition,
            );
            let restored = handle
                .set_timeout(configured_timeout)
                .map_err(ReadFailure::Serial);
            match collected {
                Ok(value) => restored.map(|()| value),
                Err(error) => {
                    let _ = restored;
                    Err(error)
                }
            }
        };
        let collected = match result {
            Ok(collected) => collected,
            Err(ReadFailure::Serial(error)) => {
                let message = format!("failed to read from {normalized}: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            Err(ReadFailure::Io(error)) => {
                let message = format!("failed to read from {normalized}: {error}");
                return Err(self.record_io_failure(&normalized, message, &error));
            }
        };
        self.mark_success(&normalized);
        let data = decode_bytes(&collected.bytes, encoding);
        Ok(ReadResult {
            port: normalized,
            bytes_read: collected.bytes.len(),
            data,
            hex: to_hex(&collected.bytes),
            encoding: encoding.to_string(),
            timed_out: matches!(collected.stop_reason, "timeout" | "idle"),
            stop_reason: collected.stop_reason.to_string(),
            matched: collected.matched,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run_command(
        &mut self,
        port: Option<&str>,
        payload: &[u8],
        timeout_ms: u64,
        idle_timeout_ms: u64,
        max_bytes: usize,
        terminator: Option<&[u8]>,
        clear_input: bool,
        strip_ansi: bool,
    ) -> Result<CommandResult, String> {
        let normalized = self.resolve_port(port)?;
        let result: Result<CollectedRead, ReadFailure> = {
            let configured_timeout =
                Duration::from_millis(self.tracked_mut(&normalized)?.settings.timeout_ms);
            let handle = self.open_handle_mut(&normalized)?;
            (|| {
                if clear_input {
                    handle
                        .clear(ClearBuffer::Input)
                        .map_err(ReadFailure::Serial)?;
                }
                handle.write_all(payload).map_err(ReadFailure::Io)?;
                handle.flush().map_err(ReadFailure::Io)?;
                let condition = terminator
                    .map(StopCondition::Sequence)
                    .unwrap_or(StopCondition::Timeout);
                let idle_timeout = terminator
                    .is_none()
                    .then(|| Duration::from_millis(idle_timeout_ms));
                let collected = collect_from_port(
                    handle,
                    Duration::from_millis(timeout_ms),
                    idle_timeout,
                    max_bytes,
                    condition,
                );
                let restored = handle
                    .set_timeout(configured_timeout)
                    .map_err(ReadFailure::Serial);
                match collected {
                    Ok(value) => restored.map(|()| value),
                    Err(error) => {
                        let _ = restored;
                        Err(error)
                    }
                }
            })()
        };
        let collected = match result {
            Ok(collected) => collected,
            Err(ReadFailure::Serial(error)) => {
                let message = format!("serial command failed on {normalized}: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            Err(ReadFailure::Io(error)) => {
                let message = format!("serial command failed on {normalized}: {error}");
                return Err(self.record_io_failure(&normalized, message, &error));
            }
        };
        self.mark_success(&normalized);
        let raw_data = String::from_utf8_lossy(&collected.bytes).into_owned();
        let data = if strip_ansi {
            String::from_utf8_lossy(&strip_ansi_bytes(&collected.bytes)).into_owned()
        } else {
            raw_data.clone()
        };
        Ok(CommandResult {
            port: normalized,
            bytes_written: payload.len(),
            bytes_read: collected.bytes.len(),
            data,
            raw_data,
            hex: to_hex(&collected.bytes),
            stop_reason: collected.stop_reason.to_string(),
            matched: collected.matched,
        })
    }

    pub fn flush(
        &mut self,
        port: Option<&str>,
        direction: &str,
    ) -> Result<ValueFlushResult, String> {
        let normalized = self.resolve_port(port)?;
        let buffer = match direction {
            "input" => ClearBuffer::Input,
            "output" => ClearBuffer::Output,
            _ => ClearBuffer::All,
        };
        let result = {
            let handle = self.open_handle_mut(&normalized)?;
            handle.clear(buffer)
        };
        if let Err(error) = result {
            let message = format!("failed to flush {normalized}: {error}");
            return Err(self.record_serial_failure(&normalized, message, error.kind()));
        }
        self.mark_success(&normalized);
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
        if let Some(level) = dtr {
            let result = {
                let handle = self.open_handle_mut(&normalized)?;
                handle.write_data_terminal_ready(level)
            };
            if let Err(error) = result {
                let message = format!("failed to set DTR: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
        }
        if let Some(level) = rts {
            let result = {
                let handle = self.open_handle_mut(&normalized)?;
                handle.write_request_to_send(level)
            };
            if let Err(error) = result {
                let message = format!("failed to set RTS: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
        }
        let mut break_sent_ms = None;
        if let Some(millis) = break_ms {
            let result = {
                let handle = self.open_handle_mut(&normalized)?;
                handle.set_break()
            };
            if let Err(error) = result {
                let message = format!("failed to assert break: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            std::thread::sleep(Duration::from_millis(millis));
            let result = {
                let handle = self.open_handle_mut(&normalized)?;
                handle.clear_break()
            };
            if let Err(error) = result {
                let message = format!("failed to clear break: {error}");
                return Err(self.record_serial_failure(&normalized, message, error.kind()));
            }
            break_sent_ms = Some(millis);
        }

        self.mark_success(&normalized);
        Ok(ControlResult {
            port: normalized,
            dtr,
            rts,
            break_sent_ms,
        })
    }

    pub fn status(&mut self) -> StatusResponse {
        let (listed, enumeration_error) = match Self::list_ports() {
            Ok(ports) => (Some(ports), None),
            Err(error) => (None, Some(error)),
        };
        let mut available = BTreeMap::new();
        if let Some(listed) = listed {
            for description in listed {
                available.insert(normalize_port(&description.port), description);
            }
            for (name, tracked) in &mut self.ports {
                let present = available.contains_key(name);
                tracked.present = Some(present);
                if !present && tracked.handle.take().is_some() {
                    tracked.last_error = Some("device is no longer present".to_string());
                }
            }
        } else {
            for tracked in self.ports.values_mut() {
                tracked.present = None;
            }
        }

        let mut ports: Vec<_> = self
            .ports
            .iter()
            .map(|(name, tracked)| {
                status_from_tracked(name, tracked, self.active.as_deref() == Some(name))
            })
            .collect();
        for (name, description) in available {
            if !self.ports.contains_key(&name) {
                ports.push(status_from_available(description));
            }
        }
        ports.sort_by(|a, b| a.port.cmp(&b.port));
        StatusResponse {
            active_port: self.active.clone(),
            enumeration_error,
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

    fn tracked_mut(&mut self, normalized: &str) -> Result<&mut TrackedPort, String> {
        self.ports
            .get_mut(normalized)
            .ok_or_else(|| format!("{normalized} is not tracked; open it first"))
    }

    fn open_handle_mut(&mut self, normalized: &str) -> Result<&mut (dyn SerialPort + '_), String> {
        let tracked = self.tracked_mut(normalized)?;
        match tracked.handle.as_deref_mut() {
            Some(handle) => Ok(handle),
            None => Err(format!(
                "{normalized} is disconnected; call serial_reconnect after the device is present"
            )),
        }
    }

    fn status_for(&self, normalized: &str) -> Option<PortStatus> {
        self.ports.get(normalized).map(|tracked| {
            status_from_tracked(
                normalized,
                tracked,
                self.active.as_deref() == Some(normalized),
            )
        })
    }

    fn mark_success(&mut self, normalized: &str) {
        if let Some(tracked) = self.ports.get_mut(normalized) {
            tracked.present = Some(true);
            tracked.last_error = None;
        }
    }

    fn mark_disconnected(&mut self, normalized: &str, message: &str) {
        if let Some(tracked) = self.ports.get_mut(normalized) {
            tracked.handle.take();
            tracked.present = Some(false);
            tracked.last_error = Some(message.to_string());
        }
    }

    fn record_serial_failure(
        &mut self,
        normalized: &str,
        message: String,
        kind: SerialErrorKind,
    ) -> String {
        let disconnected = matches!(
            kind,
            SerialErrorKind::NoDevice
                | SerialErrorKind::Io(
                    io::ErrorKind::NotFound
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::UnexpectedEof
                )
        );
        self.record_failure(normalized, message, disconnected)
    }

    fn record_io_failure(
        &mut self,
        normalized: &str,
        message: String,
        error: &io::Error,
    ) -> String {
        let disconnected = matches!(
            error.kind(),
            io::ErrorKind::NotFound
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::UnexpectedEof
        );
        self.record_failure(normalized, message, disconnected)
    }

    fn record_failure(
        &mut self,
        normalized: &str,
        message: String,
        definitely_disconnected: bool,
    ) -> String {
        let presence = if definitely_disconnected {
            Some(false)
        } else {
            Self::find_port_metadata(normalized)
                .ok()
                .map(|metadata| metadata.is_some())
        };
        if let Some(tracked) = self.ports.get_mut(normalized) {
            tracked.last_error = Some(message.clone());
            if let Some(present) = presence {
                tracked.present = Some(present);
                if !present {
                    tracked.handle.take();
                }
            }
        }
        message
    }

    fn find_port_metadata(port: &str) -> Result<Option<PortMetadata>, String> {
        let normalized = normalize_port(port);
        let ports = serialport::available_ports()
            .map_err(|error| format!("failed to enumerate COM ports: {error}"))?;
        Ok(ports
            .into_iter()
            .find(|info| normalize_port(&info.port_name) == normalized)
            .map(metadata_from_port_info))
    }

    fn find_reconnect_candidate(
        port: &str,
        old_metadata: &PortMetadata,
    ) -> Result<Option<(String, PortMetadata)>, String> {
        let normalized = normalize_port(port);
        let ports = serialport::available_ports()
            .map_err(|error| format!("failed to enumerate COM ports: {error}"))?;
        let described: Vec<_> = ports
            .into_iter()
            .map(|info| {
                let name = info.port_name.clone();
                (name, metadata_from_port_info(info))
            })
            .collect();
        select_reconnect_candidate(&normalized, old_metadata, described)
    }
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Serialize)]
pub struct ValueFlushResult {
    pub port: String,
    pub direction: String,
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

#[derive(Clone, Copy)]
enum StopCondition<'a> {
    Timeout,
    Line,
    Sequence(&'a [u8]),
}

struct CollectedRead {
    bytes: Vec<u8>,
    stop_reason: &'static str,
    matched: bool,
}

enum ReadFailure {
    Serial(serialport::Error),
    Io(io::Error),
}

fn collect_from_port(
    handle: &mut dyn SerialPort,
    overall_timeout: Duration,
    idle_timeout: Option<Duration>,
    max_bytes: usize,
    condition: StopCondition<'_>,
) -> Result<CollectedRead, ReadFailure> {
    collect_with(
        |buffer, timeout| {
            handle.set_timeout(timeout).map_err(ReadFailure::Serial)?;
            handle.read(buffer).map_err(ReadFailure::Io)
        },
        overall_timeout,
        idle_timeout,
        max_bytes,
        condition,
    )
}

fn collect_with<F>(
    mut read_once: F,
    overall_timeout: Duration,
    idle_timeout: Option<Duration>,
    max_bytes: usize,
    condition: StopCondition<'_>,
) -> Result<CollectedRead, ReadFailure>
where
    F: FnMut(&mut [u8], Duration) -> Result<usize, ReadFailure>,
{
    let started = Instant::now();
    let overall_deadline = started + overall_timeout;
    let mut last_data_at = None;
    let mut collected = Vec::new();

    while collected.len() < max_bytes {
        let now = Instant::now();
        if now >= overall_deadline {
            return Ok(stopped(collected, "timeout", false));
        }
        let idle_deadline =
            last_data_at.and_then(|last: Instant| idle_timeout.map(|idle| last + idle));
        if idle_deadline.is_some_and(|deadline| now >= deadline) {
            return Ok(stopped(collected, "idle", false));
        }
        let (deadline, timeout_reason) = match idle_deadline {
            Some(deadline) if deadline < overall_deadline => (deadline, "idle"),
            _ => (overall_deadline, "timeout"),
        };
        let timeout = deadline
            .saturating_duration_since(now)
            .max(Duration::from_millis(1));
        let mut buffer = [0u8; 1024];
        let wanted = (max_bytes - collected.len()).min(buffer.len());
        match read_once(&mut buffer[..wanted], timeout) {
            Ok(0) => return Ok(stopped(collected, timeout_reason, false)),
            Ok(count) => {
                collected.extend_from_slice(&buffer[..count]);
                last_data_at = Some(Instant::now());
                match condition {
                    StopCondition::Line if contains_line_ending(&collected) => {
                        return Ok(stopped(collected, "line", true));
                    }
                    StopCondition::Sequence(sequence)
                        if contains_subsequence(&collected, sequence) =>
                    {
                        return Ok(stopped(collected, "sequence", true));
                    }
                    _ => {}
                }
            }
            Err(ReadFailure::Io(error)) if error.kind() == io::ErrorKind::TimedOut => {
                return Ok(stopped(collected, timeout_reason, false));
            }
            Err(ReadFailure::Io(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(stopped(collected, "max_bytes", false))
}

fn stopped(bytes: Vec<u8>, stop_reason: &'static str, matched: bool) -> CollectedRead {
    CollectedRead {
        bytes,
        stop_reason,
        matched,
    }
}

fn open_serial_port(port: &str, settings: &PortSettings) -> Result<Box<dyn SerialPort>, String> {
    let builder = serialport::new(port, settings.baud_rate)
        .data_bits(parse_data_bits(settings.data_bits)?)
        .stop_bits(parse_stop_bits(settings.stop_bits)?)
        .parity(parse_parity(&settings.parity)?)
        .flow_control(parse_flow_control(&settings.flow_control)?)
        .timeout(Duration::from_millis(settings.timeout_ms));
    builder.open().map_err(|error| error.to_string())
}

fn describe_port_info(info: SerialPortInfo) -> PortDescription {
    let port = info.port_name.clone();
    let metadata = metadata_from_port_info(info);
    PortDescription {
        port,
        description: metadata.description,
        port_type: metadata.port_type,
        vid: metadata.vid,
        pid: metadata.pid,
        serial_number: metadata.serial_number,
        manufacturer: metadata.manufacturer,
        product: metadata.product,
    }
}

fn metadata_from_port_info(info: SerialPortInfo) -> PortMetadata {
    match info.port_type {
        SerialPortType::UsbPort(usb) => PortMetadata {
            description: make_description(
                "usb",
                usb.manufacturer.as_deref(),
                usb.product.as_deref(),
            ),
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
        SerialPortType::Unknown => PortMetadata::unknown(),
    }
}

fn status_from_tracked(normalized: &str, tracked: &TrackedPort, active: bool) -> PortStatus {
    let open = tracked.handle.is_some();
    let state = if !open {
        "disconnected"
    } else if tracked.last_error.is_some() {
        "faulted"
    } else {
        "open"
    };
    PortStatus {
        port: normalized.to_string(),
        description: tracked.metadata.description.clone(),
        port_type: tracked.metadata.port_type.clone(),
        vid: tracked.metadata.vid.clone(),
        pid: tracked.metadata.pid.clone(),
        serial_number: tracked.metadata.serial_number.clone(),
        manufacturer: tracked.metadata.manufacturer.clone(),
        product: tracked.metadata.product.clone(),
        state: state.to_string(),
        present: tracked.present,
        open,
        active,
        last_error: tracked.last_error.clone(),
        baud_rate: Some(tracked.settings.baud_rate),
        data_bits: Some(tracked.settings.data_bits),
        stop_bits: Some(tracked.settings.stop_bits),
        parity: Some(tracked.settings.parity.clone()),
        flow_control: Some(tracked.settings.flow_control.clone()),
        timeout_ms: Some(tracked.settings.timeout_ms),
    }
}

fn status_from_available(description: PortDescription) -> PortStatus {
    PortStatus {
        port: description.port,
        description: description.description,
        port_type: description.port_type,
        vid: description.vid,
        pid: description.pid,
        serial_number: description.serial_number,
        manufacturer: description.manufacturer,
        product: description.product,
        state: "available".to_string(),
        present: Some(true),
        open: false,
        active: false,
        last_error: None,
        baud_rate: None,
        data_bits: None,
        stop_bits: None,
        parity: None,
        flow_control: None,
        timeout_ms: None,
    }
}

fn identity_mismatch(old: &PortMetadata, new: &PortMetadata) -> Option<String> {
    if let (Some(old_serial), Some(new_serial)) = (&old.serial_number, &new.serial_number)
        && old_serial != new_serial
    {
        return Some(format!(
            "USB serial number changed from {old_serial} to {new_serial}"
        ));
    }
    if let (Some(old_vid), Some(old_pid), Some(new_vid), Some(new_pid)) =
        (&old.vid, &old.pid, &new.vid, &new.pid)
        && (old_vid != new_vid || old_pid != new_pid)
    {
        return Some(format!(
            "USB identity changed from {old_vid}:{old_pid} to {new_vid}:{new_pid}"
        ));
    }
    None
}

fn select_reconnect_candidate(
    normalized: &str,
    old_metadata: &PortMetadata,
    candidates: Vec<(String, PortMetadata)>,
) -> Result<Option<(String, PortMetadata)>, String> {
    if let Some(serial_number) = old_metadata.serial_number.as_deref() {
        let matching: Vec<_> = candidates
            .iter()
            .filter(|(_, metadata)| {
                metadata.serial_number.as_deref() == Some(serial_number)
                    && old_metadata
                        .vid
                        .as_ref()
                        .is_none_or(|vid| metadata.vid.as_ref() == Some(vid))
                    && old_metadata
                        .pid
                        .as_ref()
                        .is_none_or(|pid| metadata.pid.as_ref() == Some(pid))
            })
            .cloned()
            .collect();
        match matching.len() {
            1 => return Ok(matching.into_iter().next()),
            count if count > 1 => {
                return Err(format!(
                    "cannot reconnect {normalized}: {count} ports have USB serial number {serial_number}"
                ));
            }
            _ => {}
        }
    }

    Ok(candidates
        .into_iter()
        .find(|(name, _)| normalize_port(name) == normalized))
}

fn normalize_port(port: &str) -> String {
    let normalized = port.trim().to_ascii_uppercase();
    normalized
        .strip_prefix("\\\\.\\")
        .unwrap_or(&normalized)
        .to_string()
}

fn make_description(port_type: &str, manufacturer: Option<&str>, product: Option<&str>) -> String {
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

fn contains_subsequence(data: &[u8], sequence: &[u8]) -> bool {
    !sequence.is_empty()
        && data
            .windows(sequence.len())
            .any(|window| window == sequence)
}

fn vid_pid(value: u16) -> Option<String> {
    (value != 0).then(|| format!("{value:04X}"))
}

fn to_hex(data: &[u8]) -> String {
    data.iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_bytes(data: &[u8], encoding: &str) -> String {
    match encoding {
        "hex" => to_hex(data),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

fn strip_ansi_bytes(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index] != 0x1B {
            if data[index] == 0x08 {
                if output
                    .last()
                    .is_some_and(|byte| !matches!(byte, b'\r' | b'\n'))
                {
                    output.pop();
                }
            } else {
                output.push(data[index]);
            }
            index += 1;
            continue;
        }

        index += 1;
        if index >= data.len() {
            break;
        }
        match data[index] {
            b'[' => {
                index += 1;
                while index < data.len() {
                    let byte = data[index];
                    index += 1;
                    if (0x40..=0x7E).contains(&byte) {
                        break;
                    }
                }
            }
            b']' => {
                index += 1;
                while index < data.len() {
                    if data[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if data[index] == 0x1B && data.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    output
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
    use std::collections::VecDeque;

    #[test]
    fn hub_starts_without_active_port() {
        let mut hub = SerialHub::new();
        assert_eq!(hub.status().active_port, None);
    }

    #[test]
    fn normalize_port_trims_and_uppercases() {
        assert_eq!(normalize_port(" com3 "), "COM3");
        assert_eq!(normalize_port("\\\\.\\COM10"), "COM10");
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

    #[test]
    fn collector_matches_sequence_across_reads() {
        let result = collect_chunks(
            [b"login: roo".as_slice(), b"t\r\n# ".as_slice()],
            StopCondition::Sequence(b"# "),
            128,
        );
        assert_eq!(result.stop_reason, "sequence");
        assert!(result.matched);
        assert_eq!(result.bytes, b"login: root\r\n# ");
    }

    #[test]
    fn collector_stops_on_line_or_max_bytes() {
        let line = collect_chunks([b"hello\r\nmore".as_slice()], StopCondition::Line, 128);
        assert_eq!(line.stop_reason, "line");
        assert!(line.matched);

        let limited = collect_chunks([b"0123456789".as_slice()], StopCondition::Timeout, 4);
        assert_eq!(limited.stop_reason, "max_bytes");
        assert_eq!(limited.bytes, b"0123");
    }

    #[test]
    fn reconnect_identity_detects_reused_com_port() {
        let mut old = PortMetadata::unknown();
        old.vid = Some("1234".to_string());
        old.pid = Some("5678".to_string());
        old.serial_number = Some("BOARD-A".to_string());
        let mut new = old.clone();
        new.serial_number = Some("BOARD-B".to_string());
        assert!(identity_mismatch(&old, &new).is_some());
        assert!(identity_mismatch(&old, &old).is_none());
    }

    #[test]
    fn reconnect_candidate_follows_unique_usb_serial_number() {
        let mut old = PortMetadata::unknown();
        old.vid = Some("1234".to_string());
        old.pid = Some("5678".to_string());
        old.serial_number = Some("BOARD-A".to_string());
        let candidate =
            select_reconnect_candidate("COM3", &old, vec![("COM8".to_string(), old.clone())])
                .ok()
                .flatten()
                .expect("matching board should be selected");
        assert_eq!(candidate.0, "COM8");

        let mut replacement = PortMetadata::unknown();
        replacement.serial_number = Some("BOARD-B".to_string());
        let candidate = select_reconnect_candidate(
            "COM3",
            &old,
            vec![
                ("COM3".to_string(), replacement),
                ("COM8".to_string(), old.clone()),
            ],
        )
        .ok()
        .flatten()
        .expect("stable identity should take priority over a reused COM number");
        assert_eq!(candidate.0, "COM8");
    }

    #[test]
    fn ansi_filter_keeps_terminal_text_and_removes_controls() {
        let raw = b"\x1b[32mroot\x1b[0m@board:~# \x1b]0;title\x07";
        assert_eq!(strip_ansi_bytes(raw), b"root@board:~# ");
        assert_eq!(strip_ansi_bytes(b"ab\x08c"), b"ac");
    }

    fn collect_chunks<'a>(
        chunks: impl IntoIterator<Item = &'a [u8]>,
        condition: StopCondition<'_>,
        max_bytes: usize,
    ) -> CollectedRead {
        let mut chunks: VecDeque<Vec<u8>> =
            chunks.into_iter().map(|chunk| chunk.to_vec()).collect();
        collect_with(
            |buffer, _| {
                if let Some(chunk) = chunks.pop_front() {
                    let count = chunk.len().min(buffer.len());
                    buffer[..count].copy_from_slice(&chunk[..count]);
                    Ok(count)
                } else {
                    Err(ReadFailure::Io(io::Error::from(io::ErrorKind::TimedOut)))
                }
            },
            Duration::from_secs(1),
            None,
            max_bytes,
            condition,
        )
        .ok()
        .expect("collector should succeed")
    }
}
