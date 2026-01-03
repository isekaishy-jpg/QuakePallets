#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;

use net_protocol::{InputCommand, ProtocolError, ProtocolMessage, Snapshot, SnapshotEntity};
use net_transport::{TransportConfig, TransportError, TransportEvent, UdpTransport};

const INPUT_CHANNEL: u8 = 0;
const SNAPSHOT_CHANNEL: u8 = 1;
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
    transport: UdpTransport,
    tick: u32,
    snapshot_stride: u32,
    clients: HashMap<SocketAddr, ClientState>,
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
    pub fn bind(
        bind_addr: SocketAddr,
        transport: TransportConfig,
        snapshot_stride: u32,
    ) -> Result<Self, ServerError> {
        let transport = UdpTransport::bind(bind_addr, transport)?;
        Ok(Self {
            transport,
            tick: 0,
            snapshot_stride: snapshot_stride.max(1),
            clients: HashMap::new(),
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, ServerError> {
        Ok(self.transport.local_addr()?)
    }

    pub fn tick(&mut self) -> Result<(), ServerError> {
        let events = self.transport.poll()?;
        for event in events {
            let TransportEvent::Message {
                from,
                channel,
                payload,
            } = event;
            if channel != INPUT_CHANNEL {
                continue;
            }
            match ProtocolMessage::decode(&payload) {
                Ok(ProtocolMessage::Input(cmd)) => {
                    let client = self.clients.entry(from).or_insert_with(|| ClientState {
                        entity: EntityState::default(),
                        last_input: None,
                        last_seq: 0,
                    });
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

        if self.tick % self.snapshot_stride == 0 {
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
        }

        self.tick = self.tick.wrapping_add(1);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::{Client, ClientInput};
    use net_transport::TransportConfig;

    #[test]
    fn loopback_exchanges_snapshots() {
        let transport = TransportConfig::default();
        let bind_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut server = Server::bind(bind_addr, transport.clone(), 1).expect("server bind");
        let server_addr = server.local_addr().expect("server addr");

        let client_bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut client =
            Client::connect(client_bind, server_addr, transport).expect("client connect");

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
    }
}
