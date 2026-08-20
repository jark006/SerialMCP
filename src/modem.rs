use std::io::{self, Read, Write};

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CRC_REQUEST: u8 = b'C';
const PAD: u8 = 0x1A;
const MAX_RETRIES: usize = 16;
const MAX_GARBAGE_BYTES: usize = 4096;

pub fn xmodem_send<D: Read + Write>(device: &mut D, data: &[u8], one_k: bool) -> io::Result<usize> {
    let crc = wait_for_start(device, true)?;
    let block_size = if one_k { 1024 } else { 128 };
    send_data_blocks(device, data, block_size, crc)?;
    finish_xmodem(device)?;
    Ok(data.len())
}

pub fn xmodem_receive<D: Read + Write>(device: &mut D) -> io::Result<Vec<u8>> {
    device.write_all(&[CRC_REQUEST])?;
    device.flush()?;
    receive_xmodem_blocks(device, None, false)
}

pub fn ymodem_send<D: Read + Write>(
    device: &mut D,
    data: &[u8],
    file_name: &str,
) -> io::Result<usize> {
    wait_for_byte(device, CRC_REQUEST, "receiver did not request YMODEM CRC")?;
    let header = ymodem_header(file_name, data.len())?;
    send_packet_with_retry(device, &packet(0, &header, true), ACK)?;
    wait_for_byte(device, CRC_REQUEST, "receiver did not request YMODEM data")?;

    send_data_blocks(device, data, 1024, true)?;
    finish_ymodem_file(device)?;
    wait_for_byte(
        device,
        CRC_REQUEST,
        "receiver did not request YMODEM batch terminator",
    )?;
    send_packet_with_retry(device, &packet(0, &[0; 128], true), ACK)?;
    Ok(data.len())
}

pub fn ymodem_receive<D: Read + Write>(device: &mut D) -> io::Result<(String, Vec<u8>)> {
    device.write_all(&[CRC_REQUEST])?;
    device.flush()?;

    let header = receive_expected_packet(device, 0, true)?;
    let (name, size) = parse_ymodem_header(&header)?;
    if name.is_empty() {
        device.write_all(&[ACK])?;
        device.flush()?;
        return Ok((name, Vec::new()));
    }
    device.write_all(&[ACK, CRC_REQUEST])?;
    device.flush()?;

    let mut data = receive_xmodem_blocks(device, Some(size), true)?;
    if data.len() != size {
        cancel(device)?;
        return Err(invalid_data(
            "YMODEM transfer ended before the advertised file size",
        ));
    }
    data.truncate(size);
    device.write_all(&[CRC_REQUEST])?;
    device.flush()?;
    let end = receive_expected_packet(device, 0, true)?;
    if end.first().copied().unwrap_or_default() != 0 {
        cancel(device)?;
        return Err(invalid_data("YMODEM batch terminator is not empty"));
    }
    device.write_all(&[ACK])?;
    device.flush()?;
    Ok((name, data))
}

fn send_data_blocks<D: Read + Write>(
    device: &mut D,
    data: &[u8],
    preferred_block_size: usize,
    crc: bool,
) -> io::Result<()> {
    let mut sequence = 1u8;
    let mut offset = 0usize;
    while offset < data.len() {
        let remaining = data.len() - offset;
        let block_size = if preferred_block_size == 1024 && remaining <= 128 {
            128
        } else {
            preferred_block_size
        };
        let count = remaining.min(block_size);
        let payload = padded(&data[offset..offset + count], block_size);
        let encoded = packet(sequence, &payload, crc);
        send_packet_with_retry(device, &encoded, ACK)?;
        offset += count;
        sequence = sequence.wrapping_add(1);
    }
    Ok(())
}

fn receive_xmodem_blocks<D: Read + Write>(
    device: &mut D,
    expected_size: Option<usize>,
    ymodem: bool,
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut expected_sequence = 1u8;
    let mut errors = 0usize;
    let mut garbage = 0usize;
    let mut started = false;

    loop {
        let marker = match read_byte(device) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                errors += 1;
                if errors >= MAX_RETRIES {
                    cancel(device)?;
                    return Err(timed_out("timed out waiting for a modem data block"));
                }
                device.write_all(&[if started { NAK } else { CRC_REQUEST }])?;
                device.flush()?;
                continue;
            }
            Err(error) => return Err(error),
        };

        match marker {
            SOH | STX => {
                started = true;
                let block_size = if marker == SOH { 128 } else { 1024 };
                match read_packet_body(device, block_size, true) {
                    Ok((sequence, payload)) if sequence == expected_sequence => {
                        let wanted = expected_size
                            .map(|size| size.saturating_sub(output.len()))
                            .unwrap_or(payload.len())
                            .min(payload.len());
                        output.extend_from_slice(&payload[..wanted]);
                        expected_sequence = expected_sequence.wrapping_add(1);
                        errors = 0;
                        garbage = 0;
                        device.write_all(&[ACK])?;
                        device.flush()?;
                    }
                    Ok((sequence, _)) if sequence == expected_sequence.wrapping_sub(1) => {
                        device.write_all(&[ACK])?;
                        device.flush()?;
                    }
                    Ok(_) => {
                        cancel(device)?;
                        return Err(invalid_data("unexpected modem block sequence"));
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::InvalidData | io::ErrorKind::TimedOut
                        ) =>
                    {
                        errors += 1;
                        if errors >= MAX_RETRIES {
                            cancel(device)?;
                            return Err(error);
                        }
                        device.write_all(&[NAK])?;
                        device.flush()?;
                    }
                    Err(error) => return Err(error),
                }
            }
            EOT if ymodem => {
                device.write_all(&[NAK])?;
                device.flush()?;
                wait_for_byte(device, EOT, "second YMODEM EOT was not received")?;
                device.write_all(&[ACK])?;
                device.flush()?;
                return Ok(output);
            }
            EOT => {
                device.write_all(&[ACK])?;
                device.flush()?;
                return Ok(output);
            }
            CAN => {
                if matches!(read_byte(device), Ok(CAN)) {
                    return Err(connection_aborted("peer canceled modem transfer"));
                }
                errors += 1;
            }
            _ => {
                garbage += 1;
                if garbage >= MAX_GARBAGE_BYTES {
                    cancel(device)?;
                    return Err(invalid_data("too much unexpected modem input"));
                }
            }
        }
    }
}

fn receive_expected_packet<D: Read + Write>(
    device: &mut D,
    expected_sequence: u8,
    crc: bool,
) -> io::Result<Vec<u8>> {
    let mut errors = 0usize;
    let mut garbage = 0usize;
    loop {
        let marker = match read_byte(device) {
            Ok(marker @ (SOH | STX)) => marker,
            Ok(CAN) if matches!(read_byte(device), Ok(CAN)) => {
                return Err(connection_aborted("peer canceled modem transfer"));
            }
            Ok(_) => {
                garbage += 1;
                if garbage >= MAX_GARBAGE_BYTES {
                    cancel(device)?;
                    return Err(invalid_data("too much unexpected modem input"));
                }
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                errors += 1;
                if errors >= MAX_RETRIES {
                    cancel(device)?;
                    return Err(timed_out("modem packet retry limit exceeded"));
                }
                device.write_all(&[CRC_REQUEST])?;
                device.flush()?;
                continue;
            }
            Err(error) => return Err(error),
        };
        let block_size = if marker == SOH { 128 } else { 1024 };
        match read_packet_body(device, block_size, crc) {
            Ok((sequence, payload)) if sequence == expected_sequence => return Ok(payload),
            Ok(_) => {
                errors += 1;
                device.write_all(&[NAK])?;
                device.flush()?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::TimedOut
                ) =>
            {
                errors += 1;
                device.write_all(&[NAK])?;
                device.flush()?;
            }
            Err(error) => return Err(error),
        }
        if errors >= MAX_RETRIES {
            cancel(device)?;
            return Err(timed_out("modem packet retry limit exceeded"));
        }
    }
}

fn send_packet_with_retry<D: Read + Write>(
    device: &mut D,
    encoded: &[u8],
    expected_response: u8,
) -> io::Result<()> {
    let mut cancels = 0usize;
    for _ in 0..MAX_RETRIES {
        device.write_all(encoded)?;
        device.flush()?;
        match read_byte(device) {
            Ok(value) if value == expected_response => return Ok(()),
            Ok(CAN) => {
                cancels += 1;
                if cancels >= 2 {
                    return Err(connection_aborted("peer canceled modem transfer"));
                }
            }
            Ok(NAK) => {}
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
    }
    cancel(device)?;
    Err(timed_out("modem packet retry limit exceeded"))
}

fn finish_xmodem<D: Read + Write>(device: &mut D) -> io::Result<()> {
    send_control_with_retry(device, EOT, ACK, "XMODEM EOT was not acknowledged")
}

fn finish_ymodem_file<D: Read + Write>(device: &mut D) -> io::Result<()> {
    for _ in 0..MAX_RETRIES {
        device.write_all(&[EOT])?;
        device.flush()?;
        match read_byte(device) {
            Ok(ACK) => return Ok(()),
            Ok(NAK) => {
                return send_control_with_retry(
                    device,
                    EOT,
                    ACK,
                    "second YMODEM EOT was not acknowledged",
                );
            }
            Ok(CAN) if matches!(read_byte(device), Ok(CAN)) => {
                return Err(connection_aborted("peer canceled modem transfer"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
    }
    cancel(device)?;
    Err(timed_out("first YMODEM EOT was not acknowledged"))
}

fn send_control_with_retry<D: Read + Write>(
    device: &mut D,
    control: u8,
    expected: u8,
    message: &'static str,
) -> io::Result<()> {
    for _ in 0..MAX_RETRIES {
        device.write_all(&[control])?;
        device.flush()?;
        match read_byte(device) {
            Ok(value) if value == expected => return Ok(()),
            Ok(CAN) if matches!(read_byte(device), Ok(CAN)) => {
                return Err(connection_aborted("peer canceled modem transfer"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
    }
    cancel(device)?;
    Err(timed_out(message))
}

fn wait_for_start<D: Read + Write>(device: &mut D, allow_checksum: bool) -> io::Result<bool> {
    let mut cancels = 0usize;
    let mut timeouts = 0usize;
    let mut garbage = 0usize;
    loop {
        match read_byte(device) {
            Ok(CRC_REQUEST) => return Ok(true),
            Ok(NAK) if allow_checksum => return Ok(false),
            Ok(CAN) => {
                cancels += 1;
                if cancels >= 2 {
                    return Err(connection_aborted("peer canceled modem transfer"));
                }
            }
            Ok(_) => {
                garbage += 1;
                if garbage >= MAX_GARBAGE_BYTES {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                timeouts += 1;
                if timeouts >= MAX_RETRIES {
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    cancel(device)?;
    Err(timed_out("receiver did not start modem transfer"))
}

fn wait_for_byte<D: Read>(device: &mut D, expected: u8, message: &'static str) -> io::Result<()> {
    let mut cancels = 0usize;
    let mut timeouts = 0usize;
    let mut garbage = 0usize;
    loop {
        match read_byte(device) {
            Ok(value) if value == expected => return Ok(()),
            Ok(CAN) => {
                cancels += 1;
                if cancels >= 2 {
                    return Err(connection_aborted("peer canceled modem transfer"));
                }
            }
            Ok(_) => {
                garbage += 1;
                if garbage >= MAX_GARBAGE_BYTES {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                timeouts += 1;
                if timeouts >= MAX_RETRIES {
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(timed_out(message))
}

fn packet(sequence: u8, payload: &[u8], crc: bool) -> Vec<u8> {
    debug_assert!(matches!(payload.len(), 128 | 1024));
    let mut encoded = Vec::with_capacity(payload.len() + 5);
    encoded.push(if payload.len() == 128 { SOH } else { STX });
    encoded.push(sequence);
    encoded.push(!sequence);
    encoded.extend_from_slice(payload);
    if crc {
        encoded.extend_from_slice(&crc16(payload).to_be_bytes());
    } else {
        encoded.push(
            payload
                .iter()
                .fold(0u8, |sum, value| sum.wrapping_add(*value)),
        );
    }
    encoded
}

fn read_packet_body<D: Read>(
    device: &mut D,
    block_size: usize,
    crc: bool,
) -> io::Result<(u8, Vec<u8>)> {
    let mut body = vec![0u8; 2 + block_size + if crc { 2 } else { 1 }];
    device.read_exact(&mut body)?;
    let sequence = body[0];
    if body[1] != !sequence {
        return Err(invalid_data("invalid modem sequence complement"));
    }
    let payload = &body[2..2 + block_size];
    if crc {
        let received = u16::from_be_bytes([body[2 + block_size], body[3 + block_size]]);
        if crc16(payload) != received {
            return Err(invalid_data("modem CRC mismatch"));
        }
    } else {
        let received = body[2 + block_size];
        let calculated = payload
            .iter()
            .fold(0u8, |sum, value| sum.wrapping_add(*value));
        if calculated != received {
            return Err(invalid_data("modem checksum mismatch"));
        }
    }
    Ok((sequence, payload.to_vec()))
}

fn ymodem_header(file_name: &str, file_size: usize) -> io::Result<[u8; 128]> {
    let mut header = [0u8; 128];
    let name = file_name.as_bytes();
    let size = file_size.to_string();
    if name.is_empty() || name.len() + size.len() + 2 > header.len() {
        return Err(invalid_input(
            "YMODEM filename and size do not fit in block 0",
        ));
    }
    header[..name.len()].copy_from_slice(name);
    let size_start = name.len() + 1;
    header[size_start..size_start + size.len()].copy_from_slice(size.as_bytes());
    Ok(header)
}

fn parse_ymodem_header(header: &[u8]) -> io::Result<(String, usize)> {
    let name_end = header
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| invalid_data("YMODEM filename is not terminated"))?;
    let name = String::from_utf8(header[..name_end].to_vec())
        .map_err(|_| invalid_data("YMODEM filename is not UTF-8"))?;
    if name.is_empty() {
        return Ok((name, 0));
    }
    let metadata = &header[name_end + 1..];
    let metadata_end = metadata
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(metadata.len());
    let size_text = std::str::from_utf8(&metadata[..metadata_end])
        .map_err(|_| invalid_data("YMODEM metadata is not UTF-8"))?
        .split_whitespace()
        .next()
        .ok_or_else(|| invalid_data("YMODEM file size is missing"))?;
    let size = size_text
        .parse::<usize>()
        .map_err(|_| invalid_data("YMODEM file size is invalid"))?;
    Ok((name, size))
}

fn padded(data: &[u8], block_size: usize) -> Vec<u8> {
    let mut output = vec![PAD; block_size];
    output[..data.len()].copy_from_slice(data);
    output
}

fn read_byte<R: Read>(reader: &mut R) -> io::Result<u8> {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn cancel<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(&[CAN, CAN])?;
    writer.flush()
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn timed_out(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
}

fn connection_aborted(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::time::Duration;

    #[derive(Default)]
    struct ScriptedDevice {
        input: VecDeque<u8>,
        output: Vec<u8>,
    }

    impl ScriptedDevice {
        fn with_input(input: impl IntoIterator<Item = u8>) -> Self {
            Self {
                input: input.into_iter().collect(),
                output: Vec::new(),
            }
        }
    }

    impl Read for ScriptedDevice {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(self.input.len());
            if count == 0 {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            for slot in &mut buffer[..count] {
                *slot = self.input.pop_front().unwrap();
            }
            Ok(count)
        }
    }

    impl Write for ScriptedDevice {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ChannelDevice {
        incoming: Receiver<Vec<u8>>,
        pending: VecDeque<u8>,
        outgoing: Sender<Vec<u8>>,
    }

    impl Read for ChannelDevice {
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
            let count = buffer.len().min(self.pending.len());
            for slot in &mut buffer[..count] {
                *slot = self.pending.pop_front().unwrap();
            }
            Ok(count)
        }
    }

    impl Write for ChannelDevice {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.outgoing
                .send(buffer.to_vec())
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
            },
            ChannelDevice {
                incoming: b_rx,
                pending: VecDeque::new(),
                outgoing: a_tx,
            },
        )
    }

    #[test]
    fn crc16_matches_xmodem_vector() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn xmodem_sender_retransmits_same_packet_after_nak() {
        let mut input = vec![b'x'; 64];
        input.extend([CRC_REQUEST, NAK, ACK, ACK]);
        let mut device = ScriptedDevice::with_input(input);
        assert_eq!(xmodem_send(&mut device, b"hello", false).unwrap(), 5);
        let packet_len = 3 + 128 + 2;
        assert_eq!(
            &device.output[..packet_len],
            &device.output[packet_len..packet_len * 2]
        );
        assert_eq!(device.output.last(), Some(&EOT));
    }

    #[test]
    fn xmodem_receiver_accepts_packet_and_acknowledges_eot() {
        let encoded = packet(1, &padded(b"hello", 128), true);
        let mut input = vec![b'x'; 64];
        input.extend(encoded);
        input.push(EOT);
        let mut device = ScriptedDevice::with_input(input);
        let output = xmodem_receive(&mut device).unwrap();
        assert_eq!(&output[..5], b"hello");
        assert_eq!(output.len(), 128);
        assert_eq!(device.output, [CRC_REQUEST, ACK, ACK]);
    }

    #[test]
    fn ymodem_header_uses_decimal_size() {
        let header = ymodem_header("firmware.bin", 4096).unwrap();
        let parsed = parse_ymodem_header(&header).unwrap();
        assert_eq!(parsed, ("firmware.bin".to_string(), 4096));
        assert!(header.windows(4).any(|window| window == b"4096"));
    }

    #[test]
    fn xmodem_round_trip_over_duplex_stream() {
        let data = (0..1500).map(|value| value as u8).collect::<Vec<_>>();
        let (mut sender_device, mut receiver_device) = channel_pair();
        let receiver = std::thread::spawn(move || xmodem_receive(&mut receiver_device));

        assert_eq!(
            xmodem_send(&mut sender_device, &data, true).unwrap(),
            data.len()
        );
        let received = receiver.join().unwrap().unwrap();
        assert_eq!(&received[..data.len()], data);
        assert!(received[data.len()..].iter().all(|byte| *byte == PAD));
    }

    #[test]
    fn ymodem_round_trip_preserves_name_size_and_data() {
        let data = (0..2300)
            .map(|value| (value * 17) as u8)
            .collect::<Vec<_>>();
        let (mut sender_device, mut receiver_device) = channel_pair();
        let receiver = std::thread::spawn(move || ymodem_receive(&mut receiver_device));

        assert_eq!(
            ymodem_send(&mut sender_device, &data, "firmware.bin").unwrap(),
            data.len()
        );
        let (name, received) = receiver.join().unwrap().unwrap();
        assert_eq!(name, "firmware.bin");
        assert_eq!(received, data);
    }
}
