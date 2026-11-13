//! Test-only in-memory driver, never enumerates or opens real hardware.
use super::*;
use serialport::Result;
use std::sync::{Mutex, mpsc::Sender};

pub(crate) struct TestPort {
    pub incoming: VecDeque<u8>,
    pub writes: Arc<Mutex<Vec<u8>>>,
    pub waiting: Option<Sender<()>>,
    pub panic_on_read: bool,
    pub read_error_when_empty: Option<io::ErrorKind>,
    pub receive_capacity: Option<usize>,
    pub timeout: Duration,
}
impl Default for TestPort {
    fn default() -> Self {
        Self {
            incoming: VecDeque::new(),
            writes: Arc::new(Mutex::new(Vec::new())),
            waiting: None,
            panic_on_read: false,
            read_error_when_empty: None,
            receive_capacity: None,
            timeout: Duration::from_secs(1),
        }
    }
}
impl IoRead for TestPort {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(!self.panic_on_read, "test driver panic");
        if self.incoming.is_empty() {
            if let Some(error) = self.read_error_when_empty {
                return Err(error.into());
            }
            if let Some(tx) = self.waiting.take() {
                let _ = tx.send(());
            }
            std::thread::sleep(self.timeout);
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = buffer.len().min(self.incoming.len());
        for byte in &mut buffer[..count] {
            *byte = self.incoming.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl IoWrite for TestPort {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self
            .receive_capacity
            .is_some_and(|capacity| self.incoming.len() + buffer.len() > capacity)
        {
            return Err(io::Error::other("test driver receive buffer overrun"));
        }
        self.writes.lock().unwrap().extend_from_slice(buffer);
        self.incoming.extend(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl SerialPort for TestPort {
    fn name(&self) -> Option<String> {
        Some("test-port".into())
    }
    fn baud_rate(&self) -> Result<u32> {
        Ok(115200)
    }
    fn data_bits(&self) -> Result<DataBits> {
        Ok(DataBits::Eight)
    }
    fn flow_control(&self) -> Result<FlowControl> {
        Ok(FlowControl::None)
    }
    fn parity(&self) -> Result<Parity> {
        Ok(Parity::None)
    }
    fn stop_bits(&self) -> Result<StopBits> {
        Ok(StopBits::One)
    }
    fn timeout(&self) -> Duration {
        self.timeout
    }
    fn set_baud_rate(&mut self, _baud_rate: u32) -> Result<()> {
        Ok(())
    }
    fn set_data_bits(&mut self, _data_bits: DataBits) -> Result<()> {
        Ok(())
    }
    fn set_flow_control(&mut self, _flow_control: FlowControl) -> Result<()> {
        Ok(())
    }
    fn set_parity(&mut self, _parity: Parity) -> Result<()> {
        Ok(())
    }
    fn set_stop_bits(&mut self, _stop_bits: StopBits) -> Result<()> {
        Ok(())
    }
    fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        self.timeout = timeout;
        Ok(())
    }
    fn write_request_to_send(&mut self, _level: bool) -> Result<()> {
        Ok(())
    }
    fn write_data_terminal_ready(&mut self, _level: bool) -> Result<()> {
        Ok(())
    }
    fn read_clear_to_send(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn read_data_set_ready(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn read_ring_indicator(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn read_carrier_detect(&mut self) -> Result<bool> {
        Ok(false)
    }
    fn bytes_to_read(&self) -> Result<u32> {
        Ok(self.incoming.len() as u32)
    }
    fn bytes_to_write(&self) -> Result<u32> {
        Ok(0)
    }
    fn clear(&self, _buffer_to_clear: ClearBuffer) -> Result<()> {
        Ok(())
    }
    fn try_clone(&self) -> Result<Box<dyn SerialPort>> {
        Err(serialport::Error::new(
            serialport::ErrorKind::Unknown,
            "not cloneable",
        ))
    }
    fn set_break(&self) -> Result<()> {
        Ok(())
    }
    fn clear_break(&self) -> Result<()> {
        Ok(())
    }
}
impl SerialHub {
    pub(crate) fn insert_test_port(&mut self, name: &str, port: TestPort) {
        self.ports.insert(
            name.into(),
            TrackedPort {
                handle: Some(Box::new(port)),
                metadata: PortMetadata::unknown(),
                settings: PortSettings {
                    profile: Profile::Binary,
                    ..PortSettings::default()
                },
                present: Some(true),
                last_error: None,
                pending: VecDeque::new(),
            },
        );
        self.active = Some(name.into());
    }
}
