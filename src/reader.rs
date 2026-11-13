//! One deadline, lossless framing, and cooperative read cancellation.
use serialport::SerialPort;
use std::{
    collections::VecDeque,
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub(crate) enum StopCondition<'a> {
    Timeout,
    Line,
    Sequence(&'a [u8]),
}
pub(crate) struct CollectedRead {
    pub bytes: Vec<u8>,
    pub stop_reason: &'static str,
    pub matched: bool,
    pub failure: Option<ReadFailure>,
}
pub(crate) enum ReadFailure {
    Serial(serialport::Error),
    Io(io::Error),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_from_port(
    handle: &mut dyn SerialPort,
    pending: &mut VecDeque<u8>,
    cancel: &AtomicBool,
    overall: Duration,
    idle: Option<Duration>,
    max_bytes: usize,
    condition: StopCondition<'_>,
) -> Result<CollectedRead, ReadFailure> {
    collect_with(
        pending,
        cancel,
        |buffer, timeout| {
            handle.set_timeout(timeout).map_err(ReadFailure::Serial)?;
            handle.read(buffer).map_err(ReadFailure::Io)
        },
        overall,
        idle,
        max_bytes,
        condition,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_with<F>(
    pending: &mut VecDeque<u8>,
    cancel: &AtomicBool,
    mut read_once: F,
    overall: Duration,
    idle: Option<Duration>,
    max_bytes: usize,
    condition: StopCondition<'_>,
) -> Result<CollectedRead, ReadFailure>
where
    F: FnMut(&mut [u8], Duration) -> Result<usize, ReadFailure>,
{
    let deadline = Instant::now() + overall;
    let mut last_data_at: Option<Instant> = None;
    let mut bytes = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(stopped(bytes, "cancelled", false));
        }
        if bytes.len() >= max_bytes {
            return Ok(stopped(bytes, "max_bytes", false));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(stopped(bytes, "timeout", false));
        }
        let idle_deadline = last_data_at.and_then(|at| idle.map(|duration| at + duration));
        if idle_deadline.is_some_and(|at| now >= at) {
            return Ok(stopped(bytes, "idle", false));
        }
        let next = idle_deadline.map_or(deadline, |at| at.min(deadline));
        // Short polls let another MCP request cancel a read without touching its driver handle.
        let timeout = next
            .saturating_duration_since(now)
            .min(Duration::from_millis(50))
            .max(Duration::from_millis(1));
        let mut buffer = [0u8; 1024];
        let wanted = (max_bytes - bytes.len()).min(buffer.len());
        let count = if !pending.is_empty() {
            let count = wanted.min(pending.len());
            for slot in &mut buffer[..count] {
                *slot = pending.pop_front().unwrap();
            }
            count
        } else {
            match read_once(&mut buffer[..wanted], timeout) {
                Ok(0) => return Ok(stopped(bytes, "end_of_stream", false)),
                Ok(count) => count,
                Err(ReadFailure::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    if bytes.is_empty() {
                        return Err(error);
                    }
                    let mut partial = stopped(bytes, "error", false);
                    partial.failure = Some(error);
                    return Ok(partial);
                }
            }
        };
        let previous = bytes.len();
        bytes.extend_from_slice(&buffer[..count]);
        last_data_at = Some(Instant::now());
        let matched = match condition {
            StopCondition::Timeout => None,
            StopCondition::Line => bytes
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map(|index| {
                    let end = index + 1;
                    if bytes[index] == b'\r' && bytes.get(end) == Some(&b'\n') {
                        end + 1
                    } else {
                        end
                    }
                }),
            StopCondition::Sequence(sequence) if !sequence.is_empty() => {
                let start = previous.saturating_sub(sequence.len() - 1);
                bytes[start..]
                    .windows(sequence.len())
                    .position(|window| window == sequence)
                    .map(|index| start + index + sequence.len())
            }
            _ => None,
        };
        if let Some(end) = matched {
            // Prepend the suffix: it precedes any bytes that were already pending.
            for byte in bytes.drain(end..).rev() {
                pending.push_front(byte);
            }
            let reason = if matches!(condition, StopCondition::Line) {
                "line"
            } else {
                "sequence"
            };
            return Ok(stopped(bytes, reason, true));
        }
    }
}

fn stopped(bytes: Vec<u8>, stop_reason: &'static str, matched: bool) -> CollectedRead {
    CollectedRead {
        bytes,
        stop_reason,
        matched,
        failure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trailing_binary_frame_survives_next_read() {
        let mut pending = VecDeque::from(Vec::from(b"\x00\xffEND\x80\x01".as_slice()));
        let cancel = AtomicBool::new(false);
        let first = collect_with(
            &mut pending,
            &cancel,
            |_, _| panic!("already buffered"),
            Duration::from_secs(1),
            None,
            100,
            StopCondition::Sequence(b"END"),
        )
        .ok()
        .unwrap();
        assert_eq!(first.bytes, b"\x00\xffEND");
        let next = collect_with(
            &mut pending,
            &cancel,
            |_, _| panic!("already buffered"),
            Duration::from_secs(1),
            None,
            2,
            StopCondition::Timeout,
        )
        .ok()
        .unwrap();
        assert_eq!(next.bytes, b"\x80\x01");
    }
    #[test]
    fn short_driver_timeout_does_not_end_overall_read() {
        let mut calls = 0;
        let read = collect_with(
            &mut VecDeque::new(),
            &AtomicBool::new(false),
            |buffer, _| {
                calls += 1;
                if calls == 1 {
                    Err(ReadFailure::Io(io::ErrorKind::TimedOut.into()))
                } else {
                    buffer[0] = 42;
                    Ok(1)
                }
            },
            Duration::from_secs(1),
            None,
            1,
            StopCondition::Timeout,
        )
        .ok()
        .unwrap();
        assert_eq!(read.bytes, [42]);
    }
    #[test]
    fn cancellation_preserves_pending_data() {
        let mut pending = VecDeque::from([1, 2]);
        let result = collect_with(
            &mut pending,
            &AtomicBool::new(true),
            |_, _| panic!(),
            Duration::from_secs(1),
            None,
            10,
            StopCondition::Timeout,
        )
        .ok()
        .unwrap();
        assert_eq!(result.stop_reason, "cancelled");
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn disconnect_preserves_already_received_bytes() {
        let mut calls = 0;
        let partial = collect_with(
            &mut VecDeque::new(),
            &AtomicBool::new(false),
            |buffer, _| {
                calls += 1;
                if calls == 1 {
                    buffer[..2].copy_from_slice(&[0, 255]);
                    Ok(2)
                } else {
                    Err(ReadFailure::Io(io::ErrorKind::BrokenPipe.into()))
                }
            },
            Duration::from_secs(1),
            None,
            100,
            StopCondition::Timeout,
        )
        .ok()
        .unwrap();
        assert_eq!(partial.bytes, [0, 255]);
        assert_eq!(partial.stop_reason, "error");
        assert!(partial.failure.is_some());
    }
}
