#![forbid(unsafe_code)]

use std::fmt;
use std::net::SocketAddr;

use net_protocol::{InputCommand, ProtocolError, ProtocolMessage, Snapshot};
use net_transport::{TransportConfig, TransportError, TransportEvent, UdpTransport};

const INPUT_CHANNEL: u8 = 0;
const SNAPSHOT_CHANNEL: u8 = 1;

#[derive(Clone, Copy, Debug)]
pub struct ClientInput {
    pub move_x: f32,
    pub move_y: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub buttons: u32,
}

impl Default for ClientInput {
    fn default() -> Self {
        Self {
            move_x: 0.0,
            move_y: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            buttons: 0,
        }
    }
}

pub struct Client {
    transport: UdpTransport,
    server_addr: SocketAddr,
    next_seq: u32,
    next_tick: u32,
    last_snapshot: Option<Snapshot>,
}

#[derive(Debug)]
pub enum ClientError {
    Transport(TransportError),
    Protocol(ProtocolError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Transport(err) => write!(f, "client transport error: {}", err),
            ClientError::Protocol(err) => write!(f, "client protocol error: {}", err),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<TransportError> for ClientError {
    fn from(err: TransportError) -> Self {
        ClientError::Transport(err)
    }
}

impl From<ProtocolError> for ClientError {
    fn from(err: ProtocolError) -> Self {
        ClientError::Protocol(err)
    }
}

impl Client {
    pub fn connect(
        bind_addr: SocketAddr,
        server_addr: SocketAddr,
        transport: TransportConfig,
    ) -> Result<Self, ClientError> {
        let mut transport = UdpTransport::bind(bind_addr, transport)?;
        transport.connect_peer(server_addr);
        Ok(Self {
            transport,
            server_addr,
            next_seq: 0,
            next_tick: 0,
            last_snapshot: None,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, ClientError> {
        Ok(self.transport.local_addr()?)
    }

    pub fn send_input(&mut self, input: ClientInput) -> Result<(), ClientError> {
        let cmd = InputCommand {
            client_seq: self.next_seq,
            client_tick: self.next_tick,
            move_x: input.move_x,
            move_y: input.move_y,
            yaw: input.yaw,
            pitch: input.pitch,
            buttons: input.buttons,
        };
        self.next_seq = self.next_seq.wrapping_add(1);
        self.next_tick = self.next_tick.wrapping_add(1);

        let payload = ProtocolMessage::Input(cmd).encode()?;
        self.transport
            .send(self.server_addr, INPUT_CHANNEL, payload)?;
        self.transport.flush()?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), ClientError> {
        let events = self.transport.poll()?;
        for event in events {
            let TransportEvent::Message { channel, payload, .. } = event;
            if channel != SNAPSHOT_CHANNEL {
                continue;
            }
            match ProtocolMessage::decode(&payload) {
                Ok(ProtocolMessage::Snapshot(snapshot)) => {
                    self.last_snapshot = Some(snapshot);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last_snapshot.as_ref()
    }
}
