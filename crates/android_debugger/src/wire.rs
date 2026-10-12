//! Bounded, synchronous JDWP transport. The session owner schedules calls off the UI thread.
//!
//! Polling retains partial frames and takes at most a short socket-read budget. Commands
//! have an absolute deadline, acknowledge reply-bearing target commands, and retain composite events
//! in FIFO order. A transport or framing failure closes the connection; a well-formed
//! JDWP error reply remains recoverable by the caller. No production thread is spawned.

use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::VecDeque,
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const HANDSHAKE: &[u8; 14] = b"JDWP-Handshake";
const HEADER_BYTES: usize = 11;
pub const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024;
const MAX_BODY_BYTES: usize = MAX_PACKET_BYTES - HEADER_BYTES;
const MAX_PENDING_EVENTS: usize = 1024;
const MAX_PENDING_EVENT_BYTES: usize = MAX_PACKET_BYTES;
const MAX_COLLECTION_ITEMS: usize = 1_000_000;
const POLL_BUDGET: Duration = Duration::from_millis(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdSizes {
    pub field: usize,
    pub method: usize,
    pub object: usize,
    pub reference_type: usize,
    pub frame: usize,
}

impl IdSizes {
    pub fn validate(self) -> Result<Self> {
        for width in [
            self.field,
            self.method,
            self.object,
            self.reference_type,
            self.frame,
        ] {
            validate_id_width(width)?;
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Location {
    pub type_tag: u8,
    pub class: u64,
    pub method: u64,
    pub index: u64,
}

/// A valid target error reply, distinct from a transport/framing failure.
#[derive(Debug)]
pub struct JdwpError {
    pub code: u16,
    pub request_id: u32,
    pub set: u8,
    pub command: u8,
}

impl fmt::Display for JdwpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "JDWP command {}.{} (request {}) returned error {}",
            self.set, self.command, self.request_id, self.code
        )
    }
}

impl std::error::Error for JdwpError {}

pub fn error_code(error: &anyhow::Error) -> Option<u16> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<JdwpError>().map(|error| error.code))
}

pub fn is_absent_information(error: &anyhow::Error) -> bool {
    error_code(error) == Some(101)
}

#[derive(Debug)]
struct Disconnected;

impl fmt::Display for Disconnected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JDWP target closed the connection")
    }
}

impl std::error::Error for Disconnected {}

/// A truncated frame, malformed packet or expired deadline is not a clean EOF.
pub fn is_disconnected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<Disconnected>()
            || cause.downcast_ref::<io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::NotConnected
                )
            })
    })
}

fn validate_id_width(width: usize) -> Result<()> {
    ensure!(
        (1..=8).contains(&width),
        "Invalid JDWP ID width {width}; expected 1..=8"
    );
    Ok(())
}

fn validate_id(value: u64, width: usize) -> Result<()> {
    validate_id_width(width)?;
    ensure!(
        width == 8 || value >> (width * 8) == 0,
        "JDWP ID does not fit its {width}-byte width"
    );
    Ok(())
}

#[derive(Default)]
pub struct Encoder {
    data: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn reserve(&mut self, bytes: usize) -> Result<()> {
        let needed = self
            .data
            .len()
            .checked_add(bytes)
            .context("JDWP encoder length overflow")?;
        ensure!(
            needed <= MAX_BODY_BYTES,
            "JDWP encoded body exceeds packet limit"
        );
        if needed > self.data.capacity() {
            // Geometric growth, capped at the packet budget, avoids reallocating per ID.
            let capacity = needed.next_power_of_two().min(MAX_BODY_BYTES);
            self.data
                .try_reserve_exact(capacity - self.data.len())
                .context("Cannot allocate JDWP body")?;
        }
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) -> Result<&mut Self> {
        self.reserve(bytes.len())?;
        self.data.extend_from_slice(bytes);
        Ok(self)
    }

    pub fn u8(&mut self, value: u8) -> Result<&mut Self> {
        self.append(&[value])
    }
    pub fn u16(&mut self, value: u16) -> Result<&mut Self> {
        self.append(&value.to_be_bytes())
    }
    pub fn u32(&mut self, value: u32) -> Result<&mut Self> {
        self.append(&value.to_be_bytes())
    }
    pub fn i32(&mut self, value: i32) -> Result<&mut Self> {
        self.append(&value.to_be_bytes())
    }
    pub fn u64(&mut self, value: u64) -> Result<&mut Self> {
        self.append(&value.to_be_bytes())
    }

    pub fn id(&mut self, value: u64, width: usize) -> Result<&mut Self> {
        validate_id(value, width)?;
        self.append(&value.to_be_bytes()[8 - width..])
    }

    pub fn string(&mut self, value: &str) -> Result<&mut Self> {
        let length = i32::try_from(value.len()).context("JDWP string length overflow")?;
        self.reserve(
            4usize
                .checked_add(value.len())
                .context("JDWP string length overflow")?,
        )?;
        self.data.extend_from_slice(&length.to_be_bytes());
        self.data.extend_from_slice(value.as_bytes());
        Ok(self)
    }

    pub fn location(&mut self, value: Location, sizes: IdSizes) -> Result<&mut Self> {
        sizes.validate()?;
        validate_id(value.class, sizes.reference_type)?;
        validate_id(value.method, sizes.method)?;
        self.reserve(1 + sizes.reference_type + sizes.method + 8)?;
        self.data.push(value.type_tag);
        self.data
            .extend_from_slice(&value.class.to_be_bytes()[8 - sizes.reference_type..]);
        self.data
            .extend_from_slice(&value.method.to_be_bytes()[8 - sizes.method..]);
        self.data.extend_from_slice(&value.index.to_be_bytes());
        Ok(self)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }
}

pub struct Decoder<'a> {
    data: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, cursor: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.cursor
    }

    pub fn bytes(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .context("JDWP decoder length overflow")?;
        ensure!(
            end <= self.data.len(),
            "Truncated JDWP payload: need {length} bytes, have {}",
            self.remaining()
        );
        let result = &self.data[self.cursor..end];
        self.cursor = end;
        Ok(result)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.bytes(N)?);
        Ok(bytes)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.fixed::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.fixed()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.fixed()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    pub fn id(&mut self, width: usize) -> Result<u64> {
        validate_id_width(width)?;
        let mut bytes = [0; 8];
        bytes[8 - width..].copy_from_slice(self.bytes(width)?);
        Ok(u64::from_be_bytes(bytes))
    }

    /// Validate a count before the caller allocates a collection.
    pub fn count(&mut self, minimum_item_bytes: usize) -> Result<usize> {
        ensure!(minimum_item_bytes > 0, "JDWP item minimum must be positive");
        let count = usize::try_from(self.i32()?).context("Negative JDWP collection count")?;
        ensure!(
            count <= MAX_COLLECTION_ITEMS,
            "JDWP collection count exceeds limit"
        );
        ensure!(
            count <= self.remaining() / minimum_item_bytes,
            "JDWP collection count exceeds remaining payload"
        );
        Ok(count)
    }

    pub fn string(&mut self) -> Result<&'a str> {
        let length = usize::try_from(self.i32()?).context("Negative JDWP string length")?;
        ensure!(
            length <= MAX_BODY_BYTES - 4,
            "JDWP string exceeds packet limit"
        );
        std::str::from_utf8(self.bytes(length)?).context("Invalid UTF-8 in JDWP string")
    }

    pub fn location(&mut self, sizes: IdSizes) -> Result<Location> {
        sizes.validate()?;
        Ok(Location {
            type_tag: self.u8()?,
            class: self.id(sizes.reference_type)?,
            method: self.id(sizes.method)?,
            index: self.u64()?,
        })
    }

    pub fn finish(self) -> Result<()> {
        ensure!(
            self.remaining() == 0,
            "Trailing JDWP payload: {} bytes",
            self.remaining()
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PacketKind {
    Reply { error: u16 },
    Command { set: u8, command: u8 },
}

#[derive(Clone, Copy)]
struct Header {
    id: u32,
    body_bytes: usize,
    kind: PacketKind,
}

impl Header {
    fn decode(bytes: &[u8; HEADER_BYTES]) -> Result<Self> {
        let mut decoder = Decoder::new(bytes);
        let length = decoder.u32()? as usize;
        ensure!(
            (HEADER_BYTES..=MAX_PACKET_BYTES).contains(&length),
            "Invalid JDWP packet length {length}"
        );
        let id = decoder.u32()?;
        let kind = match decoder.u8()? {
            0 => PacketKind::Command {
                set: decoder.u8()?,
                command: decoder.u8()?,
            },
            0x80 => PacketKind::Reply {
                error: decoder.u16()?,
            },
            flags => bail!("Invalid JDWP packet flags {flags:#x}"),
        };
        decoder.finish()?;
        Ok(Self {
            id,
            body_bytes: length - HEADER_BYTES,
            kind,
        })
    }
}

struct Packet {
    header: Header,
    body: Vec<u8>,
}

#[derive(Default)]
struct Incoming {
    header_bytes: [u8; HEADER_BYTES],
    header_read: usize,
    header: Option<Header>,
    body: Vec<u8>,
    body_read: usize,
}

enum ReadProgress {
    Bytes(usize),
    TimedOut,
    Eof,
}

fn deadline(timeout: Duration) -> Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .context("JDWP timeout is too large")
}

fn read_progress(stream: &mut TcpStream, bytes: &mut [u8], end: Instant) -> Result<ReadProgress> {
    loop {
        let Some(remaining) = end
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
        else {
            return Ok(ReadProgress::TimedOut);
        };
        stream
            .set_read_timeout(Some(remaining))
            .context("Set JDWP read timeout")?;
        match stream.read(bytes) {
            Ok(0) => return Ok(ReadProgress::Eof),
            Ok(length) => return Ok(ReadProgress::Bytes(length)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(ReadProgress::TimedOut);
            }
            Err(error) => return Err(error).context("Read JDWP transport"),
        }
    }
}

fn write_all(stream: &mut TcpStream, mut bytes: &[u8], end: Instant) -> Result<()> {
    while !bytes.is_empty() {
        let remaining = end
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .context("JDWP write timed out")?;
        stream
            .set_write_timeout(Some(remaining))
            .context("Set JDWP write timeout")?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "JDWP write made no progress",
                ))
                .context("Write JDWP transport");
            }
            Ok(length) => bytes = &bytes[length..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("Write JDWP transport"),
        }
    }
    Ok(())
}

pub struct Wire {
    stream: TcpStream,
    timeout: Duration,
    sizes: IdSizes,
    next_id: u32,
    incoming: Incoming,
    events: VecDeque<Vec<u8>>,
    event_bytes: usize,
    failed: Option<String>,
}

impl Wire {
    /// A well-formed target error reply leaves the connection usable; framing,
    /// timeout and transport failures close it and cannot be retried.
    pub(crate) fn is_usable(&self) -> bool {
        self.failed.is_none()
    }

    /// Numeric IPv4/IPv6 and localhost are accepted. DNS is deliberately delegated
    /// to the session owner: std DNS resolution cannot have a finite timeout here
    /// without creating a resolver thread. Android ADB forwarding uses localhost.
    pub fn attach(host: &str, port: u16, timeout: Duration) -> Result<Self> {
        ensure!(port != 0, "JDWP port must be nonzero");
        ensure!(
            !timeout.is_zero() && timeout <= MAX_TIMEOUT,
            "JDWP timeout must be in (0, 120s]"
        );
        let end = deadline(timeout)?;
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let addresses = if host.eq_ignore_ascii_case("localhost") {
            vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
            ]
        } else {
            vec![SocketAddr::new(
                host.parse::<IpAddr>()
                    .context("JDWP host must be a numeric IP or localhost")?,
                port,
            )]
        };
        let mut stream = None;
        let mut last_error = None;
        for address in addresses {
            let remaining = end
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .context("JDWP connect timed out")?;
            match TcpStream::connect_timeout(&address, remaining) {
                Ok(connection) => {
                    stream = Some(connection);
                    break;
                }
                Err(error) => last_error = Some(error),
            }
        }
        let mut stream = stream.ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot connect to JDWP target: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_default()
            )
        })?;
        stream.set_nodelay(true).context("Set JDWP TCP_NODELAY")?;
        write_all(&mut stream, HANDSHAKE, end)?;
        let mut response = [0; HANDSHAKE.len()];
        let mut received = 0;
        while received < response.len() {
            match read_progress(&mut stream, &mut response[received..], end)? {
                ReadProgress::Bytes(length) => received += length,
                ReadProgress::TimedOut => bail!("JDWP handshake timed out"),
                ReadProgress::Eof if received == 0 => return Err(Disconnected.into()),
                ReadProgress::Eof => bail!("Truncated JDWP handshake"),
            }
        }
        ensure!(&response == HANDSHAKE, "Target rejected JDWP handshake");
        let mut wire = Self {
            stream,
            timeout,
            sizes: IdSizes {
                field: 8,
                method: 8,
                object: 8,
                reference_type: 8,
                frame: 8,
            },
            next_id: 1,
            incoming: Incoming::default(),
            events: VecDeque::new(),
            event_bytes: 0,
            failed: None,
        };
        let body = wire.command_inner(1, 7, Vec::new(), end)?;
        let mut decoder = Decoder::new(&body);
        let mut width = || usize::try_from(decoder.i32()?).context("Negative JDWP ID width");
        wire.sizes = IdSizes {
            field: width()?,
            method: width()?,
            object: width()?,
            reference_type: width()?,
            frame: width()?,
        }
        .validate()?;
        decoder.finish()?;
        Ok(wire)
    }

    pub fn ids(&self) -> IdSizes {
        self.sizes
    }

    fn healthy(&self) -> Result<()> {
        if let Some(reason) = &self.failed {
            bail!("JDWP connection is unusable after: {reason}");
        }
        Ok(())
    }

    fn fail(&mut self, error: &anyhow::Error) {
        self.failed = Some(format!("{error:#}"));
        self.incoming = Incoming::default();
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    fn receive(&mut self, end: Instant) -> Result<Option<Packet>> {
        while self.incoming.header_read < HEADER_BYTES {
            match read_progress(
                &mut self.stream,
                &mut self.incoming.header_bytes[self.incoming.header_read..],
                end,
            )? {
                ReadProgress::Bytes(length) => self.incoming.header_read += length,
                ReadProgress::TimedOut => return Ok(None),
                ReadProgress::Eof if self.incoming.header_read == 0 => {
                    return Err(Disconnected.into());
                }
                ReadProgress::Eof => bail!("Truncated JDWP packet header on EOF"),
            }
        }
        if self.incoming.header.is_none() {
            let header = Header::decode(&self.incoming.header_bytes)?;
            self.incoming
                .body
                .try_reserve_exact(header.body_bytes)
                .context("Cannot allocate JDWP packet")?;
            self.incoming.body.resize(header.body_bytes, 0);
            self.incoming.header = Some(header);
        }
        while self.incoming.body_read < self.incoming.body.len() {
            match read_progress(
                &mut self.stream,
                &mut self.incoming.body[self.incoming.body_read..],
                end,
            )? {
                ReadProgress::Bytes(length) => self.incoming.body_read += length,
                ReadProgress::TimedOut => return Ok(None),
                ReadProgress::Eof => bail!("Truncated JDWP packet body on EOF"),
            }
        }
        let header = self.incoming.header.context("Missing JDWP header")?;
        let body = std::mem::take(&mut self.incoming.body);
        self.incoming = Incoming::default();
        Ok(Some(Packet { header, body }))
    }

    fn acknowledge(&mut self, id: u32, error: u16, end: Instant) -> Result<()> {
        let mut bytes = [0; HEADER_BYTES];
        bytes[..4].copy_from_slice(&(HEADER_BYTES as u32).to_be_bytes());
        bytes[4..8].copy_from_slice(&id.to_be_bytes());
        bytes[8] = 0x80;
        bytes[9..].copy_from_slice(&error.to_be_bytes());
        write_all(&mut self.stream, &bytes, end)
    }

    fn target_command(&mut self, packet: Packet, end: Instant) -> Result<()> {
        match packet.header.kind {
            PacketKind::Command {
                set: 64,
                command: 100,
            } => {
                ensure!(
                    self.events.len() < MAX_PENDING_EVENTS,
                    "JDWP pending event count exceeds limit"
                );
                let total = self
                    .event_bytes
                    .checked_add(packet.body.len())
                    .context("JDWP event bytes overflow")?;
                ensure!(
                    total <= MAX_PENDING_EVENT_BYTES,
                    "JDWP pending event bytes exceed limit"
                );
                // JDWP Event.Composite is unsolicited and has no reply packet.
                self.event_bytes = total;
                self.events.push_back(packet.body);
                Ok(())
            }
            PacketKind::Command { set, command } => {
                self.acknowledge(packet.header.id, 99, end)?;
                bail!("Unsupported JDWP target command {set}.{command}")
            }
            PacketKind::Reply { .. } => {
                bail!("Unexpected JDWP reply without an outstanding request")
            }
        }
    }

    fn pop_event(&mut self) -> Option<Vec<u8>> {
        let event = self.events.pop_front()?;
        self.event_bytes -= event.len();
        Some(event)
    }

    fn command_inner(
        &mut self,
        set: u8,
        command: u8,
        body: Vec<u8>,
        end: Instant,
    ) -> Result<Vec<u8>> {
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "JDWP command body exceeds packet limit"
        );
        let id = self.next_id;
        self.next_id = id.checked_add(1).context("JDWP request IDs exhausted")?;
        let mut header = [0; HEADER_BYTES];
        header[..4].copy_from_slice(&((HEADER_BYTES + body.len()) as u32).to_be_bytes());
        header[4..8].copy_from_slice(&id.to_be_bytes());
        header[9] = set;
        header[10] = command;
        self.stream
            .set_read_timeout(Some(self.timeout))
            .context("Restore JDWP command read timeout")?;
        self.stream
            .set_write_timeout(Some(self.timeout))
            .context("Restore JDWP command write timeout")?;
        write_all(&mut self.stream, &header, end)?;
        write_all(&mut self.stream, &body, end)?;
        drop(body);
        loop {
            let packet = self.receive(end)?.context("JDWP command timed out")?;
            match packet.header.kind {
                PacketKind::Reply { error } => {
                    ensure!(
                        packet.header.id == id,
                        "JDWP reply ID {} does not match request {id}",
                        packet.header.id
                    );
                    if error != 0 {
                        return Err(JdwpError {
                            code: error,
                            request_id: id,
                            set,
                            command,
                        }
                        .into());
                    }
                    return Ok(packet.body);
                }
                PacketKind::Command { .. } => self.target_command(packet, end)?,
            }
        }
    }

    pub fn command(&mut self, set: u8, command: u8, body: Vec<u8>) -> Result<Vec<u8>> {
        self.healthy()?;
        let result = self.command_inner(set, command, body, deadline(self.timeout)?);
        if let Err(error) = &result {
            if error_code(error).is_none() {
                self.fail(error);
            }
        }
        result
    }

    /// All commands in one DAP operation share its absolute deadline. A target
    /// cannot extend that operation by replying just before each socket timeout.
    pub fn command_until(
        &mut self,
        set: u8,
        command: u8,
        body: Vec<u8>,
        end: Instant,
    ) -> Result<Vec<u8>> {
        self.healthy()?;
        let end = end.min(deadline(self.timeout)?);
        let result = self.command_inner(set, command, body, end);
        if let Err(error) = &result {
            if error_code(error).is_none() {
                self.fail(error);
            }
        }
        result
    }

    pub fn poll_event(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(event) = self.pop_event() {
            return Ok(Some(event));
        }
        self.healthy()?;
        let result = (|| {
            let Some(packet) = self.receive(deadline(POLL_BUDGET.min(self.timeout))?)? else {
                return Ok(None);
            };
            self.target_command(packet, deadline(POLL_BUDGET.min(self.timeout))?)?;
            Ok(self.pop_event())
        })();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread};

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    fn spawn_target(f: impl FnOnce(TcpStream) + Send + 'static) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let end = Instant::now() + TEST_TIMEOUT;
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < end,
                            "Mock target did not receive a connection"
                        );
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("Mock accept failed: {error}"),
                }
            };
            stream.set_read_timeout(Some(TEST_TIMEOUT)).unwrap();
            stream.set_write_timeout(Some(TEST_TIMEOUT)).unwrap();
            stream.set_nodelay(true).unwrap();
            f(stream);
        });
        (port, handle)
    }

    fn read_packet(stream: &mut TcpStream) -> Packet {
        let mut raw = [0; HEADER_BYTES];
        stream.read_exact(&mut raw).unwrap();
        let header = Header::decode(&raw).unwrap();
        let mut body = vec![0; header.body_bytes];
        stream.read_exact(&mut body).unwrap();
        Packet { header, body }
    }

    fn packet(id: u32, kind: PacketKind, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((HEADER_BYTES + body.len()) as u32).to_be_bytes());
        bytes.extend_from_slice(&id.to_be_bytes());
        match kind {
            PacketKind::Reply { error } => {
                bytes.push(0x80);
                bytes.extend_from_slice(&error.to_be_bytes());
            }
            PacketKind::Command { set, command } => bytes.extend_from_slice(&[0, set, command]),
        }
        bytes.extend_from_slice(body);
        bytes
    }

    fn handshake(stream: &mut TcpStream, sizes: [i32; 5]) {
        let mut hello = [0; 14];
        stream.read_exact(&mut hello).unwrap();
        assert_eq!(&hello, HANDSHAKE);
        stream.write_all(HANDSHAKE).unwrap();
        let request = read_packet(stream);
        assert!(matches!(
            request.header.kind,
            PacketKind::Command { set: 1, command: 7 }
        ));
        assert!(request.body.is_empty());
        let body: Vec<u8> = sizes.into_iter().flat_map(i32::to_be_bytes).collect();
        stream
            .write_all(&packet(
                request.header.id,
                PacketKind::Reply { error: 0 },
                &body,
            ))
            .unwrap();
    }

    fn event(sequence: u32) -> Vec<u8> {
        // VMDeath: suspendPolicy, event count, kind, requestID.
        let mut body = vec![0, 0, 0, 0, 1, 99];
        body.extend_from_slice(&sequence.to_be_bytes());
        body
    }

    #[test]
    fn codec_preserves_big_endian_values_dynamic_ids_strings_and_locations() {
        let sizes = IdSizes {
            field: 1,
            method: 2,
            object: 3,
            reference_type: 4,
            frame: 8,
        };
        let location = Location {
            type_tag: 1,
            class: 0x10203040,
            method: 0x5060,
            index: u64::MAX,
        };
        let mut encoder = Encoder::new();
        encoder
            .u8(0xfe)
            .unwrap()
            .u16(0xabcd)
            .unwrap()
            .u32(0x12345678)
            .unwrap()
            .i32(-42)
            .unwrap()
            .u64(u64::MAX)
            .unwrap()
            .id(0x102030, sizes.object)
            .unwrap()
            .string("λ🙂")
            .unwrap()
            .location(location, sizes)
            .unwrap();
        assert_eq!(
            &encoder.as_bytes()[..7],
            &[0xfe, 0xab, 0xcd, 0x12, 0x34, 0x56, 0x78]
        );
        let data = encoder.into_bytes();
        let mut decoder = Decoder::new(&data);
        assert_eq!(decoder.u8().unwrap(), 0xfe);
        assert_eq!(decoder.u16().unwrap(), 0xabcd);
        assert_eq!(decoder.u32().unwrap(), 0x12345678);
        assert_eq!(decoder.i32().unwrap(), -42);
        assert_eq!(decoder.u64().unwrap(), u64::MAX);
        assert_eq!(decoder.id(sizes.object).unwrap(), 0x102030);
        assert_eq!(decoder.string().unwrap(), "λ🙂");
        assert_eq!(decoder.location(sizes).unwrap(), location);
        decoder.finish().unwrap();
        for width in 1..=8 {
            let value = if width == 8 {
                u64::MAX
            } else {
                (1u64 << (width * 8)) - 1
            };
            let mut encoder = Encoder::new();
            encoder.id(value, width).unwrap();
            assert_eq!(Decoder::new(encoder.as_bytes()).id(width).unwrap(), value);
        }
    }

    #[test]
    fn codec_rejects_bad_counts_strings_ids_truncation_and_trailing_bytes() {
        assert!(Decoder::new(&[0, 0, 0]).u32().is_err());
        assert!(Decoder::new(&[1]).finish().is_err());
        for count in [-1i32, 2, i32::MAX] {
            let mut bytes = count.to_be_bytes().to_vec();
            bytes.push(0);
            assert!(Decoder::new(&bytes).count(1).is_err());
        }
        assert!(Decoder::new(&[0, 0, 0, 0]).count(0).is_err());
        assert_eq!(Decoder::new(&[0, 0, 0, 2, 8, 9]).count(1).unwrap(), 2);
        assert!(Decoder::new(&[255, 255, 255, 255]).string().is_err());
        assert!(Decoder::new(&[0, 0, 0, 2, b'a']).string().is_err());
        assert!(Decoder::new(&[0, 0, 0, 1, 0xff]).string().is_err());
        for width in [0, 9, usize::MAX] {
            assert!(Encoder::new().id(0, width).is_err());
            assert!(Decoder::new(&[]).id(width).is_err());
        }
        assert!(Encoder::new().id(256, 1).is_err());
        let mut encoder = Encoder::new();
        encoder.u8(1).unwrap();
        assert!(encoder.reserve(MAX_BODY_BYTES).is_err());
        assert_eq!(encoder.into_bytes(), [1]);
    }

    #[test]
    fn attach_rejects_handshake_and_invalid_dynamic_ids() {
        let (port, server) = spawn_target(|mut stream| {
            let mut hello = [0; 14];
            stream.read_exact(&mut hello).unwrap();
            stream.write_all(b"JDWP-Rejected!").unwrap();
        });
        assert!(Wire::attach("127.0.0.1", port, TEST_TIMEOUT).is_err());
        server.join().unwrap();
        let (port, server) = spawn_target(|mut stream| handshake(&mut stream, [8, 8, 0, 8, 8]));
        assert!(Wire::attach("localhost", port, TEST_TIMEOUT).is_err());
        server.join().unwrap();
        assert!(Wire::attach("127.0.0.1", 0, TEST_TIMEOUT).is_err());
        assert!(Wire::attach("127.0.0.1", 1, Duration::ZERO).is_err());
        assert!(Wire::attach("unresolved.example", 1, TEST_TIMEOUT).is_err());
    }

    #[test]
    fn interleaved_events_have_no_reply_and_are_returned_in_order() {
        let (port, server) = spawn_target(|mut stream| {
            handshake(&mut stream, [1, 2, 3, 4, 8]);
            let request = read_packet(&mut stream);
            assert!(matches!(
                request.header.kind,
                PacketKind::Command { set: 2, command: 3 }
            ));
            assert_eq!(request.body, [4, 5]);
            for id in [700, 701] {
                stream
                    .write_all(&packet(
                        id,
                        PacketKind::Command {
                            set: 64,
                            command: 100,
                        },
                        &event(id),
                    ))
                    .unwrap();
            }
            stream
                .write_all(&packet(
                    request.header.id,
                    PacketKind::Reply { error: 0 },
                    &[9, 8, 7],
                ))
                .unwrap();
            // Any event reply would precede this legitimate next request on TCP.
            let next = read_packet(&mut stream);
            assert!(matches!(
                next.header.kind,
                PacketKind::Command { set: 1, command: 4 }
            ));
            assert!(next.body.is_empty());
            assert_ne!(next.header.id, request.header.id);
            stream
                .write_all(&packet(
                    next.header.id,
                    PacketKind::Reply { error: 0 },
                    &[6],
                ))
                .unwrap();
        });
        let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
        assert_eq!(
            wire.ids(),
            IdSizes {
                field: 1,
                method: 2,
                object: 3,
                reference_type: 4,
                frame: 8
            }
        );
        assert_eq!(wire.command(2, 3, vec![4, 5]).unwrap(), [9, 8, 7]);
        assert_eq!(wire.poll_event().unwrap(), Some(event(700)));
        assert_eq!(wire.poll_event().unwrap(), Some(event(701)));
        assert_eq!(wire.command(1, 4, Vec::new()).unwrap(), [6]);
        server.join().unwrap();
    }

    #[test]
    fn polling_retains_partial_header_and_body_before_a_command() {
        for split in [5, HEADER_BYTES + 4] {
            let (ready, observed) = mpsc::sync_channel(1);
            let (release, released) = mpsc::sync_channel(1);
            let (port, server) = spawn_target(move |mut stream| {
                handshake(&mut stream, [8; 5]);
                let bytes = packet(
                    800,
                    PacketKind::Command {
                        set: 64,
                        command: 100,
                    },
                    &event(800),
                );
                stream.write_all(&bytes[..split]).unwrap();
                ready.send(()).unwrap();
                released.recv_timeout(TEST_TIMEOUT).unwrap();
                stream.write_all(&bytes[split..]).unwrap();
                let request = read_packet(&mut stream);
                assert!(matches!(
                    request.header.kind,
                    PacketKind::Command { set: 1, command: 4 }
                ));
                stream
                    .write_all(&packet(
                        request.header.id,
                        PacketKind::Reply { error: 0 },
                        &[42],
                    ))
                    .unwrap();
                let next = read_packet(&mut stream);
                assert!(matches!(
                    next.header.kind,
                    PacketKind::Command { set: 1, command: 1 }
                ));
                assert!(next.body.is_empty());
                stream
                    .write_all(&packet(
                        next.header.id,
                        PacketKind::Reply { error: 0 },
                        &[43],
                    ))
                    .unwrap();
            });
            let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
            observed.recv_timeout(TEST_TIMEOUT).unwrap();
            assert_eq!(wire.poll_event().unwrap(), None);
            if split < HEADER_BYTES {
                assert_eq!(wire.incoming.header_read, split);
            } else {
                assert_eq!(wire.incoming.body_read, split - HEADER_BYTES);
            }
            release.send(()).unwrap();
            assert_eq!(wire.command(1, 4, Vec::new()).unwrap(), [42]);
            assert_eq!(wire.poll_event().unwrap(), Some(event(800)));
            assert_eq!(wire.command(1, 1, Vec::new()).unwrap(), [43]);
            server.join().unwrap();
        }
    }

    #[test]
    fn target_error_codes_are_typed_and_do_not_destroy_the_connection() {
        let (port, server) = spawn_target(|mut stream| {
            handshake(&mut stream, [8; 5]);
            let first = read_packet(&mut stream);
            stream
                .write_all(&packet(
                    first.header.id,
                    PacketKind::Reply { error: 101 },
                    &[],
                ))
                .unwrap();
            let next = read_packet(&mut stream);
            stream
                .write_all(&packet(
                    next.header.id,
                    PacketKind::Reply { error: 0 },
                    &[1],
                ))
                .unwrap();
        });
        let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
        let error = wire
            .command(2, 12, Vec::new())
            .unwrap_err()
            .context("source metadata");
        assert_eq!(error_code(&error), Some(101));
        assert!(is_absent_information(&error));
        assert!(!is_disconnected(&error));
        assert_eq!(wire.command(1, 1, Vec::new()).unwrap(), [1]);
        for code in [99, 102, 511] {
            let error: anyhow::Error = JdwpError {
                code,
                request_id: 1,
                set: 2,
                command: 12,
            }
            .into();
            assert_eq!(error_code(&error), Some(code));
            assert!(!is_absent_information(&error));
        }
        server.join().unwrap();
    }

    #[test]
    fn invalid_packet_lengths_flags_and_reply_ids_close_the_transport() {
        for (length, flags, wrong_id) in [
            (10, 0x80, false),
            ((MAX_PACKET_BYTES + 1) as u32, 0x80, false),
            (11, 1, false),
            (11, 0x80, true),
        ] {
            let (port, server) = spawn_target(move |mut stream| {
                handshake(&mut stream, [8; 5]);
                let request = read_packet(&mut stream);
                let id = request.header.id + u32::from(wrong_id);
                let mut raw = packet(id, PacketKind::Reply { error: 0 }, &[]);
                raw[..4].copy_from_slice(&length.to_be_bytes());
                raw[8] = flags;
                stream.write_all(&raw).unwrap();
            });
            let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
            let error = wire.command(1, 1, Vec::new()).unwrap_err();
            assert!(!is_disconnected(&error));
            assert!(wire.failed.is_some());
            assert!(wire.command(1, 1, Vec::new()).is_err());
            assert_eq!(wire.incoming.body.len(), 0);
            server.join().unwrap();
        }
    }

    #[test]
    fn clean_eof_is_distinct_from_truncated_frames() {
        for prefix in [0, 5, HEADER_BYTES + 2] {
            let (port, server) = spawn_target(move |mut stream| {
                handshake(&mut stream, [8; 5]);
                let bytes = packet(
                    9,
                    PacketKind::Command {
                        set: 64,
                        command: 100,
                    },
                    &event(9),
                );
                stream.write_all(&bytes[..prefix]).unwrap();
            });
            let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
            let end = Instant::now() + TEST_TIMEOUT;
            let error = loop {
                match wire.poll_event() {
                    Err(error) => break error,
                    Ok(None) => assert!(Instant::now() < end, "Expected target EOF"),
                    Ok(Some(_)) => panic!("Truncated event must not be published"),
                }
            };
            assert_eq!(is_disconnected(&error), prefix == 0);
            server.join().unwrap();
        }
    }

    #[test]
    fn event_queue_overflow_is_an_error_instead_of_silent_event_loss() {
        let (port, server) = spawn_target(|mut stream| {
            handshake(&mut stream, [8; 5]);
            let _request = read_packet(&mut stream);
            for index in 0..=MAX_PENDING_EVENTS {
                stream
                    .write_all(&packet(
                        index as u32 + 100,
                        PacketKind::Command {
                            set: 64,
                            command: 100,
                        },
                        &event(index as u32),
                    ))
                    .unwrap();
            }
        });
        let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
        let error = wire.command(1, 1, Vec::new()).unwrap_err();
        assert!(error.to_string().contains("pending event count"));
        assert!(!is_disconnected(&error));
        for index in 0..MAX_PENDING_EVENTS {
            assert_eq!(wire.poll_event().unwrap(), Some(event(index as u32)));
        }
        assert!(wire.poll_event().is_err());
        server.join().unwrap();
    }

    #[test]
    fn unsupported_reply_bearing_target_command_gets_an_explicit_error_reply() {
        let (port, server) = spawn_target(|mut stream| {
            handshake(&mut stream, [8; 5]);
            let _request = read_packet(&mut stream);
            stream
                .write_all(&packet(
                    500,
                    PacketKind::Command { set: 2, command: 7 },
                    &[],
                ))
                .unwrap();
            let ack = read_packet(&mut stream);
            assert_eq!(ack.header.id, 500);
            assert!(matches!(ack.header.kind, PacketKind::Reply { error: 99 }));
            assert!(ack.body.is_empty());
        });
        let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
        let error = wire.command(1, 1, Vec::new()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Unsupported JDWP target command 2.7")
        );
        assert!(!is_disconnected(&error));
        server.join().unwrap();
    }

    #[test]
    fn command_timeout_closes_transport_without_claiming_clean_eof() {
        let (port, server) = spawn_target(|mut stream| {
            handshake(&mut stream, [8; 5]);
            let _request = read_packet(&mut stream);
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "Timed-out client should close its transport"
            );
        });
        let mut wire = Wire::attach("127.0.0.1", port, TEST_TIMEOUT).unwrap();
        wire.timeout = Duration::from_millis(40);
        let error = wire.command(1, 1, Vec::new()).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(!is_disconnected(&error));
        assert!(wire.command(1, 1, Vec::new()).is_err());
        server.join().unwrap();
    }
}
