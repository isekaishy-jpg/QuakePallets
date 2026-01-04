#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

const HEADER_SIZE: usize = 4 + 2 + 2 + 4 + 1;
const MESSAGE_HEADER_SIZE: usize = 1 + 1 + 2 + 2;
const MAX_SENT_PACKETS: usize = 256;
const SEQ_WINDOW: u16 = 0x8000;
const RELIABLE_FLAG: u8 = 1 << 0;
const SEQUENCED_FLAG: u8 = 1 << 1;

type LoopbackQueue = Arc<Mutex<VecDeque<TransportEvent>>>;
type LoopbackRegistry = Mutex<HashMap<SocketAddr, LoopbackQueue>>;

#[derive(Clone, Copy, Debug)]
pub enum ChannelKind {
    ReliableOrdered,
    UnreliableSequenced,
    Unreliable,
}

#[derive(Clone, Copy, Debug)]
pub struct ChannelConfig {
    pub kind: ChannelKind,
    pub max_pending: usize,
}

impl ChannelConfig {
    pub fn reliable() -> Self {
        Self {
            kind: ChannelKind::ReliableOrdered,
            max_pending: 128,
        }
    }

    pub fn unreliable() -> Self {
        Self {
            kind: ChannelKind::Unreliable,
            max_pending: 128,
        }
    }

    pub fn sequenced() -> Self {
        Self {
            kind: ChannelKind::UnreliableSequenced,
            max_pending: 8,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub protocol_id: u32,
    pub mtu: usize,
    pub channels: Vec<ChannelConfig>,
}

impl TransportConfig {
    pub fn new(protocol_id: u32, mtu: usize, channels: Vec<ChannelConfig>) -> Self {
        Self {
            protocol_id,
            mtu: mtu.max(HEADER_SIZE + MESSAGE_HEADER_SIZE + 1),
            channels,
        }
    }
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            protocol_id: 0x5155_414B,
            mtu: 1200,
            channels: vec![
                ChannelConfig::reliable(),
                ChannelConfig::sequenced(),
                ChannelConfig::unreliable(),
            ],
        }
    }
}

#[derive(Debug)]
pub enum TransportError {
    Io(std::io::Error),
    Encode(String),
    Channel(String),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Io(err) => write!(f, "net transport io error: {}", err),
            TransportError::Encode(err) => write!(f, "net transport encode error: {}", err),
            TransportError::Channel(err) => write!(f, "net transport channel error: {}", err),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<std::io::Error> for TransportError {
    fn from(err: std::io::Error) -> Self {
        TransportError::Io(err)
    }
}

#[derive(Debug)]
pub enum TransportEvent {
    Message {
        from: SocketAddr,
        channel: u8,
        payload: Vec<u8>,
    },
}

pub trait Transport {
    fn local_addr(&self) -> Result<SocketAddr, TransportError>;
    fn connect_peer(&mut self, addr: SocketAddr);
    fn send(
        &mut self,
        addr: SocketAddr,
        channel: u8,
        payload: Vec<u8>,
    ) -> Result<(), TransportError>;
    fn flush(&mut self) -> Result<(), TransportError>;
    fn poll(&mut self) -> Result<Vec<TransportEvent>, TransportError>;
    fn mtu(&self) -> usize;
    fn now_ms(&self) -> u64;
}

pub struct UdpTransport {
    socket: UdpSocket,
    config: TransportConfig,
    peers: HashMap<SocketAddr, PeerState>,
    recv_buf: Vec<u8>,
    start: Instant,
}

impl UdpTransport {
    pub fn bind(addr: SocketAddr, config: TransportConfig) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_nonblocking(true)?;
        let recv_len = config.mtu.max(1500);
        Ok(Self {
            socket,
            config,
            peers: HashMap::new(),
            recv_buf: vec![0u8; recv_len],
            start: Instant::now(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.socket.local_addr()?)
    }

    pub fn connect_peer(&mut self, addr: SocketAddr) {
        self.peers
            .entry(addr)
            .or_insert_with(|| PeerState::new(&self.config.channels));
    }

    pub fn send(
        &mut self,
        addr: SocketAddr,
        channel: u8,
        payload: Vec<u8>,
    ) -> Result<(), TransportError> {
        if payload.len() + HEADER_SIZE + MESSAGE_HEADER_SIZE > self.config.mtu {
            return Err(TransportError::Encode(format!(
                "payload size {} exceeds mtu {}",
                payload.len(),
                self.config.mtu
            )));
        }
        let peer = self
            .peers
            .entry(addr)
            .or_insert_with(|| PeerState::new(&self.config.channels));
        peer.enqueue(channel, payload)
    }

    pub fn flush(&mut self) -> Result<(), TransportError> {
        let mtu = self.config.mtu;
        let protocol_id = self.config.protocol_id;
        let mut to_send: Vec<(SocketAddr, Vec<u8>)> = Vec::new();

        for (addr, peer) in self.peers.iter_mut() {
            if let Some(packet) = peer.build_packet(protocol_id, mtu) {
                to_send.push((*addr, packet.bytes));
                peer.track_sent(packet.sequence, packet.reliable_refs);
            }
        }

        for (addr, bytes) in to_send {
            let _ = self.socket.send_to(&bytes, addr)?;
        }

        Ok(())
    }

    pub fn poll(&mut self) -> Result<Vec<TransportEvent>, TransportError> {
        let mut events = Vec::new();

        loop {
            let (len, from) = match self.socket.recv_from(&mut self.recv_buf) {
                Ok(result) => result,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => return Err(TransportError::Io(err)),
            };

            let packet = &self.recv_buf[..len];
            let decoded = match decode_packet(packet, self.config.protocol_id) {
                Ok(Some(packet)) => packet,
                Ok(None) => continue,
                Err(_) => continue,
            };

            let peer = self
                .peers
                .entry(from)
                .or_insert_with(|| PeerState::new(&self.config.channels));
            peer.process_acks(decoded.ack, decoded.ack_bits);
            let is_new = peer.track_received(decoded.sequence);
            if !is_new {
                continue;
            }

            for msg in decoded.messages {
                peer.receive_message(from, msg, &mut events);
            }
        }

        Ok(events)
    }

    pub fn mtu(&self) -> usize {
        self.config.mtu
    }

    pub fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Transport for UdpTransport {
    fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.local_addr()
    }

    fn connect_peer(&mut self, addr: SocketAddr) {
        self.connect_peer(addr);
    }

    fn send(
        &mut self,
        addr: SocketAddr,
        channel: u8,
        payload: Vec<u8>,
    ) -> Result<(), TransportError> {
        self.send(addr, channel, payload)
    }

    fn flush(&mut self) -> Result<(), TransportError> {
        self.flush()
    }

    fn poll(&mut self) -> Result<Vec<TransportEvent>, TransportError> {
        self.poll()
    }

    fn mtu(&self) -> usize {
        self.mtu()
    }

    fn now_ms(&self) -> u64 {
        self.now_ms()
    }
}

pub struct LoopbackTransport {
    addr: SocketAddr,
    config: TransportConfig,
    peers: HashMap<SocketAddr, LoopbackQueue>,
    inbox: LoopbackQueue,
    start: Instant,
}

impl LoopbackTransport {
    pub fn bind(config: TransportConfig) -> Result<Self, TransportError> {
        let addr = next_loopback_addr();
        let inbox = Arc::new(Mutex::new(VecDeque::new()));
        loopback_registry()
            .lock()
            .expect("loopback registry poisoned")
            .insert(addr, Arc::clone(&inbox));
        Ok(Self {
            addr,
            config,
            peers: HashMap::new(),
            inbox,
            start: Instant::now(),
        })
    }
}

impl Drop for LoopbackTransport {
    fn drop(&mut self) {
        if let Ok(mut registry) = loopback_registry().lock() {
            registry.remove(&self.addr);
        }
    }
}

impl Transport for LoopbackTransport {
    fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.addr)
    }

    fn connect_peer(&mut self, addr: SocketAddr) {
        if let Some(queue) = loopback_registry()
            .lock()
            .ok()
            .and_then(|registry| registry.get(&addr).cloned())
        {
            self.peers.insert(addr, queue);
        }
    }

    fn send(
        &mut self,
        addr: SocketAddr,
        channel: u8,
        payload: Vec<u8>,
    ) -> Result<(), TransportError> {
        if payload.len() + HEADER_SIZE + MESSAGE_HEADER_SIZE > self.config.mtu {
            return Err(TransportError::Encode(format!(
                "payload size {} exceeds mtu {}",
                payload.len(),
                self.config.mtu
            )));
        }
        let queue = self.peers.get(&addr).ok_or_else(|| {
            TransportError::Channel(format!("loopback peer {} not connected", addr))
        })?;
        let mut queue = queue.lock().expect("loopback queue poisoned");
        queue.push_back(TransportEvent::Message {
            from: self.addr,
            channel,
            payload,
        });
        Ok(())
    }

    fn flush(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn poll(&mut self) -> Result<Vec<TransportEvent>, TransportError> {
        let mut events = Vec::new();
        let mut inbox = self.inbox.lock().expect("loopback inbox poisoned");
        while let Some(event) = inbox.pop_front() {
            events.push(event);
        }
        Ok(events)
    }

    fn mtu(&self) -> usize {
        self.config.mtu
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

fn loopback_registry() -> &'static LoopbackRegistry {
    static REGISTRY: OnceLock<LoopbackRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_loopback_addr() -> SocketAddr {
    static NEXT_PORT: AtomicU16 = AtomicU16::new(40000);
    let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
}

struct PacketBytes {
    bytes: Vec<u8>,
    sequence: u16,
    reliable_refs: Vec<ReliableRef>,
}

#[derive(Clone, Copy)]
struct ReliableRef {
    channel: u8,
    id: u16,
}

#[derive(Clone)]
struct OutgoingMessage {
    id: u16,
    payload: Vec<u8>,
}

enum SendChannel {
    ReliableOrdered {
        next_id: u16,
        pending: VecDeque<OutgoingMessage>,
        max_pending: usize,
    },
    UnreliableSequenced {
        next_id: u16,
        pending: Option<OutgoingMessage>,
        max_pending: usize,
    },
    Unreliable {
        pending: VecDeque<OutgoingMessage>,
        max_pending: usize,
    },
}

enum RecvChannel {
    ReliableOrdered {
        expected_id: u16,
        buffer: BTreeMap<u16, Vec<u8>>,
        max_pending: usize,
    },
    UnreliableSequenced {
        last_id: Option<u16>,
    },
    Unreliable,
}

struct SentPacket {
    sequence: u16,
    reliable_refs: Vec<ReliableRef>,
}

struct PeerState {
    next_sequence: u16,
    last_received: Option<u16>,
    received_mask: u32,
    send_channels: Vec<SendChannel>,
    recv_channels: Vec<RecvChannel>,
    sent_packets: VecDeque<SentPacket>,
}

impl PeerState {
    fn new(channels: &[ChannelConfig]) -> Self {
        let mut send_channels = Vec::with_capacity(channels.len());
        let mut recv_channels = Vec::with_capacity(channels.len());
        for cfg in channels {
            send_channels.push(match cfg.kind {
                ChannelKind::ReliableOrdered => SendChannel::ReliableOrdered {
                    next_id: 0,
                    pending: VecDeque::new(),
                    max_pending: cfg.max_pending,
                },
                ChannelKind::UnreliableSequenced => SendChannel::UnreliableSequenced {
                    next_id: 0,
                    pending: None,
                    max_pending: cfg.max_pending,
                },
                ChannelKind::Unreliable => SendChannel::Unreliable {
                    pending: VecDeque::new(),
                    max_pending: cfg.max_pending,
                },
            });

            recv_channels.push(match cfg.kind {
                ChannelKind::ReliableOrdered => RecvChannel::ReliableOrdered {
                    expected_id: 0,
                    buffer: BTreeMap::new(),
                    max_pending: cfg.max_pending,
                },
                ChannelKind::UnreliableSequenced => {
                    RecvChannel::UnreliableSequenced { last_id: None }
                }
                ChannelKind::Unreliable => RecvChannel::Unreliable,
            });
        }

        Self {
            next_sequence: 0,
            last_received: None,
            received_mask: 0,
            send_channels,
            recv_channels,
            sent_packets: VecDeque::new(),
        }
    }

    fn enqueue(&mut self, channel: u8, payload: Vec<u8>) -> Result<(), TransportError> {
        let channel_idx = usize::from(channel);
        let send_channel = self
            .send_channels
            .get_mut(channel_idx)
            .ok_or_else(|| TransportError::Channel(format!("channel {} out of range", channel)))?;

        match send_channel {
            SendChannel::ReliableOrdered {
                next_id,
                pending,
                max_pending,
            } => {
                if pending.len() >= *max_pending {
                    return Err(TransportError::Channel(format!(
                        "channel {} pending overflow",
                        channel
                    )));
                }
                let id = *next_id;
                *next_id = next_id.wrapping_add(1);
                pending.push_back(OutgoingMessage { id, payload });
            }
            SendChannel::UnreliableSequenced {
                next_id,
                pending,
                max_pending,
            } => {
                if pending.is_some() && *max_pending == 0 {
                    return Ok(());
                }
                let id = *next_id;
                *next_id = next_id.wrapping_add(1);
                *pending = Some(OutgoingMessage { id, payload });
            }
            SendChannel::Unreliable {
                pending,
                max_pending,
            } => {
                if pending.len() >= *max_pending {
                    return Err(TransportError::Channel(format!(
                        "channel {} pending overflow",
                        channel
                    )));
                }
                pending.push_back(OutgoingMessage { id: 0, payload });
            }
        }

        Ok(())
    }

    fn build_packet(&mut self, protocol_id: u32, mtu: usize) -> Option<PacketBytes> {
        if !self.has_pending() {
            return None;
        }

        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let ack = self.last_received.unwrap_or(0);
        let ack_bits = self.received_mask;

        let mut bytes = Vec::with_capacity(mtu);
        bytes.extend_from_slice(&protocol_id.to_le_bytes());
        bytes.extend_from_slice(&sequence.to_le_bytes());
        bytes.extend_from_slice(&ack.to_le_bytes());
        bytes.extend_from_slice(&ack_bits.to_le_bytes());
        bytes.push(0);

        let mut msg_count = 0u8;
        let mut reliable_refs = Vec::new();

        for (channel_id, channel) in self.send_channels.iter_mut().enumerate() {
            let channel_id = channel_id as u8;
            match channel {
                SendChannel::ReliableOrdered { pending, .. } => {
                    for msg in pending.iter() {
                        if !fits_packet(&bytes, msg.payload.len(), mtu) {
                            break;
                        }
                        encode_message(&mut bytes, channel_id, RELIABLE_FLAG, msg.id, &msg.payload);
                        msg_count = msg_count.saturating_add(1);
                        reliable_refs.push(ReliableRef {
                            channel: channel_id,
                            id: msg.id,
                        });
                    }
                }
                SendChannel::UnreliableSequenced { pending, .. } => {
                    if let Some(msg) = pending.take() {
                        if fits_packet(&bytes, msg.payload.len(), mtu) {
                            encode_message(
                                &mut bytes,
                                channel_id,
                                SEQUENCED_FLAG,
                                msg.id,
                                &msg.payload,
                            );
                            msg_count = msg_count.saturating_add(1);
                        } else {
                            *pending = Some(msg);
                        }
                    }
                }
                SendChannel::Unreliable { pending, .. } => {
                    while let Some(msg) = pending.front() {
                        if !fits_packet(&bytes, msg.payload.len(), mtu) {
                            break;
                        }
                        let msg = pending.pop_front().expect("front exists");
                        encode_message(&mut bytes, channel_id, 0, 0, &msg.payload);
                        msg_count = msg_count.saturating_add(1);
                    }
                }
            }
        }

        if msg_count == 0 {
            return None;
        }

        bytes[HEADER_SIZE - 1] = msg_count;

        Some(PacketBytes {
            bytes,
            sequence,
            reliable_refs,
        })
    }

    fn track_sent(&mut self, sequence: u16, reliable_refs: Vec<ReliableRef>) {
        if reliable_refs.is_empty() {
            return;
        }
        self.sent_packets.push_back(SentPacket {
            sequence,
            reliable_refs,
        });
        while self.sent_packets.len() > MAX_SENT_PACKETS {
            self.sent_packets.pop_front();
        }
    }

    fn process_acks(&mut self, ack: u16, ack_bits: u32) {
        let mut acked = Vec::new();
        self.sent_packets.retain(|packet| {
            if packet_acked(packet.sequence, ack, ack_bits) {
                acked.extend(packet.reliable_refs.iter().copied());
                false
            } else {
                true
            }
        });
        for reliable in acked {
            self.ack_reliable(reliable);
        }
    }

    fn ack_reliable(&mut self, reliable: ReliableRef) {
        let channel_idx = reliable.channel as usize;
        if let Some(SendChannel::ReliableOrdered { pending, .. }) =
            self.send_channels.get_mut(channel_idx)
        {
            pending.retain(|msg| msg.id != reliable.id);
        }
    }

    fn track_received(&mut self, sequence: u16) -> bool {
        match self.last_received {
            None => {
                self.last_received = Some(sequence);
                self.received_mask = 0;
                true
            }
            Some(last) => {
                if sequence == last {
                    return false;
                }
                let diff = sequence.wrapping_sub(last);
                if diff < SEQ_WINDOW {
                    self.received_mask = if diff > 32 {
                        0
                    } else {
                        (self.received_mask << diff) | 1
                    };
                    self.last_received = Some(sequence);
                    true
                } else {
                    let back = last.wrapping_sub(sequence);
                    if (1..=32).contains(&back) {
                        self.received_mask |= 1 << (back - 1);
                    }
                    false
                }
            }
        }
    }

    fn receive_message(
        &mut self,
        from: SocketAddr,
        msg: DecodedMessage,
        events: &mut Vec<TransportEvent>,
    ) {
        let channel_idx = msg.channel as usize;
        let channel = match self.recv_channels.get_mut(channel_idx) {
            Some(channel) => channel,
            None => return,
        };

        match channel {
            RecvChannel::ReliableOrdered {
                expected_id,
                buffer,
                max_pending,
            } => {
                if msg.id == *expected_id {
                    events.push(TransportEvent::Message {
                        from,
                        channel: msg.channel,
                        payload: msg.payload,
                    });
                    *expected_id = expected_id.wrapping_add(1);
                    while let Some(payload) = buffer.remove(expected_id) {
                        events.push(TransportEvent::Message {
                            from,
                            channel: msg.channel,
                            payload,
                        });
                        *expected_id = expected_id.wrapping_add(1);
                    }
                } else if sequence_more_recent(msg.id, *expected_id) && buffer.len() < *max_pending
                {
                    buffer.insert(msg.id, msg.payload);
                }
            }
            RecvChannel::UnreliableSequenced { last_id } => {
                let accept = match last_id {
                    None => true,
                    Some(last) => sequence_more_recent(msg.id, *last),
                };
                if accept {
                    *last_id = Some(msg.id);
                    events.push(TransportEvent::Message {
                        from,
                        channel: msg.channel,
                        payload: msg.payload,
                    });
                }
            }
            RecvChannel::Unreliable => {
                events.push(TransportEvent::Message {
                    from,
                    channel: msg.channel,
                    payload: msg.payload,
                });
            }
        }
    }

    fn has_pending(&self) -> bool {
        self.send_channels.iter().any(|channel| match channel {
            SendChannel::ReliableOrdered { pending, .. } => !pending.is_empty(),
            SendChannel::UnreliableSequenced { pending, .. } => pending.is_some(),
            SendChannel::Unreliable { pending, .. } => !pending.is_empty(),
        })
    }
}

fn fits_packet(current: &[u8], payload_len: usize, mtu: usize) -> bool {
    current
        .len()
        .checked_add(MESSAGE_HEADER_SIZE + payload_len)
        .map(|size| size <= mtu)
        .unwrap_or(false)
}

fn encode_message(bytes: &mut Vec<u8>, channel: u8, flags: u8, id: u16, payload: &[u8]) {
    bytes.push(channel);
    bytes.push(flags);
    bytes.extend_from_slice(&id.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    bytes.extend_from_slice(payload);
}

fn decode_packet(data: &[u8], protocol_id: u32) -> Result<Option<DecodedPacket>, DecodeError> {
    if data.len() < HEADER_SIZE {
        return Err(DecodeError::new("packet too small"));
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != protocol_id {
        return Ok(None);
    }
    let sequence = u16::from_le_bytes([data[4], data[5]]);
    let ack = u16::from_le_bytes([data[6], data[7]]);
    let ack_bits = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let msg_count = data[12] as usize;

    let mut offset = HEADER_SIZE;
    let mut messages = Vec::with_capacity(msg_count);
    for _ in 0..msg_count {
        if offset + MESSAGE_HEADER_SIZE > data.len() {
            return Err(DecodeError::new("message header truncated"));
        }
        let channel = data[offset];
        let _flags = data[offset + 1];
        let id = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        let len = u16::from_le_bytes([data[offset + 4], data[offset + 5]]) as usize;
        offset += MESSAGE_HEADER_SIZE;
        if offset + len > data.len() {
            return Err(DecodeError::new("message payload truncated"));
        }
        let payload = data[offset..offset + len].to_vec();
        offset += len;
        messages.push(DecodedMessage {
            channel,
            id,
            payload,
        });
    }
    if offset != data.len() {
        return Err(DecodeError::new("packet trailing data"));
    }

    Ok(Some(DecodedPacket {
        sequence,
        ack,
        ack_bits,
        messages,
    }))
}

fn packet_acked(sequence: u16, ack: u16, ack_bits: u32) -> bool {
    if sequence == ack {
        return true;
    }
    let diff = ack.wrapping_sub(sequence);
    if diff == 0 || diff > 32 {
        return false;
    }
    ((ack_bits >> (diff - 1)) & 1) == 1
}

fn sequence_more_recent(a: u16, b: u16) -> bool {
    let diff = a.wrapping_sub(b);
    diff != 0 && diff < SEQ_WINDOW
}

struct DecodedPacket {
    sequence: u16,
    ack: u16,
    ack_bits: u32,
    messages: Vec<DecodedMessage>,
}

struct DecodedMessage {
    channel: u8,
    id: u16,
    payload: Vec<u8>,
}

#[derive(Debug)]
struct DecodeError {
    message: String,
}

impl DecodeError {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_bits_track_recent_packets() {
        let mut peer = PeerState::new(&[ChannelConfig::reliable()]);
        assert!(peer.track_received(10));
        assert_eq!(peer.last_received, Some(10));
        assert_eq!(peer.received_mask, 0);
        assert!(peer.track_received(11));
        assert_eq!(peer.last_received, Some(11));
        assert_eq!(peer.received_mask, 1);
        assert!(!peer.track_received(10));
        assert_eq!(peer.received_mask & 1, 1);
    }

    #[test]
    fn sequence_more_recent_handles_wrap() {
        assert!(sequence_more_recent(1, 0));
        assert!(sequence_more_recent(0, u16::MAX));
        assert!(!sequence_more_recent(0, 1));
    }
}
