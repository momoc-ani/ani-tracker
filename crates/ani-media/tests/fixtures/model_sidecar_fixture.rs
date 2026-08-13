use std::io::{self, Read, Write};

use serde_json::json;

const MAGIC: [u8; 8] = *b"ANIFRM1\0";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 48;

fn main() -> Result<(), String> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let message = read_message(&mut input)?;
        match message.kind {
            1 => write_message(
                &mut output,
                2,
                message.request_id,
                0,
                0,
                0,
                0,
                serde_json::to_vec(&json!({
                    "ready": true,
                    "protocolVersion": VERSION,
                    "backend": "ncnn-vulkan",
                    "gpuDevice": "fixture-vulkan-device",
                    "modelId": "rife-v4.6"
                }))
                .map_err(|error| error.to_string())?,
            )?,
            3 | 4 => {
                let frame_bytes = frame_bytes(message.width, message.height, message.stride)?;
                if message.payload.len() != frame_bytes.saturating_mul(2) {
                    write_message(
                        &mut output,
                        6,
                        message.request_id,
                        0,
                        0,
                        0,
                        0,
                        b"invalid frame pair".to_vec(),
                    )?;
                    continue;
                }
                let (previous, next) = message.payload.split_at(frame_bytes);
                let frame = previous
                    .iter()
                    .zip(next)
                    .map(|(left, right)| ((*left as u16 + *right as u16) / 2) as u8)
                    .collect();
                write_message(
                    &mut output,
                    5,
                    message.request_id,
                    message.width,
                    message.height,
                    message.stride,
                    message.pts_micros,
                    frame,
                )?;
            }
            8 => {
                let frame_bytes = frame_bytes(message.width, message.height, message.stride)?;
                if message.payload.len() != frame_bytes {
                    write_message(
                        &mut output,
                        6,
                        message.request_id,
                        0,
                        0,
                        0,
                        0,
                        b"invalid frame".to_vec(),
                    )?;
                    continue;
                }
                write_message(
                    &mut output,
                    5,
                    message.request_id,
                    message.width,
                    message.height,
                    message.stride,
                    message.pts_micros,
                    message.payload,
                )?;
            }
            7 => break,
            _ => {
                write_message(
                    &mut output,
                    6,
                    message.request_id,
                    0,
                    0,
                    0,
                    0,
                    b"unknown request".to_vec(),
                )?;
            }
        }
    }
    Ok(())
}

struct Message {
    kind: u16,
    request_id: u64,
    width: u32,
    height: u32,
    stride: u32,
    pts_micros: i64,
    payload: Vec<u8>,
}

fn read_message(reader: &mut impl Read) -> Result<Message, String> {
    let mut header = [0_u8; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .map_err(|error| error.to_string())?;
    if header[0..8] != MAGIC || read_u16(&header[8..10])? != VERSION {
        return Err("invalid fixture protocol".to_owned());
    }
    let payload_len = read_u32(&header[44..48])? as usize;
    let mut payload = vec![0; payload_len];
    reader
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    Ok(Message {
        kind: read_u16(&header[10..12])?,
        request_id: read_u64(&header[16..24])?,
        width: read_u32(&header[24..28])?,
        height: read_u32(&header[28..32])?,
        stride: read_u32(&header[32..36])?,
        pts_micros: read_i64(&header[36..44])?,
        payload,
    })
}

#[allow(clippy::too_many_arguments)]
fn write_message(
    writer: &mut impl Write,
    kind: u16,
    request_id: u64,
    width: u32,
    height: u32,
    stride: u32,
    pts_micros: i64,
    payload: Vec<u8>,
) -> Result<(), String> {
    let mut header = [0_u8; HEADER_BYTES];
    header[0..8].copy_from_slice(&MAGIC);
    header[8..10].copy_from_slice(&VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&kind.to_le_bytes());
    header[16..24].copy_from_slice(&request_id.to_le_bytes());
    header[24..28].copy_from_slice(&width.to_le_bytes());
    header[28..32].copy_from_slice(&height.to_le_bytes());
    header[32..36].copy_from_slice(&stride.to_le_bytes());
    header[36..44].copy_from_slice(&pts_micros.to_le_bytes());
    header[44..48].copy_from_slice(
        &u32::try_from(payload.len())
            .map_err(|_| "fixture payload too large".to_owned())?
            .to_le_bytes(),
    );
    writer
        .write_all(&header)
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&payload)
        .map_err(|error| error.to_string())?;
    writer.flush().map_err(|error| error.to_string())
}

fn frame_bytes(width: u32, height: u32, stride: u32) -> Result<usize, String> {
    if width == 0 || height == 0 || stride != width.saturating_mul(3) {
        return Err("invalid frame dimensions".to_owned());
    }
    usize::try_from(stride)
        .ok()
        .and_then(|stride| {
            usize::try_from(height)
                .ok()
                .and_then(|height| stride.checked_mul(height))
        })
        .ok_or_else(|| "frame size overflow".to_owned())
}

fn read_u16(value: &[u8]) -> Result<u16, String> {
    Ok(u16::from_le_bytes(value.try_into().map_err(|_| "u16")?))
}

fn read_u32(value: &[u8]) -> Result<u32, String> {
    Ok(u32::from_le_bytes(value.try_into().map_err(|_| "u32")?))
}

fn read_u64(value: &[u8]) -> Result<u64, String> {
    Ok(u64::from_le_bytes(value.try_into().map_err(|_| "u64")?))
}

fn read_i64(value: &[u8]) -> Result<i64, String> {
    Ok(i64::from_le_bytes(value.try_into().map_err(|_| "i64")?))
}
