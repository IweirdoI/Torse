//! Nonblocking framed transports. Socket polling stays outside world publication.
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Mutex};
use torse_types::{Fault, FaultCode};
#[derive(Clone, Copy, Debug)]
pub struct TransportLimits {
    pub frame: usize,
    pub queued_frames: usize,
    pub queued_bytes: usize,
    pub poll_bytes: usize,
}
impl TransportLimits {
    fn validate(self) -> Result<(), Fault> {
        if self.frame == 0
            || self.frame > u32::MAX as usize
            || self.queued_frames == 0
            || self.queued_bytes < self.frame.saturating_add(4)
            || self.poll_bytes == 0
        {
            return Err(Fault::invalid_data("transport limits"));
        }
        Ok(())
    }
}
pub trait Transport {
    fn send(&mut self, frame: Vec<u8>) -> Result<(), Fault>;
    fn receive(&mut self) -> Result<Option<Vec<u8>>, Fault>;
    fn poll(&mut self) -> Result<(), Fault>;
    /// Control path: drops unsent data and closes, independent of queue capacity.
    fn close(&mut self);
}
struct Queue {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    closed: bool,
}
pub struct MemoryTransport {
    input: Arc<Mutex<Queue>>,
    output: Arc<Mutex<Queue>>,
    limits: TransportLimits,
}
fn closed() -> Fault {
    Fault::new(FaultCode::InvalidState, "transport closed")
}
impl MemoryTransport {
    pub fn pair(limits: TransportLimits) -> Result<(Self, Self), Fault> {
        limits.validate()?;
        let queue = || {
            Arc::new(Mutex::new(Queue {
                frames: VecDeque::new(),
                bytes: 0,
                closed: false,
            }))
        };
        let a = queue();
        let b = queue();
        Ok((
            Self {
                input: a.clone(),
                output: b.clone(),
                limits,
            },
            Self {
                input: b,
                output: a,
                limits,
            },
        ))
    }
}
impl Transport for MemoryTransport {
    fn send(&mut self, frame: Vec<u8>) -> Result<(), Fault> {
        let mut out = self.output.lock().map_err(|_| closed())?;
        if out.closed {
            return Err(closed());
        }
        if frame.is_empty()
            || frame.len() > self.limits.frame
            || out.frames.len() >= self.limits.queued_frames
            || frame.len() > self.limits.queued_bytes.saturating_sub(out.bytes)
        {
            return Err(Fault::resource_limit("transport output capacity"));
        }
        out.bytes += frame.len();
        out.frames.push_back(frame);
        Ok(())
    }
    fn receive(&mut self) -> Result<Option<Vec<u8>>, Fault> {
        let mut input = self.input.lock().map_err(|_| closed())?;
        if input.closed {
            return Err(closed());
        }
        let value = input.frames.pop_front();
        if let Some(v) = &value {
            input.bytes -= v.len();
        }
        Ok(value)
    }
    fn poll(&mut self) -> Result<(), Fault> {
        if self.input.lock().map_err(|_| closed())?.closed {
            Err(closed())
        } else {
            Ok(())
        }
    }
    fn close(&mut self) {
        for queue in [&self.input, &self.output] {
            if let Ok(mut q) = queue.lock() {
                q.closed = true;
                q.frames.clear();
                q.bytes = 0;
            }
        }
    }
}
impl Drop for MemoryTransport {
    fn drop(&mut self) {
        self.close();
    }
}
pub struct TcpTransport {
    stream: TcpStream,
    limits: TransportLimits,
    input: Vec<u8>,
    ready: VecDeque<Vec<u8>>,
    output: VecDeque<Vec<u8>>,
    offset: usize,
    queued: usize,
    ready_bytes: usize,
    closed: bool,
}
impl TcpTransport {
    pub fn new(stream: TcpStream, limits: TransportLimits) -> Result<Self, Fault> {
        limits.validate()?;
        stream.set_nonblocking(true).map_err(io_fault)?;
        stream.set_nodelay(true).map_err(io_fault)?;
        Ok(Self {
            stream,
            limits,
            input: Vec::new(),
            ready: VecDeque::new(),
            output: VecDeque::new(),
            offset: 0,
            queued: 0,
            ready_bytes: 0,
            closed: false,
        })
    }
    fn pump(&mut self) -> Result<(), Fault> {
        let mut remaining = self.limits.poll_bytes;
        let mut buffer = [0u8; 8192];
        let mut operations = 64;
        while remaining > 0 && operations > 0 {
            operations -= 1;
            let n = buffer.len().min(remaining);
            match self.stream.read(&mut buffer[..n]) {
                Ok(0) => return Err(closed()),
                Ok(n) => {
                    if n > self
                        .limits
                        .queued_bytes
                        .saturating_sub(self.input.len() + self.ready_bytes)
                    {
                        return Err(Fault::resource_limit("TCP incoming capacity"));
                    }
                    self.input.extend_from_slice(&buffer[..n]);
                    remaining -= n;
                    loop {
                        if self.input.len() < 4 {
                            break;
                        }
                        let len =
                            u32::from_le_bytes(self.input[..4].try_into().expect("four bytes"))
                                as usize;
                        if len == 0 || len > self.limits.frame {
                            return Err(Fault::invalid_data("TCP frame length"));
                        }
                        if self.input.len() < len + 4 {
                            break;
                        }
                        if self.ready.len() >= self.limits.queued_frames {
                            return Err(Fault::resource_limit("TCP incoming frame count"));
                        }
                        self.ready.push_back(self.input[4..4 + len].to_vec());
                        self.ready_bytes += len;
                        self.input.drain(..4 + len);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_fault(e)),
            }
        }
        let mut remaining = self.limits.poll_bytes;
        let mut operations = 64;
        while remaining > 0 && operations > 0 {
            operations -= 1;
            let Some(front) = self.output.front() else {
                break;
            };
            let end = front.len().min(self.offset + remaining);
            match self.stream.write(&front[self.offset..end]) {
                Ok(0) => return Err(closed()),
                Ok(n) => {
                    remaining -= n;
                    self.offset += n;
                    self.queued -= n;
                    if self.offset == front.len() {
                        self.output.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_fault(e)),
            }
        }
        Ok(())
    }
}
fn io_fault(e: std::io::Error) -> Fault {
    Fault::new(FaultCode::InvalidState, format!("transport I/O: {e}"))
}
impl Transport for TcpTransport {
    fn send(&mut self, frame: Vec<u8>) -> Result<(), Fault> {
        if self.closed {
            return Err(closed());
        }
        if frame.is_empty()
            || frame.len() > self.limits.frame
            || self.output.len() >= self.limits.queued_frames
            || frame.len() + 4 > self.limits.queued_bytes.saturating_sub(self.queued)
        {
            return Err(Fault::resource_limit("TCP output capacity"));
        }
        let mut packet = Vec::with_capacity(frame.len() + 4);
        packet.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        packet.extend(frame);
        self.queued += packet.len();
        self.output.push_back(packet);
        Ok(())
    }
    fn receive(&mut self) -> Result<Option<Vec<u8>>, Fault> {
        if self.closed {
            return Err(closed());
        }
        let frame = self.ready.pop_front();
        if let Some(v) = &frame {
            self.ready_bytes -= v.len();
        }
        Ok(frame)
    }
    fn poll(&mut self) -> Result<(), Fault> {
        if self.closed {
            return Err(closed());
        }
        let result = self.pump();
        if result.is_err() {
            self.close();
        }
        result
    }
    fn close(&mut self) {
        self.closed = true;
        self.input.clear();
        self.ready.clear();
        self.output.clear();
        self.queued = 0;
        self.ready_bytes = 0;
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
impl Drop for TcpTransport {
    fn drop(&mut self) {
        self.close();
    }
}
