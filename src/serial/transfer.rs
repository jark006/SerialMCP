use super::*;
use std::{fs, path::Path, time::Instant};

const MODEM_TOOL_PROBE: &str = "for t in lrz lsz rz sz rx sx rb sb; do p=$(command -v \"$t\") && [ -f \"$p\" ] && [ -x \"$p\" ] && printf '%s=1\\n' \"$t\" || printf '%s=0\\n' \"$t\"; done";

impl SerialHub {
    /// Detect common X/Y/ZMODEM command-line tools on the target shell.
    pub fn check_modem_tools(
        &mut self,
        port: Option<&str>,
        timeout_ms: u64,
    ) -> Result<ModemToolsResult, String> {
        let shell = crate::terminal::ShellCommand::new(MODEM_TOOL_PROBE);
        let result = self.run_command(
            port,
            &shell.payload,
            timeout_ms,
            300,
            16_384,
            Some(&shell.end),
            false,
            false,
            "text",
            true,
        )?;
        let (data, exit_code) = shell.parse(result.raw_data.as_bytes());
        if result.stop_reason == "cancelled" {
            return Err("modem tool probe cancelled; no complete result available".into());
        }
        if exit_code != Some(0) {
            return Err(format!(
                "modem tool probe did not reach a successful completion marker (stop_reason={})",
                result.stop_reason
            ));
        }
        Ok(ModemToolsResult {
            port: result.port,
            tools: parse_modem_tool_probe(&data),
        })
    }

    /// Transfer a local file to the target using XMODEM, YMODEM, or ZMODEM.
    ///
    /// The optional command is sent before the protocol starts (for example
    /// `rz`, `rb`, or a custom boot-loader command). Keeping the command and
    /// protocol in one operation prevents another request from consuming its handshake.
    #[allow(clippy::too_many_arguments)]
    pub fn send_file(
        &mut self,
        port: Option<&str>,
        protocol: &str,
        local_path: &str,
        remote_path: Option<&str>,
        command: Option<&str>,
        timeout_ms: u64,
        shell_command: bool,
    ) -> Result<FileTransferResult, String> {
        let shell = command
            .filter(|_| shell_command)
            .map(crate::terminal::ShellCommand::new);
        let normalized = self.resolve_port(port)?;
        let metadata = fs::metadata(local_path)
            .map_err(|error| format!("failed to inspect local file {local_path}: {error}"))?;
        if !metadata.is_file() {
            return Err(format!("local path is not a file: {local_path}"));
        }
        if metadata.len() > crate::modem::MAX_FILE_BYTES as u64 {
            return Err("file exceeds the 64 MiB transfer limit".to_string());
        }
        let mut bytes = Vec::new();
        fs::File::open(local_path)
            .and_then(|file| {
                file.take(crate::modem::MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|error| format!("failed to read local file {local_path}: {error}"))?;
        if bytes.len() > crate::modem::MAX_FILE_BYTES {
            return Err("file exceeds the 64 MiB transfer limit".to_string());
        }
        let result = {
            let configured_timeout =
                Duration::from_millis(self.tracked_mut(&normalized)?.settings.timeout_ms);
            let tracked = self.tracked_mut(&normalized)?;
            let handle = tracked
                .handle
                .as_deref_mut()
                .ok_or("port is disconnected; call serial_reconnect")?;
            let result = (|| {
                handle
                    .set_timeout(Duration::from_millis(timeout_ms))
                    .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                if command.is_some() {
                    tracked.pending.clear();
                }
                let startup = shell
                    .as_ref()
                    .map(|shell| {
                        std::str::from_utf8(&shell.payload).expect("shell payload is UTF-8")
                    })
                    .or(command);
                send_transfer_command(handle, startup)?;
                if let Some(shell) = &shell {
                    wait_for_transfer_marker(
                        handle,
                        &mut tracked.pending,
                        &shell.begin,
                        timeout_ms,
                    )?;
                    handle
                        .set_timeout(Duration::from_millis(timeout_ms))
                        .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                }
                let mut device = SerialIo {
                    handle,
                    pending: &mut tracked.pending,
                    observed: VecDeque::new(),
                    shell: shell.as_ref(),
                    remote_finished: false,
                    driver_failed: false,
                };
                let transferred = match protocol {
                    "xmodem" => send_xmodem(&mut device, &bytes, false),
                    "xmodem-1k" => send_xmodem(&mut device, &bytes, true),
                    "ymodem" => send_ymodem(
                        &mut device,
                        &bytes,
                        transfer_file_name(local_path, remote_path),
                    ),
                    "zmodem" => send_zmodem(
                        &mut device,
                        &bytes,
                        transfer_file_name(local_path, remote_path),
                        shell.is_some(),
                    ),
                    _ => {
                        return Err(TransferFailure::Message(
                            "unsupported modem protocol".to_string(),
                        ));
                    }
                }
                .map_err(|error| device.finish_failure(error, timeout_ms))?;
                if let Some(shell) = &shell {
                    wait_for_transfer_shell(handle, &mut tracked.pending, shell, timeout_ms)?;
                }
                handle
                    .set_timeout(configured_timeout)
                    .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                Ok(transferred)
            })();
            if result.is_err() {
                let _ = handle.set_timeout(configured_timeout);
            }
            result
        };
        let transferred = match result {
            Ok(value) => value,
            Err(TransferFailure::Serial(error)) => {
                let message = format!("{protocol} upload failed on {normalized}: {error}");
                return Err(self.record_failure(&normalized, message, false));
            }
            Err(TransferFailure::Io(error)) => {
                let message = format!("{protocol} upload failed on {normalized}: {error}");
                return Err(self.record_io_failure(&normalized, message, &error));
            }
            Err(TransferFailure::Message(message)) => return Err(message),
            Err(TransferFailure::Protocol(message)) => {
                let message = format!("{protocol} upload failed on {normalized}: {message}");
                return Err(message);
            }
        };
        self.mark_success(&normalized);
        Ok(FileTransferResult {
            port: normalized,
            protocol: protocol.to_string(),
            direction: "send".to_string(),
            local_path: local_path.to_string(),
            remote_path: remote_path.map(str::to_string),
            bytes_transferred: transferred,
            shell_ready: shell.is_some(),
            remote_exit_code: shell.as_ref().map(|_| 0),
        })
    }

    /// Receive a file from the target and save it to a local path.
    #[allow(clippy::too_many_arguments)]
    pub fn receive_file(
        &mut self,
        port: Option<&str>,
        protocol: &str,
        local_path: &str,
        remote_path: Option<&str>,
        command: Option<&str>,
        timeout_ms: u64,
        shell_command: bool,
    ) -> Result<FileTransferResult, String> {
        let shell = command
            .filter(|_| shell_command)
            .map(crate::terminal::ShellCommand::new);
        let normalized = self.resolve_port(port)?;
        validate_receive_path(local_path)?;
        let result = {
            let configured_timeout =
                Duration::from_millis(self.tracked_mut(&normalized)?.settings.timeout_ms);
            let tracked = self.tracked_mut(&normalized)?;
            let handle = tracked
                .handle
                .as_deref_mut()
                .ok_or("port is disconnected; call serial_reconnect")?;
            let result = (|| {
                handle
                    .set_timeout(Duration::from_millis(timeout_ms))
                    .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                if command.is_some() {
                    tracked.pending.clear();
                }
                let startup = shell
                    .as_ref()
                    .map(|shell| {
                        std::str::from_utf8(&shell.payload).expect("shell payload is UTF-8")
                    })
                    .or(command);
                send_transfer_command(handle, startup)?;
                if let Some(shell) = &shell {
                    wait_for_transfer_marker(
                        handle,
                        &mut tracked.pending,
                        &shell.begin,
                        timeout_ms,
                    )?;
                    handle
                        .set_timeout(Duration::from_millis(timeout_ms))
                        .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                }
                let mut device = SerialIo {
                    handle,
                    pending: &mut tracked.pending,
                    observed: VecDeque::new(),
                    shell: shell.as_ref(),
                    remote_finished: false,
                    driver_failed: false,
                };
                let (transferred, data) = match protocol {
                    "xmodem" => receive_xmodem(&mut device, false),
                    "xmodem-1k" => receive_xmodem(&mut device, true),
                    "ymodem" => receive_ymodem(&mut device),
                    "zmodem" => {
                        let ready = if shell.is_some() {
                            await_zmodem_receiver(&mut device)
                        } else {
                            Ok(())
                        };
                        ready.and_then(|()| receive_zmodem(&mut device))
                    }
                    _ => {
                        return Err(TransferFailure::Message(
                            "unsupported modem protocol".to_string(),
                        ));
                    }
                }
                .map_err(|error| device.finish_failure(error, timeout_ms))?;
                if let Some(shell) = &shell {
                    wait_for_transfer_shell(handle, &mut tracked.pending, shell, timeout_ms)?;
                }
                save_received_file(local_path, &data).map_err(|error| {
                    TransferFailure::Message(format!(
                        "failed to save received file to {local_path}: {error}"
                    ))
                })?;
                handle
                    .set_timeout(configured_timeout)
                    .map_err(|error| TransferFailure::Serial(error.to_string()))?;
                Ok(transferred)
            })();
            if result.is_err() {
                let _ = handle.set_timeout(configured_timeout);
            }
            result
        };
        let transferred = match result {
            Ok(value) => value,
            Err(TransferFailure::Serial(error)) => {
                let message = format!("{protocol} download failed on {normalized}: {error}");
                return Err(self.record_failure(&normalized, message, false));
            }
            Err(TransferFailure::Io(error)) => {
                let message = format!("{protocol} download failed on {normalized}: {error}");
                return Err(self.record_io_failure(&normalized, message, &error));
            }
            Err(TransferFailure::Message(message)) => return Err(message),
            Err(TransferFailure::Protocol(message)) => {
                let message = format!("{protocol} download failed on {normalized}: {message}");
                return Err(message);
            }
        };
        self.mark_success(&normalized);
        Ok(FileTransferResult {
            port: normalized,
            protocol: protocol.to_string(),
            direction: "receive".to_string(),
            local_path: local_path.to_string(),
            remote_path: remote_path.map(str::to_string),
            bytes_transferred: transferred,
            shell_ready: shell.is_some(),
            remote_exit_code: shell.as_ref().map(|_| 0),
        })
    }
}

fn wait_for_transfer_marker(
    handle: &mut dyn SerialPort,
    pending: &mut VecDeque<u8>,
    marker: &[u8],
    timeout_ms: u64,
) -> Result<(), TransferFailure> {
    let collected = collect_from_port(
        handle,
        pending,
        &AtomicBool::new(false),
        Duration::from_millis(timeout_ms),
        None,
        16_384,
        StopCondition::Sequence(marker),
    )
    .map_err(|error| match error {
        ReadFailure::Io(error) => TransferFailure::Io(error),
        ReadFailure::Serial(error) => TransferFailure::Serial(error.to_string()),
    })?;
    if !collected.matched || collected.failure.is_some() {
        return Err(TransferFailure::Protocol(format!(
            "remote shell startup marker was not received (stop_reason={}); protocol was not started",
            collected.stop_reason
        )));
    }
    Ok(())
}

fn wait_for_transfer_shell(
    handle: &mut dyn SerialPort,
    pending: &mut VecDeque<u8>,
    shell: &crate::terminal::ShellCommand,
    timeout_ms: u64,
) -> Result<(), TransferFailure> {
    let collected = collect_from_port(
        handle,
        pending,
        &AtomicBool::new(false),
        Duration::from_millis(timeout_ms),
        None,
        16_384,
        StopCondition::Sequence(&shell.end),
    )
    .map_err(|error| match error {
        ReadFailure::Io(error) => TransferFailure::Io(error),
        ReadFailure::Serial(error) => TransferFailure::Serial(error.to_string()),
    })?;
    if !collected.matched || collected.failure.is_some() {
        return Err(TransferFailure::Protocol(format!(
            "file protocol finished but shell completion was not confirmed (stop_reason={}); do not replay the transfer",
            collected.stop_reason
        )));
    }
    let mut output = shell.begin.clone();
    output.extend_from_slice(&collected.bytes);
    match shell.parse(&output).1 {
        Some(0) => Ok(()),
        code => Err(TransferFailure::Protocol(format!(
            "file protocol finished but remote command exit_code={code:?}"
        ))),
    }
}

fn validate_receive_path(local_path: &str) -> Result<(), String> {
    let path = Path::new(local_path);
    if path.file_name().is_none() {
        return Err(format!("local path must name a file: {local_path}"));
    }
    if path.is_dir() {
        return Err(format!("local path is a directory: {local_path}"));
    }
    if path.exists() {
        return Err(format!(
            "local destination already exists; choose a new path: {local_path}"
        ));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
        return Err(format!(
            "local destination directory does not exist: {}",
            parent.display()
        ));
    }
    Ok(())
}

fn save_received_file(local_path: &str, data: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(local_path)?;
    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(local_path);
    }
    result
}

pub(crate) fn parse_modem_tool_probe(data: &[u8]) -> BTreeMap<String, bool> {
    const TOOL_NAMES: [&str; 8] = ["lrz", "lsz", "rz", "sz", "rx", "sx", "rb", "sb"];
    let text = String::from_utf8_lossy(data);
    let mut tools: BTreeMap<String, bool> = TOOL_NAMES
        .iter()
        .map(|name| ((*name).to_string(), false))
        .collect();
    for line in text.lines() {
        let Some((name, value)) = line.trim().split_once('=') else {
            continue;
        };
        if let Some(available) = tools.get_mut(name) {
            *available = value == "1";
        }
    }
    tools
}

#[derive(Debug)]
enum TransferFailure {
    Serial(String),
    Io(io::Error),
    Message(String),
    Protocol(String),
}

struct SerialIo<'a> {
    handle: &'a mut dyn SerialPort,
    pending: &'a mut VecDeque<u8>,
    observed: VecDeque<u8>,
    shell: Option<&'a crate::terminal::ShellCommand>,
    remote_finished: bool,
    driver_failed: bool,
}

impl SerialIo<'_> {
    fn observe(&mut self, bytes: &[u8]) {
        const DIAGNOSTIC_BYTES: usize = 16_384;
        let bytes = &bytes[bytes.len().saturating_sub(DIAGNOSTIC_BYTES)..];
        let remove = self
            .observed
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(DIAGNOSTIC_BYTES);
        self.observed.drain(..remove);
        self.observed.extend(bytes);
        if let Some(shell) = self.shell {
            let observed = self.observed.make_contiguous();
            self.remote_finished = observed
                .windows(shell.end.len())
                .any(|window| window == shell.end);
        }
    }

    fn finish_failure(&mut self, failure: TransferFailure, timeout_ms: u64) -> TransferFailure {
        if self.driver_failed {
            return failure;
        }
        if let Some(shell) = self.shell
            && !self.remote_finished
        {
            // A peer CAN often precedes its shell exit marker. Collect that
            // diagnostic without sending another protocol request into the shell.
            let tail = collect_from_port(
                self.handle,
                self.pending,
                &AtomicBool::new(false),
                Duration::from_millis(timeout_ms),
                None,
                16_384,
                StopCondition::Sequence(&shell.end),
            );
            match tail {
                Ok(tail) => {
                    self.observe(&tail.bytes);
                    if let Some(failure) = tail.failure {
                        return transfer_read_failure(failure);
                    }
                }
                Err(failure) => return transfer_read_failure(failure),
            }
        }
        let failure = match failure {
            TransferFailure::Io(error) => TransferFailure::Protocol(error.to_string()),
            other => other,
        };
        self.describe_failure(failure, self.shell)
    }

    fn describe_failure(
        &self,
        failure: TransferFailure,
        shell: Option<&crate::terminal::ShellCommand>,
    ) -> TransferFailure {
        let Some(shell) = shell else {
            return failure;
        };
        let observed: Vec<_> = self.observed.iter().copied().collect();
        let Some(end) = observed
            .windows(shell.end.len())
            .position(|window| window == shell.end)
        else {
            return failure;
        };
        // The protocol consumes the startup marker. Reconstruct it solely for
        // parsing the command's own exit marker, before any trailing prompt.
        let mut response = shell.begin.clone();
        response.extend_from_slice(&observed[..end + shell.end.len()]);
        let (data, code) = shell.parse(&response);
        let Some(code) = code.filter(|code| *code != 0) else {
            return failure;
        };
        let clean = strip_ansi_bytes(&data);
        let text = String::from_utf8_lossy(&clean)
            .trim()
            .chars()
            .take(1024)
            .collect::<String>();
        let reason = match failure {
            TransferFailure::Protocol(reason) => format!("{reason}; "),
            _ => String::new(),
        };
        TransferFailure::Message(format!(
            "{reason}remote file command exited with code {code}: {text}"
        ))
    }
}

fn transfer_read_failure(failure: ReadFailure) -> TransferFailure {
    match failure {
        ReadFailure::Io(error) => TransferFailure::Io(error),
        ReadFailure::Serial(error) => TransferFailure::Serial(error.to_string()),
    }
}

trait TransferIo: IoRead + IoWrite {
    fn unread(&mut self, bytes: &[u8]);
}
impl TransferIo for SerialIo<'_> {
    fn unread(&mut self, bytes: &[u8]) {
        for byte in bytes.iter().rev() {
            self.pending.push_front(*byte);
        }
    }
}

impl IoRead for SerialIo<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remote_finished {
            return Err(io::Error::other(
                "remote file command ended before protocol completed",
            ));
        }
        if !self.pending.is_empty() {
            let count = buffer.len().min(self.pending.len());
            for slot in &mut buffer[..count] {
                *slot = self.pending.pop_front().unwrap();
            }
            self.observe(&buffer[..count]);
            return Ok(count);
        }
        let count = IoRead::read(self.handle, buffer).inspect_err(|error| {
            self.driver_failed = !matches!(
                error.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            );
        })?;
        self.observe(&buffer[..count]);
        Ok(count)
    }
}

impl IoWrite for SerialIo<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        IoWrite::write(self.handle, buffer).inspect_err(|_| self.driver_failed = true)
    }

    fn flush(&mut self) -> io::Result<()> {
        IoWrite::flush(self.handle).inspect_err(|_| self.driver_failed = true)
    }
}

fn send_transfer_command(
    handle: &mut dyn SerialPort,
    command: Option<&str>,
) -> Result<(), TransferFailure> {
    let Some(command) = command.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    handle
        .clear(ClearBuffer::Input)
        .map_err(|error| TransferFailure::Serial(error.to_string()))?;
    handle
        .write_all(command.as_bytes())
        .map_err(TransferFailure::Io)?;
    if !command.as_bytes().ends_with(b"\n") && !command.as_bytes().ends_with(b"\r") {
        handle.write_all(b"\n").map_err(TransferFailure::Io)?;
    }
    handle.flush().map_err(TransferFailure::Io)
}

fn transfer_file_name<'a>(local_path: &'a str, remote_path: Option<&'a str>) -> &'a str {
    remote_path.unwrap_or_else(|| {
        Path::new(local_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("payload.bin")
    })
}

fn send_xmodem<D: IoRead + IoWrite>(
    handle: &mut D,
    data: &[u8],
    one_k: bool,
) -> Result<usize, TransferFailure> {
    crate::modem::xmodem_send(handle, data, one_k).map_err(TransferFailure::Io)
}

fn receive_xmodem<D: IoRead + IoWrite>(
    handle: &mut D,
    _one_k: bool,
) -> Result<(usize, Vec<u8>), TransferFailure> {
    let output = crate::modem::xmodem_receive(handle).map_err(TransferFailure::Io)?;
    Ok((output.len(), output))
}

fn send_ymodem<D: IoRead + IoWrite>(
    handle: &mut D,
    data: &[u8],
    file_name: &str,
) -> Result<usize, TransferFailure> {
    crate::modem::ymodem_send(handle, data, file_name).map_err(TransferFailure::Io)
}

fn receive_ymodem<D: IoRead + IoWrite>(
    handle: &mut D,
) -> Result<(usize, Vec<u8>), TransferFailure> {
    let (name, output) = crate::modem::ymodem_receive(handle).map_err(TransferFailure::Io)?;
    if name.is_empty() {
        return Err(TransferFailure::Protocol(
            "YMODEM session completed without a file".to_string(),
        ));
    }
    Ok((output.len(), output))
}

fn send_zmodem<D: TransferIo>(
    handle: &mut D,
    data: &[u8],
    file_name: &str,
    wait_for_receiver: bool,
) -> Result<usize, TransferFailure> {
    use zmodem2::{FileInfo, Position, Sender};

    if wait_for_receiver {
        await_zmodem_receiver(handle)?;
    }
    let mut sender = Sender::new().map_err(|error| TransferFailure::Protocol(error.to_string()))?;
    // UART peers can exit between subpackets. An ACK per KiB bounds in-flight
    // file bytes instead of continuing a large streaming window into a shell.
    sender.set_streaming_window(1);
    sender
        .start_file(FileInfo::new(
            Path::new(file_name)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("payload.bin")
                .as_bytes(),
            Some(Position::new(data.len() as u32)),
        ))
        .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
    sender
        .finish()
        .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
    drive_zmodem_sender(handle, &mut sender, data)
}

// A shell startup marker precedes exec/tcsetattr. Wait for the receiver's
// real header before sending ZRQINIT, so cooked terminal echo cannot reflect it.
fn await_zmodem_receiver<D: TransferIo>(handle: &mut D) -> Result<(), TransferFailure> {
    let mut received = Vec::new();
    while received.len() < 4096 {
        let mut byte = [0];
        handle.read_exact(&mut byte).map_err(TransferFailure::Io)?;
        received.push(byte[0]);
        if received.len() >= 3 {
            let index = received.len() - 3;
            if received[index] == b'*'
                && received[index + 1] == 0x18
                && matches!(received[index + 2], b'A' | b'B' | b'C')
            {
                handle.unread(&received[index..]);
                return Ok(());
            }
        }
    }
    Err(TransferFailure::Protocol(
        "remote ZMODEM receiver did not emit a header".into(),
    ))
}

fn drive_zmodem_sender<D: TransferIo>(
    handle: &mut D,
    sender: &mut zmodem2::Sender,
    data: &[u8],
) -> Result<usize, TransferFailure> {
    use zmodem2::{Action, Event};
    let deadline = Instant::now() + Duration::from_secs(24 * 60 * 60);
    let mut incoming = Vec::new();
    let mut need_more_wire = false;
    let mut idle_timeouts = 0usize;
    let mut session_completed = false;
    loop {
        if Instant::now() >= deadline {
            sender.abort();
            return Err(TransferFailure::Protocol("transfer timed out".to_string()));
        }
        match sender.poll() {
            Action::WriteWire(bytes) => {
                let payload = bytes.to_vec();
                handle.write_all(&payload).map_err(TransferFailure::Io)?;
                handle.flush().map_err(TransferFailure::Io)?;
                sender.wire_written(payload.len());
            }
            Action::ReadFile { offset, max_len } => {
                let start = offset.get() as usize;
                let end = start.saturating_add(max_len).min(data.len());
                if end <= start {
                    return Err(TransferFailure::Protocol(
                        "sender requested data past EOF".to_string(),
                    ));
                }
                sender
                    .submit_file(&data[start..end])
                    .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
            }
            Action::Event(Event::SessionCompleted) => session_completed = true,
            Action::Event(Event::Aborted) => {
                return Err(TransferFailure::Protocol(
                    "peer aborted transfer".to_string(),
                ));
            }
            Action::Event(_) => {}
            Action::Idle => {
                if session_completed {
                    handle.unread(&incoming);
                    return Ok(data.len());
                }
                if incoming.is_empty() || need_more_wire {
                    let mut buffer = [0u8; 4096];
                    match handle.read(&mut buffer) {
                        Ok(0) => {
                            idle_timeouts += 1;
                            if idle_timeouts >= 16 {
                                sender.abort();
                                return Err(TransferFailure::Protocol(
                                    "protocol response retry limit exceeded".to_string(),
                                ));
                            }
                            sender
                                .timeout()
                                .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
                        }
                        Ok(count) => {
                            idle_timeouts = 0;
                            incoming.extend_from_slice(&buffer[..count]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                            idle_timeouts += 1;
                            if idle_timeouts >= 16 {
                                sender.abort();
                                return Err(TransferFailure::Protocol(
                                    "protocol response retry limit exceeded".to_string(),
                                ));
                            }
                            sender
                                .timeout()
                                .map_err(|error| TransferFailure::Protocol(error.to_string()))?
                        }
                        Err(error) => return Err(TransferFailure::Io(error)),
                    }
                }
                if !incoming.is_empty() {
                    let consumed = sender
                        .submit_wire(&incoming)
                        .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
                    incoming.drain(..consumed);
                    need_more_wire = consumed == 0;
                    if incoming.len() > 65_536 {
                        return Err(TransferFailure::Protocol(
                            "ZMODEM parser made no progress within 64 KiB".into(),
                        ));
                    }
                    if consumed > 0 {
                        idle_timeouts = 0;
                    }
                }
            }
            _ => {
                return Err(TransferFailure::Protocol(
                    "unsupported ZMODEM sender action".to_string(),
                ));
            }
        }
    }
}

fn receive_zmodem<D: TransferIo>(handle: &mut D) -> Result<(usize, Vec<u8>), TransferFailure> {
    drive_zmodem_receiver(handle)
}

fn cancel_modem_session<D: IoWrite>(handle: &mut D) -> Result<(), TransferFailure> {
    // zmodem2::Receiver::abort only changes local state; it emits no wire CAN.
    handle
        .write_all(b"\x18\x18\x18\x18\x18\x18\x18\x18\x08\x08\x08\x08\x08\x08\x08\x08")
        .map_err(TransferFailure::Io)?;
    handle.flush().map_err(TransferFailure::Io)
}

fn drive_zmodem_receiver<D: TransferIo>(
    handle: &mut D,
) -> Result<(usize, Vec<u8>), TransferFailure> {
    use zmodem2::{Action, Event, Receiver};
    let mut receiver = Receiver::with_flow_control(0, true)
        .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(24 * 60 * 60);
    let mut incoming = Vec::new();
    let mut need_more_wire = false;
    let mut idle_timeouts = 0usize;
    let mut files_started = 0usize;
    let mut expected_size = None;
    let mut session_completed = false;
    loop {
        if Instant::now() >= deadline {
            cancel_modem_session(handle)?;
            return Err(TransferFailure::Protocol("transfer timed out".to_string()));
        }
        match receiver.poll() {
            Action::WriteWire(bytes) => {
                let payload = bytes.to_vec();
                handle.write_all(&payload).map_err(TransferFailure::Io)?;
                handle.flush().map_err(TransferFailure::Io)?;
                receiver.wire_written(payload.len());
            }
            Action::WriteFile(bytes) => {
                if output.len().saturating_add(bytes.len()) > crate::modem::MAX_FILE_BYTES {
                    cancel_modem_session(handle)?;
                    return Err(TransferFailure::Protocol(
                        "file exceeds the 64 MiB transfer limit".to_string(),
                    ));
                }
                output.extend_from_slice(bytes);
                let count = bytes.len();
                receiver
                    .file_written(count)
                    .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
            }
            Action::Event(Event::FileStarted(info)) => {
                files_started += 1;
                if files_started > 1 {
                    cancel_modem_session(handle)?;
                    return Err(TransferFailure::Protocol(
                        "multiple files are not supported by serial_receive_file".to_string(),
                    ));
                }
                expected_size = info.size.map(|size| size.get() as usize);
                if expected_size.is_some_and(|size| size > crate::modem::MAX_FILE_BYTES) {
                    cancel_modem_session(handle)?;
                    return Err(TransferFailure::Protocol(
                        "file exceeds the 64 MiB transfer limit".to_string(),
                    ));
                }
            }
            Action::Event(Event::FileCompleted) => {
                if let Some(size) = expected_size
                    && size != output.len()
                {
                    cancel_modem_session(handle)?;
                    return Err(TransferFailure::Protocol(format!(
                        "received {} bytes but peer advertised {size}",
                        output.len()
                    )));
                }
            }
            Action::Event(Event::SessionCompleted) => {
                if files_started != 1 {
                    cancel_modem_session(handle)?;
                    return Err(TransferFailure::Protocol(
                        "ZMODEM session completed without a file".to_string(),
                    ));
                }
                session_completed = true;
            }
            Action::Event(Event::Aborted) => {
                return Err(TransferFailure::Protocol(
                    "peer aborted transfer".to_string(),
                ));
            }
            Action::Idle => {
                if session_completed {
                    finish_zmodem_receiver(handle, incoming)?;
                    return Ok((output.len(), output));
                }
                if incoming.is_empty() || need_more_wire {
                    let mut buffer = [0u8; 4096];
                    match handle.read(&mut buffer) {
                        Ok(0) => {
                            idle_timeouts += 1;
                            if idle_timeouts >= 16 {
                                cancel_modem_session(handle)?;
                                return Err(TransferFailure::Protocol(
                                    "protocol response retry limit exceeded".to_string(),
                                ));
                            }
                            receiver
                                .timeout()
                                .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
                        }
                        Ok(count) => {
                            idle_timeouts = 0;
                            incoming.extend_from_slice(&buffer[..count]);
                        }
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                            idle_timeouts += 1;
                            if idle_timeouts >= 16 {
                                cancel_modem_session(handle)?;
                                return Err(TransferFailure::Protocol(
                                    "protocol response retry limit exceeded".to_string(),
                                ));
                            }
                            receiver
                                .timeout()
                                .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
                        }
                        Err(error) => return Err(TransferFailure::Io(error)),
                    }
                }
                if !incoming.is_empty() {
                    let consumed = receiver
                        .submit_wire(&incoming)
                        .map_err(|error| TransferFailure::Protocol(error.to_string()))?;
                    incoming.drain(..consumed);
                    need_more_wire = consumed == 0;
                    if incoming.len() > 65_536 {
                        return Err(TransferFailure::Protocol(
                            "ZMODEM parser made no progress within 64 KiB".into(),
                        ));
                    }
                    if consumed > 0 {
                        idle_timeouts = 0;
                    }
                }
            }
            _ => {
                return Err(TransferFailure::Protocol(
                    "unsupported ZMODEM receiver action".to_string(),
                ));
            }
        }
    }
}

// zmodem2 reports SessionCompleted when it receives ZFIN. The transport must
// still consume the sender's final OO, then return any shell prompt to the UART queue.
fn finish_zmodem_receiver<D: TransferIo>(
    handle: &mut D,
    mut incoming: Vec<u8>,
) -> Result<(), TransferFailure> {
    let mut start = 0;
    loop {
        while incoming
            .get(start)
            // lrzsz emits a ZHEX trailer with the high bit set on LF (0x8A).
            .is_some_and(|byte| matches!(byte & 0x7f, b'\r' | b'\n' | 0x11 | 0x13))
        {
            start += 1;
        }
        let tail = &incoming[start..];
        if tail.starts_with(b"OO") {
            handle.unread(&tail[2..]);
            return Ok(());
        }
        if (!tail.is_empty() && tail[0] != b'O') || tail.len() >= 2 || incoming.len() >= 4096 {
            handle.unread(tail);
            return Err(TransferFailure::Protocol(
                "expected final ZMODEM OO handshake".into(),
            ));
        }
        let mut buffer = [0; 1024];
        match handle.read(&mut buffer) {
            Ok(0) => {
                handle.unread(tail);
                return Err(TransferFailure::Protocol(
                    "missing final ZMODEM OO handshake".into(),
                ));
            }
            Ok(count) => incoming.extend_from_slice(&buffer[..count]),
            Err(error) => {
                handle.unread(tail);
                return Err(TransferFailure::Io(error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};

    #[test]
    fn peer_cancel_does_not_disconnect_port_but_driver_failure_does() {
        let path =
            std::env::temp_dir().join(format!("serial-mcp-cancel-test-{}.bin", std::process::id()));
        assert!(!path.exists());
        let mut hub = SerialHub::new();
        hub.insert_test_port(
            "COM_TEST",
            super::super::test_driver::TestPort {
                incoming: [0x18, 0x18].into(),
                ..Default::default()
            },
        );
        let error = hub
            .receive_file(
                Some("COM_TEST"),
                "xmodem",
                path.to_str().unwrap(),
                None,
                None,
                100,
                false,
            )
            .unwrap_err();
        assert!(error.contains("peer canceled"));
        assert_eq!(hub.status_for("COM_TEST").unwrap().state, "open");
        hub.insert_test_port(
            "COM_TEST",
            super::super::test_driver::TestPort {
                read_error_when_empty: Some(io::ErrorKind::BrokenPipe),
                ..Default::default()
            },
        );
        assert!(
            hub.receive_file(
                Some("COM_TEST"),
                "xmodem",
                path.to_str().unwrap(),
                None,
                None,
                100,
                false
            )
            .is_err()
        );
        assert_eq!(hub.status_for("COM_TEST").unwrap().state, "disconnected");
        assert!(!path.exists());
    }

    #[test]
    fn matching_shell_exit_stops_protocol_reads_without_touching_driver() {
        let shell = crate::terminal::ShellCommand::new("lsz missing");
        let mut pending: VecDeque<_> = b"No such file\r\n"
            .iter()
            .copied()
            .chain(shell.rc.iter().copied())
            .chain(b"128\x1f".iter().copied())
            .chain(shell.end.iter().copied())
            .collect();
        let mut port = super::super::test_driver::TestPort {
            panic_on_read: true,
            ..Default::default()
        };
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: Some(&shell),
            remote_finished: false,
            driver_failed: false,
        };
        let mut bytes = [0; 1024];
        assert!(device.read(&mut bytes).unwrap() > 0);
        let error = device.read(&mut bytes).unwrap_err();
        assert!(
            matches!(device.finish_failure(TransferFailure::Io(error), 100), TransferFailure::Message(message) if message.contains("code 128") && message.contains("No such file"))
        );
    }

    #[test]
    fn failed_transfer_reports_own_shell_error_before_trailing_prompt() {
        let shell = crate::terminal::ShellCommand::new("lrz -y");
        let mut incoming = b"-sh: lrz: Permission denied\r\n".to_vec();
        incoming.extend_from_slice(&shell.rc);
        incoming.extend_from_slice(b"126\x1f");
        incoming.extend_from_slice(&shell.end);
        incoming.extend_from_slice(b"root# ");
        let mut port = super::super::test_driver::TestPort {
            incoming: incoming.clone().into(),
            ..Default::default()
        };
        let mut pending = VecDeque::new();
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: None,
            remote_finished: false,
            driver_failed: false,
        };
        let mut bytes = vec![0; incoming.len()];
        device.read_exact(&mut bytes).unwrap();
        let failure = device.describe_failure(
            TransferFailure::Io(io::ErrorKind::TimedOut.into()),
            Some(&shell),
        );
        assert!(
            matches!(failure, TransferFailure::Message(message) if message.contains("code 126") && message.contains("Permission denied") && !message.contains("root#"))
        );
        let other_shell = crate::terminal::ShellCommand::new("lsz");
        assert!(matches!(
            device.describe_failure(
                TransferFailure::Io(io::ErrorKind::TimedOut.into()),
                Some(&other_shell)
            ),
            TransferFailure::Io(_)
        ));
    }

    #[test]
    fn diagnostic_buffer_retains_bounded_tail() {
        let mut port = super::super::test_driver::TestPort::default();
        let mut pending = VecDeque::new();
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: None,
            remote_finished: false,
            driver_failed: false,
        };
        device.observe(&vec![1; 20_000]);
        device.observe(b"last");
        assert_eq!(device.observed.len(), 16_384);
        assert_eq!(
            device
                .observed
                .iter()
                .rev()
                .take(4)
                .copied()
                .collect::<Vec<_>>(),
            b"tsal"
        );
    }

    #[test]
    fn final_handshake_accepts_lrzsz_high_bit_trailer_and_preserves_prompt() {
        let mut port = super::super::test_driver::TestPort {
            incoming: VecDeque::from(Vec::from(b"O# prompt".as_slice())),
            ..Default::default()
        };
        let mut pending = VecDeque::new();
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: None,
            remote_finished: false,
            driver_failed: false,
        };
        finish_zmodem_receiver(&mut device, vec![b'\r', 0x8a, 0x11, b'O']).unwrap();
        assert_eq!(pending.into_iter().collect::<Vec<_>>(), b"# prompt");
    }

    #[test]
    fn shell_sync_waits_for_completion_and_preserves_trailing_prompt() {
        let shell = crate::terminal::ShellCommand::new("lrz -y");
        let mut incoming = shell.rc.clone();
        incoming.extend_from_slice(b"0\x1f");
        incoming.extend_from_slice(&shell.end);
        incoming.extend_from_slice(b"# ready");
        let mut port = super::super::test_driver::TestPort {
            incoming: incoming.into(),
            ..Default::default()
        };
        let mut pending = VecDeque::new();
        wait_for_transfer_shell(&mut port, &mut pending, &shell, 1000).unwrap();
        assert_eq!(pending.into_iter().collect::<Vec<_>>(), b"# ready");
    }

    #[test]
    fn startup_marker_ignores_echoed_crc_request_and_preserves_real_handshake() {
        let shell = crate::terminal::ShellCommand::new("lrz --ymodem -y");
        let mut input = shell.payload.clone();
        input.extend_from_slice(&shell.begin);
        input.push(b'C');
        let mut port = super::super::test_driver::TestPort {
            incoming: input.into(),
            ..Default::default()
        };
        let mut pending = VecDeque::new();
        wait_for_transfer_marker(&mut port, &mut pending, &shell.begin, 1000).unwrap();
        assert_eq!(pending.into_iter().collect::<Vec<_>>(), b"C");
        assert!(port.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn passive_zmodem_start_waits_for_peer_header_without_writing() {
        let mut port = super::super::test_driver::TestPort {
            incoming: VecDeque::from(Vec::from(b"lrz waiting.**\x18B0100".as_slice())),
            ..Default::default()
        };
        let mut pending = VecDeque::new();
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: None,
            remote_finished: false,
            driver_failed: false,
        };
        await_zmodem_receiver(&mut device).unwrap();
        assert_eq!(pending.into_iter().collect::<Vec<_>>(), b"*\x18B");
        assert!(port.writes.lock().unwrap().is_empty());
    }

    #[test]
    #[cfg(unix)]
    #[ignore = "requires a POSIX shell and chmod"]
    fn modem_probe_rejects_present_but_nonexecutable_files() {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let executable =
            std::env::var("SERIAL_MCP_TEST_SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let script = format!(
            "d=$(mktemp -d) || exit 1; trap 'rm -rf -- \"$d\"' EXIT; printf '#!/bin/sh\\nexit 0\\n' > \"$d/lrz\"; cp \"$d/lrz\" \"$d/lsz\"; chmod 600 \"$d/lrz\"; chmod 700 \"$d/lsz\"; old_path=$PATH; PATH=\"$d\"; {MODEM_TOOL_PROBE}; PATH=$old_path"
        );
        let mut child = Command::new(executable)
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let tools = parse_modem_tool_probe(&output.stdout);
        assert!(!tools["lrz"]);
        assert!(tools["lsz"]);
    }

    #[test]
    fn buffered_handshake_precedes_driver_bytes() {
        let mut port = super::super::test_driver::TestPort {
            incoming: [0x06].into(),
            ..Default::default()
        };
        let mut pending = VecDeque::from(*b"C");
        let mut device = SerialIo {
            handle: &mut port,
            pending: &mut pending,
            observed: VecDeque::new(),
            shell: None,
            remote_finished: false,
            driver_failed: false,
        };
        let mut buffer = [0; 2];
        assert_eq!(device.read(&mut buffer).unwrap(), 1);
        assert_eq!(buffer[0], b'C');
        assert_eq!(device.read(&mut buffer).unwrap(), 1);
        assert_eq!(buffer[0], 0x06);
    }

    #[test]
    fn receive_never_overwrites_an_existing_file() {
        let path = std::env::temp_dir().join(format!(
            "serial-mcp-receive-test-{}.bin",
            std::process::id()
        ));
        let path = path.to_str().unwrap();
        // create_new prevents the test itself from clobbering unrelated data.
        save_received_file(path, b"original").unwrap();
        assert!(validate_receive_path(path).is_err());
        assert_eq!(
            save_received_file(path, b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(path).unwrap(), b"original");
        fs::remove_file(path).unwrap();
    }
    struct ChannelDevice {
        incoming: Receiver<Vec<u8>>,
        pending: VecDeque<u8>,
        outgoing: Sender<Vec<u8>>,
        suffix_after_oo: bool,
        read_limit: usize,
    }

    impl TransferIo for ChannelDevice {
        fn unread(&mut self, bytes: &[u8]) {
            for byte in bytes.iter().rev() {
                self.pending.push_front(*byte);
            }
        }
    }

    impl IoRead for ChannelDevice {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.pending.is_empty() {
                match self.incoming.recv_timeout(Duration::from_millis(100)) {
                    Ok(bytes) => self.pending.extend(bytes),
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(io::Error::from(io::ErrorKind::TimedOut));
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                    }
                }
            }
            let count = buffer.len().min(self.pending.len()).min(self.read_limit);
            for slot in &mut buffer[..count] {
                *slot = self.pending.pop_front().unwrap();
            }
            Ok(count)
        }
    }

    impl IoWrite for ChannelDevice {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let mut bytes = buffer.to_vec();
            if self.suffix_after_oo && buffer == b"OO" {
                bytes.extend_from_slice(b"\r\n# ");
            }
            self.outgoing
                .send(bytes)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn channel_pair() -> (ChannelDevice, ChannelDevice) {
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        (
            ChannelDevice {
                incoming: a_rx,
                pending: VecDeque::new(),
                outgoing: b_tx,
                suffix_after_oo: false,
                read_limit: 4096,
            },
            ChannelDevice {
                incoming: b_rx,
                pending: VecDeque::new(),
                outgoing: a_tx,
                suffix_after_oo: false,
                read_limit: 4096,
            },
        )
    }

    #[test]
    fn zmodem_round_trip_over_duplex_stream() {
        for read_limit in [1, 4096] {
            zmodem_round_trip(read_limit);
        }
    }

    #[test]
    fn zmodem_second_file_rejection_sends_wire_cancel_to_sender() {
        use zmodem2::{Action, Event, FileInfo, Position, Sender as ZSender};
        let (mut sender_device, mut receiver_device) = channel_pair();
        let receiver = std::thread::spawn(move || drive_zmodem_receiver(&mut receiver_device));
        let mut sender = ZSender::new().unwrap();
        sender
            .start_file(FileInfo::new(b"first.bin", Some(Position::new(1))))
            .unwrap();
        let mut wire_cancelled = false;
        let mut first_completed = false;
        for _ in 0..1000 {
            match sender.poll() {
                Action::WriteWire(bytes) => {
                    let bytes = bytes.to_vec();
                    sender_device.write_all(&bytes).unwrap();
                    sender.wire_written(bytes.len());
                }
                Action::ReadFile { .. } => sender.submit_file(b"x").unwrap(),
                Action::Event(Event::FileCompleted) if !first_completed => {
                    first_completed = true;
                    sender
                        .start_file(FileInfo::new(b"second.bin", Some(Position::new(1))))
                        .unwrap();
                    sender.finish().unwrap();
                }
                Action::Idle => {
                    let mut bytes = [0; 4096];
                    let count = sender_device.read(&mut bytes).unwrap();
                    if bytes[..count].windows(8).any(|bytes| bytes == [0x18; 8]) {
                        wire_cancelled = true;
                        break;
                    }
                    sender.submit_wire(&bytes[..count]).unwrap();
                }
                _ => {}
            }
        }
        assert!(wire_cancelled, "peer never received a wire cancellation");
        assert!(
            matches!(receiver.join().unwrap(), Err(TransferFailure::Protocol(message)) if message.contains("multiple files"))
        );
    }

    fn zmodem_round_trip(read_limit: usize) {
        use zmodem2::{FileInfo, Position, Sender as ZSender};
        let data = (0..65_537)
            .map(|value| (value * 31) as u8)
            .collect::<Vec<_>>();
        let (mut sender_device, mut receiver_device) = channel_pair();
        sender_device.suffix_after_oo = true;
        sender_device.read_limit = read_limit;
        receiver_device.read_limit = read_limit;
        let (release_tx, release_rx) = mpsc::channel();
        let receiver = std::thread::spawn(move || {
            let result = drive_zmodem_receiver(&mut receiver_device);
            let suffix: Vec<_> = receiver_device.pending.drain(..).collect();
            let _ = release_rx.recv();
            (result, suffix)
        });
        let mut sender = ZSender::new().unwrap();
        sender
            .start_file(FileInfo::new(
                b"firmware.bin",
                Some(Position::new(data.len() as u32)),
            ))
            .unwrap();
        sender.finish().unwrap();

        let sent = drive_zmodem_sender(&mut sender_device, &mut sender, &data);
        let _ = release_tx.send(());
        let (received_result, suffix) = receiver.join().unwrap();
        assert!(sent.is_ok(), "sender failed: {sent:?}");
        assert!(
            received_result.is_ok(),
            "receiver failed: {:?}",
            received_result.as_ref().err()
        );
        assert_eq!(sent.unwrap(), data.len());
        let (count, received) = received_result.unwrap();
        assert_eq!(count, data.len());
        assert_eq!(received, data);
        assert_eq!(suffix, b"\r\n# ");
    }
}
