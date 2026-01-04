#![forbid(unsafe_code)]

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;

use net_protocol::{
    Connect, Disconnect, InputCommand, ProtocolError, ProtocolMessage, Snapshot, SnapshotEntity,
};
use net_transport::{Transport, TransportConfig, TransportError, TransportEvent, UdpTransport};

const CONTROL_CHANNEL: u8 = 0;
const INPUT_CHANNEL: u8 = 1;
const SNAPSHOT_CHANNEL: u8 = 2;
const FIXED_DT: f32 = 1.0 / 60.0;
const MOVE_SPEED: f32 = 320.0;

#[derive(Clone, Debug)]
struct EntityState {
    position: [f32; 3],
    velocity: [f32; 3],
    yaw: f32,
}

impl Default for EntityState {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0, 0.0],
            velocity: [0.0, 0.0, 0.0],
            yaw: 0.0,
        }
    }
}

struct ClientState {
    entity: EntityState,
    last_input: Option<InputCommand>,
    last_seq: u32,
}

pub struct Server {
    transport: Box<dyn Transport>,
    tick: u32,
    snapshot_stride: u32,
    clients: HashMap<SocketAddr, ClientState>,
}

pub struct TickReport {
    pub new_clients: usize,
    pub snapshots_sent: usize,
}

#[derive(Debug)]
pub enum ServerError {
    Transport(TransportError),
    Protocol(ProtocolError),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::Transport(err) => write!(f, "server transport error: {}", err),
            ServerError::Protocol(err) => write!(f, "server protocol error: {}", err),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<TransportError> for ServerError {
    fn from(err: TransportError) -> Self {
        ServerError::Transport(err)
    }
}

impl From<ProtocolError> for ServerError {
    fn from(err: ProtocolError) -> Self {
        ServerError::Protocol(err)
    }
}

impl Server {
    pub fn bind(transport: Box<dyn Transport>, snapshot_stride: u32) -> Result<Self, ServerError> {
        Ok(Self {
            transport,
            tick: 0,
            snapshot_stride: snapshot_stride.max(1),
            clients: HashMap::new(),
        })
    }

    pub fn bind_udp(
        bind_addr: SocketAddr,
        transport: TransportConfig,
        snapshot_stride: u32,
    ) -> Result<Self, ServerError> {
        let transport = UdpTransport::bind(bind_addr, transport)?;
        Self::bind(Box::new(transport), snapshot_stride)
    }

    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        Ok(self.transport.local_addr()?)
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn tick(&mut self) -> Result<TickReport, ServerError> {
        let mut report = TickReport {
            new_clients: 0,
            snapshots_sent: 0,
        };
        let events = self.transport.poll()?;
        for event in events {
            let TransportEvent::Message {
                from,
                channel,
                payload,
            } = event;
            match ProtocolMessage::decode(&payload) {
                Ok(ProtocolMessage::Connect(connect)) if channel == CONTROL_CHANNEL => {
                    self.register_client(from, connect);
                }
                Ok(ProtocolMessage::Disconnect(disconnect)) if channel == CONTROL_CHANNEL => {
                    self.unregister_client(from, disconnect);
                }
                Ok(ProtocolMessage::Input(cmd)) if channel == INPUT_CHANNEL => {
                    let client = match self.clients.entry(from) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => {
                            report.new_clients += 1;
                            entry.insert(ClientState {
                                entity: EntityState::default(),
                                last_input: None,
                                last_seq: 0,
                            })
                        }
                    };
                    if cmd.client_seq >= client.last_seq {
                        client.last_seq = cmd.client_seq;
                        client.last_input = Some(cmd);
                    }
                }
                _ => {}
            }
        }

        for client in self.clients.values_mut() {
            if let Some(input) = &client.last_input {
                client.entity.velocity[0] = input.move_x * MOVE_SPEED;
                client.entity.velocity[2] = input.move_y * MOVE_SPEED;
                client.entity.yaw = input.yaw;
            }
            client.entity.position[0] += client.entity.velocity[0] * FIXED_DT;
            client.entity.position[2] += client.entity.velocity[2] * FIXED_DT;
        }

        if self.tick.is_multiple_of(self.snapshot_stride) {
            let entities: Vec<SnapshotEntity> = self
                .clients
                .iter()
                .enumerate()
                .map(|(index, (_addr, client))| SnapshotEntity {
                    net_id: index as u32 + 1,
                    position: client.entity.position,
                    velocity: client.entity.velocity,
                    yaw: client.entity.yaw,
                })
                .collect();

            for (addr, client) in self.clients.iter() {
                let snapshot = Snapshot {
                    server_tick: self.tick,
                    ack_client_seq: client.last_seq,
                    entities: entities.clone(),
                };
                let payload = ProtocolMessage::Snapshot(snapshot).encode()?;
                self.transport.send(*addr, SNAPSHOT_CHANNEL, payload)?;
            }
            self.transport.flush()?;
            report.snapshots_sent = self.clients.len();
        }

        self.tick = self.tick.wrapping_add(1);
        Ok(report)
    }

    fn register_client(&mut self, addr: SocketAddr, _connect: Connect) {
        if let Entry::Vacant(entry) = self.clients.entry(addr) {
            entry.insert(ClientState {
                entity: EntityState::default(),
                last_input: None,
                last_seq: 0,
            });
        }
    }

    fn unregister_client(&mut self, addr: SocketAddr, _disconnect: Disconnect) {
        self.clients.remove(&addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::{Client, ClientInput};
    use net_transport::{LoopbackTransport, TransportConfig};

    #[test]
    fn loopback_exchanges_snapshots() {
        let transport = TransportConfig::default();
        let mut server_transport =
            LoopbackTransport::bind(transport.clone()).expect("loopback bind");
        let mut client_transport = LoopbackTransport::bind(transport).expect("loopback bind");
        let server_addr = server_transport.local_addr().expect("server addr");
        let client_addr = client_transport.local_addr().expect("client addr");
        server_transport.connect_peer(client_addr);
        client_transport.connect_peer(server_addr);

        let mut server = Server::bind(Box::new(server_transport), 1).expect("server bind");
        let mut client =
            Client::connect(Box::new(client_transport), server_addr, 1).expect("client connect");

        for _ in 0..5 {
            client
                .send_input(ClientInput {
                    move_x: 1.0,
                    move_y: 0.0,
                    yaw: 0.1,
                    pitch: 0.0,
                    buttons: 0,
                })
                .expect("send input");
            server.tick().expect("server tick");
            client.poll().expect("client poll");
        }

        assert!(client.last_snapshot().is_some());
        client.disconnect().expect("disconnect");
        server.tick().expect("server tick");
        assert_eq!(server.client_count(), 0);
    }
}
