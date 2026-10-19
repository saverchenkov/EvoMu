//! Wire-level Protobuf encoder/decoder for Quasar (`publicGatewaySvc`) `eve_public.*` frames.

use super::{ProtoError, MAX_FRAME_SIZE};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtoWireValue {
    Varint(u64),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Fixed32(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuasarGatewayEnvelope {
    pub type_url: String,
    pub correlation_id: u64,
    pub status_code: u32,
    pub payload: Vec<u8>,
}

pub fn write_varint(mut val: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if val == 0 {
            break;
        }
    }
}

pub fn read_varint(input: &mut &[u8]) -> Result<u64, ProtoError> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        if input.is_empty() || shift >= 64 {
            return Err(ProtoError::UnexpectedEof);
        }
        let b = input[0];
        *input = &input[1..];
        result |= u64::from(b & 0x7F) << shift;
        if (b & 0x80) == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

pub fn encode_fields(fields: &[(u32, ProtoWireValue)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (field_num, val) in fields {
        match val {
            ProtoWireValue::Varint(v) => {
                write_varint((u64::from(*field_num) << 3) | 0, &mut out);
                write_varint(*v, &mut out);
            }
            ProtoWireValue::Fixed64(v) => {
                write_varint((u64::from(*field_num) << 3) | 1, &mut out);
                out.extend_from_slice(&v.to_le_bytes());
            }
            ProtoWireValue::Bytes(b) => {
                write_varint((u64::from(*field_num) << 3) | 2, &mut out);
                write_varint(b.len() as u64, &mut out);
                out.extend_from_slice(b);
            }
            ProtoWireValue::Fixed32(v) => {
                write_varint((u64::from(*field_num) << 3) | 5, &mut out);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

pub fn decode_fields(mut input: &[u8]) -> Result<Vec<(u32, ProtoWireValue)>, ProtoError> {
    if input.len() > MAX_FRAME_SIZE {
        return Err(ProtoError::FrameTooLarge(input.len()));
    }
    let mut out = Vec::new();
    while !input.is_empty() {
        let tag = read_varint(&mut input)?;
        let field_num = u32::try_from(tag >> 3).map_err(|_| ProtoError::UnexpectedEof)?;
        let wire_type = (tag & 0x07) as u8;
        if field_num == 0 {
            return Err(ProtoError::UnsupportedOpcode(wire_type));
        }
        let val = match wire_type {
            0 => ProtoWireValue::Varint(read_varint(&mut input)?),
            1 => {
                if input.len() < 8 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let v = u64::from_le_bytes(input[..8].try_into().unwrap());
                input = &input[8..];
                ProtoWireValue::Fixed64(v)
            }
            2 => {
                let len = usize::try_from(read_varint(&mut input)?)
                    .map_err(|_| ProtoError::UnexpectedEof)?;
                if len > MAX_FRAME_SIZE || input.len() < len {
                    return Err(ProtoError::UnexpectedEof);
                }
                let b = input[..len].to_vec();
                input = &input[len..];
                ProtoWireValue::Bytes(b)
            }
            5 => {
                if input.len() < 4 {
                    return Err(ProtoError::UnexpectedEof);
                }
                let v = u32::from_le_bytes(input[..4].try_into().unwrap());
                input = &input[4..];
                ProtoWireValue::Fixed32(v)
            }
            other => return Err(ProtoError::UnsupportedOpcode(other)),
        };
        out.push((field_num, val));
    }
    Ok(out)
}

pub fn encode_envelope(env: &QuasarGatewayEnvelope) -> Vec<u8> {
    encode_fields(&[
        (1, ProtoWireValue::Bytes(env.type_url.as_bytes().to_vec())),
        (2, ProtoWireValue::Varint(env.correlation_id)),
        (3, ProtoWireValue::Varint(u64::from(env.status_code))),
        (4, ProtoWireValue::Bytes(env.payload.clone())),
    ])
}

pub fn decode_envelope(bytes: &[u8]) -> Result<QuasarGatewayEnvelope, ProtoError> {
    let fields = decode_fields(bytes)?;
    let mut type_url = String::new();
    let mut correlation_id = 0u64;
    let mut status_code = 0u32;
    let mut payload = Vec::new();
    for (fnum, val) in fields {
        match (fnum, val) {
            (1, ProtoWireValue::Bytes(b)) => {
                type_url = String::from_utf8(b)?;
            }
            (2, ProtoWireValue::Varint(v)) => {
                correlation_id = v;
            }
            (3, ProtoWireValue::Varint(v)) => {
                status_code = v as u32;
            }
            (4, ProtoWireValue::Bytes(b)) => {
                payload = b;
            }
            _ => {}
        }
    }
    Ok(QuasarGatewayEnvelope {
        type_url,
        correlation_id,
        status_code,
        payload,
    })
}
