#![forbid(unsafe_code)]

use std::fmt;
use std::net::SocketAddr;

use net_protocol::{Connect, Disconnect, InputCommand, ProtocolError, ProtocolMessage, Snapshot};
use net_transport::{Transport, TransportConfig, TransportError, TransportEvent, UdpTransport};

const CONTROL_CHANNEL: u8 = 0;
const INPUT_CHANNEL: u8 = 1;
const SNAPSHOT_CHANNEL: u8 = 2;

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
    transport: Box<dyn Transport>,
    server_addr: SocketAddr,
    client_id: u32,
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
        mut transport: Box<dyn Transport>,
        server_addr: SocketAddr,
        client_id: u32,
    ) -> Result<Self, ClientError> {
        transport.connect_peer(server_addr);
        let mut client = Self {
            transport,
            server_addr,
            client_id,
            next_seq: 0,
            next_tick: 0,
            last_snapshot: None,
        };
        client.send_control(ProtocolMessage::Connect(Connect { client_id }))?;
        client.transport.flush()?;
        Ok(client)
    }

    pub fn connect_udp(
        bind_addr: SocketAddr,
        server_addr: SocketAddr,
        transport: TransportConfig,
        client_id: u32,
    ) -> Result<Self, ClientError> {
        let transport = UdpTransport::bind(bind_addr, transport)?;
        Self::connect(Box::new(transport), server_addr, client_id)
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

    pub fn disconnect(&mut self) -> Result<(), ClientError> {
        self.send_control(ProtocolMessage::Disconnect(Disconnect {
            client_id: self.client_id,
        }))?;
        self.transport.flush()?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), ClientError> {
        let events = self.transport.poll()?;
        for event in events {
            let TransportEvent::Message {
                channel, payload, ..
            } = event;
            if channel != SNAPSHOT_CHANNEL {
                continue;
            }
            if let Ok(ProtocolMessage::Snapshot(snapshot)) = ProtocolMessage::decode(&payload) {
                self.last_snapshot = Some(snapshot);
            }
        }
        Ok(())
    }

    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last_snapshot.as_ref()
    }

    fn send_control(&mut self, message: ProtocolMessage) -> Result<(), ClientError> {
        let payload = message.encode()?;
        self.transport
            .send(self.server_addr, CONTROL_CHANNEL, payload)?;
        Ok(())
    }
}
