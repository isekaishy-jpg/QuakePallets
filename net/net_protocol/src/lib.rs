#![forbid(unsafe_code)]

use std::fmt;

const TYPE_INPUT: u8 = 1;
const TYPE_SNAPSHOT: u8 = 2;
const TYPE_DELTA_SNAPSHOT: u8 = 3;
const MAX_ENTITIES: usize = 2048;

#[derive(Clone, Debug, PartialEq)]
pub struct InputCommand {
    pub client_seq: u32,
    pub client_tick: u32,
    pub move_x: f32,
    pub move_y: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub buttons: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotEntity {
    pub net_id: u32,
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub yaw: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub server_tick: u32,
    pub ack_client_seq: u32,
    pub entities: Vec<SnapshotEntity>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeltaSnapshot {
    pub server_tick: u32,
    pub baseline_tick: u32,
    pub ack_client_seq: u32,
    pub entities: Vec<SnapshotEntity>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolMessage {
    Input(InputCommand),
    Snapshot(Snapshot),
    DeltaSnapshot(DeltaSnapshot),
}

#[derive(Debug)]
pub enum ProtocolError {
    Encode(String),
    Decode(String),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Encode(msg) => write!(f, "net protocol encode error: {}", msg),
            ProtocolError::Decode(msg) => write!(f, "net protocol decode error: {}", msg),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl ProtocolMessage {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        match self {
            ProtocolMessage::Input(cmd) => encode_input(cmd),
            ProtocolMessage::Snapshot(snapshot) => encode_snapshot(snapshot),
            ProtocolMessage::DeltaSnapshot(snapshot) => encode_delta_snapshot(snapshot),
        }
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProtocolError> {
        let (&msg_type, rest) = data
            .split_first()
            .ok_or_else(|| ProtocolError::Decode("packet too short".into()))?;
        match msg_type {
            TYPE_INPUT => decode_input(rest).map(ProtocolMessage::Input),
            TYPE_SNAPSHOT => decode_snapshot(rest).map(ProtocolMessage::Snapshot),
            TYPE_DELTA_SNAPSHOT => decode_delta_snapshot(rest).map(ProtocolMessage::DeltaSnapshot),
            _ => Err(ProtocolError::Decode(format!(
                "unknown message type {}",
                msg_type
            ))),
        }
    }
}

fn encode_input(cmd: &InputCommand) -> Result<Vec<u8>, ProtocolError> {
    let mut bytes = Vec::with_capacity(1 + 28);
    bytes.push(TYPE_INPUT);
    write_u32(&mut bytes, cmd.client_seq);
    write_u32(&mut bytes, cmd.client_tick);
    write_f32(&mut bytes, cmd.move_x);
    write_f32(&mut bytes, cmd.move_y);
    write_f32(&mut bytes, cmd.yaw);
    write_f32(&mut bytes, cmd.pitch);
    write_u32(&mut bytes, cmd.buttons);
    Ok(bytes)
}

fn decode_input(mut data: &[u8]) -> Result<InputCommand, ProtocolError> {
    let client_seq = read_u32(&mut data)?;
    let client_tick = read_u32(&mut data)?;
    let move_x = read_f32(&mut data)?;
    let move_y = read_f32(&mut data)?;
    let yaw = read_f32(&mut data)?;
    let pitch = read_f32(&mut data)?;
    let buttons = read_u32(&mut data)?;
    if !data.is_empty() {
        return Err(ProtocolError::Decode("input trailing bytes".into()));
    }
    Ok(InputCommand {
        client_seq,
        client_tick,
        move_x,
        move_y,
        yaw,
        pitch,
        buttons,
    })
}

fn encode_snapshot(snapshot: &Snapshot) -> Result<Vec<u8>, ProtocolError> {
    if snapshot.entities.len() > MAX_ENTITIES {
        return Err(ProtocolError::Encode(format!(
            "snapshot entity count {} exceeds {}",
            snapshot.entities.len(),
            MAX_ENTITIES
        )));
    }
    let mut bytes = Vec::with_capacity(1 + 10 + snapshot.entities.len() * 32);
    bytes.push(TYPE_SNAPSHOT);
    write_u32(&mut bytes, snapshot.server_tick);
    write_u32(&mut bytes, snapshot.ack_client_seq);
    write_u16(&mut bytes, snapshot.entities.len() as u16);
    for entity in &snapshot.entities {
        write_u32(&mut bytes, entity.net_id);
        for value in entity.position {
            write_f32(&mut bytes, value);
        }
        for value in entity.velocity {
            write_f32(&mut bytes, value);
        }
        write_f32(&mut bytes, entity.yaw);
    }
    Ok(bytes)
}

fn decode_snapshot(mut data: &[u8]) -> Result<Snapshot, ProtocolError> {
    let server_tick = read_u32(&mut data)?;
    let ack_client_seq = read_u32(&mut data)?;
    let count = read_u16(&mut data)? as usize;
    if count > MAX_ENTITIES {
        return Err(ProtocolError::Decode(format!(
            "snapshot entity count {} exceeds {}",
            count, MAX_ENTITIES
        )));
    }
    let mut entities = Vec::with_capacity(count);
    for _ in 0..count {
        let net_id = read_u32(&mut data)?;
        let mut position = [0.0; 3];
        for value in &mut position {
            *value = read_f32(&mut data)?;
        }
        let mut velocity = [0.0; 3];
        for value in &mut velocity {
            *value = read_f32(&mut data)?;
        }
        let yaw = read_f32(&mut data)?;
        entities.push(SnapshotEntity {
            net_id,
            position,
            velocity,
            yaw,
        });
    }
    if !data.is_empty() {
        return Err(ProtocolError::Decode("snapshot trailing bytes".into()));
    }
    Ok(Snapshot {
        server_tick,
        ack_client_seq,
        entities,
    })
}

fn encode_delta_snapshot(snapshot: &DeltaSnapshot) -> Result<Vec<u8>, ProtocolError> {
    if snapshot.entities.len() > MAX_ENTITIES {
        return Err(ProtocolError::Encode(format!(
            "snapshot entity count {} exceeds {}",
            snapshot.entities.len(),
            MAX_ENTITIES
        )));
    }
    let mut bytes = Vec::with_capacity(1 + 14 + snapshot.entities.len() * 32);
    bytes.push(TYPE_DELTA_SNAPSHOT);
    write_u32(&mut bytes, snapshot.server_tick);
    write_u32(&mut bytes, snapshot.baseline_tick);
    write_u32(&mut bytes, snapshot.ack_client_seq);
    write_u16(&mut bytes, snapshot.entities.len() as u16);
    for entity in &snapshot.entities {
        write_u32(&mut bytes, entity.net_id);
        for value in entity.position {
            write_f32(&mut bytes, value);
        }
        for value in entity.velocity {
            write_f32(&mut bytes, value);
        }
        write_f32(&mut bytes, entity.yaw);
    }
    Ok(bytes)
}

fn decode_delta_snapshot(mut data: &[u8]) -> Result<DeltaSnapshot, ProtocolError> {
    let server_tick = read_u32(&mut data)?;
    let baseline_tick = read_u32(&mut data)?;
    let ack_client_seq = read_u32(&mut data)?;
    let count = read_u16(&mut data)? as usize;
    if count > MAX_ENTITIES {
        return Err(ProtocolError::Decode(format!(
            "snapshot entity count {} exceeds {}",
            count, MAX_ENTITIES
        )));
    }
    let mut entities = Vec::with_capacity(count);
    for _ in 0..count {
        let net_id = read_u32(&mut data)?;
        let mut position = [0.0; 3];
        for value in &mut position {
            *value = read_f32(&mut data)?;
        }
        let mut velocity = [0.0; 3];
        for value in &mut velocity {
            *value = read_f32(&mut data)?;
        }
        let yaw = read_f32(&mut data)?;
        entities.push(SnapshotEntity {
            net_id,
            position,
            velocity,
            yaw,
        });
    }
    if !data.is_empty() {
        return Err(ProtocolError::Decode("delta snapshot trailing bytes".into()));
    }
    Ok(DeltaSnapshot {
        server_tick,
        baseline_tick,
        ack_client_seq,
        entities,
    })
}

fn write_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_f32(bytes: &mut Vec<u8>, value: f32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(data: &mut &[u8]) -> Result<u16, ProtocolError> {
    if data.len() < 2 {
        return Err(ProtocolError::Decode("unexpected eof".into()));
    }
    let value = u16::from_le_bytes([data[0], data[1]]);
    *data = &data[2..];
    Ok(value)
}

fn read_u32(data: &mut &[u8]) -> Result<u32, ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::Decode("unexpected eof".into()));
    }
    let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    *data = &data[4..];
    Ok(value)
}

fn read_f32(data: &mut &[u8]) -> Result<f32, ProtocolError> {
    if data.len() < 4 {
        return Err(ProtocolError::Decode("unexpected eof".into()));
    }
    let value = f32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    *data = &data[4..];
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_round_trip() {
        let input = InputCommand {
            client_seq: 7,
            client_tick: 42,
            move_x: 1.0,
            move_y: -0.5,
            yaw: 0.25,
            pitch: -0.75,
            buttons: 3,
        };
        let msg = ProtocolMessage::Input(input.clone());
        let encoded = msg.encode().expect("encode input");
        let decoded = ProtocolMessage::decode(&encoded).expect("decode input");
        assert_eq!(decoded, ProtocolMessage::Input(input));
    }

    #[test]
    fn snapshot_round_trip() {
        let snapshot = Snapshot {
            server_tick: 120,
            ack_client_seq: 9,
            entities: vec![
                SnapshotEntity {
                    net_id: 1,
                    position: [1.0, 2.0, 3.0],
                    velocity: [0.1, 0.2, 0.3],
                    yaw: 0.5,
                },
                SnapshotEntity {
                    net_id: 2,
                    position: [-1.0, -2.0, -3.0],
                    velocity: [-0.1, -0.2, -0.3],
                    yaw: -0.5,
                },
            ],
        };
        let msg = ProtocolMessage::Snapshot(snapshot.clone());
        let encoded = msg.encode().expect("encode snapshot");
        let decoded = ProtocolMessage::decode(&encoded).expect("decode snapshot");
        assert_eq!(decoded, ProtocolMessage::Snapshot(snapshot));
    }

    #[test]
    fn delta_snapshot_round_trip() {
        let snapshot = DeltaSnapshot {
            server_tick: 240,
            baseline_tick: 200,
            ack_client_seq: 10,
            entities: vec![SnapshotEntity {
                net_id: 4,
                position: [4.0, 5.0, 6.0],
                velocity: [0.4, 0.5, 0.6],
                yaw: 1.25,
            }],
        };
        let msg = ProtocolMessage::DeltaSnapshot(snapshot.clone());
        let encoded = msg.encode().expect("encode delta snapshot");
        let decoded = ProtocolMessage::decode(&encoded).expect("decode delta snapshot");
        assert_eq!(decoded, ProtocolMessage::DeltaSnapshot(snapshot));
    }
}
