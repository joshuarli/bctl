use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::host::Fallible;
use crate::net::Stream;

const BYTES_PER_PIXEL: usize = 4;
const ENCODING_RAW: i32 = 0;
const ENCODING_DESKTOP_SIZE: i32 = -223;

pub struct Viewer {
    stream: Stream,
    width: u16,
    height: u16,
}

pub struct Probe {
    pub x: u16,
    pub y: u16,
}

fn read<const N: usize>(stream: &mut Stream) -> Fallible<[u8; N]> {
    let mut bytes = [0u8; N];
    stream.read_exact(&mut bytes).map_err(|e| format!("RFB read: {e}"))?;
    Ok(bytes)
}

fn skip(stream: &mut Stream, bytes: usize) -> Fallible<()> {
    let mut sink = vec![0u8; bytes];
    stream.read_exact(&mut sink).map_err(|e| format!("RFB read: {e}"))
}

impl Viewer {
    pub fn handshake(mut stream: Stream) -> Fallible<Viewer> {
        let io = |e: std::io::Error| format!("RFB handshake: {e}");
        read::<12>(&mut stream)?;
        stream.write_all(b"RFB 003.008\n").map_err(io)?;
        let [count] = read::<1>(&mut stream)?;
        skip(&mut stream, usize::from(count))?;
        stream.write_all(&[1]).map_err(io)?;
        if u32::from_be_bytes(read(&mut stream)?) != 0 {
            return Err("RFB security handshake failed".into());
        }
        stream.write_all(&[1]).map_err(io)?;
        let width = u16::from_be_bytes(read(&mut stream)?);
        let height = u16::from_be_bytes(read(&mut stream)?);
        let format = read::<16>(&mut stream)?;
        if format[0] != 32 {
            return Err(format!("server pixel format is {} bits per pixel, expected 32", format[0]));
        }
        let name_length = u32::from_be_bytes(read(&mut stream)?) as usize;
        skip(&mut stream, name_length)?;
        let mut set_encodings = vec![2, 0, 0, 2];
        set_encodings.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        set_encodings.extend_from_slice(&ENCODING_DESKTOP_SIZE.to_be_bytes());
        stream.write_all(&set_encodings).map_err(io)?;
        Ok(Viewer { stream, width, height })
    }

    pub fn request(&mut self, incremental: bool) -> Fallible<()> {
        let mut message = vec![3, u8::from(incremental), 0, 0, 0, 0];
        message.extend_from_slice(&self.width.to_be_bytes());
        message.extend_from_slice(&self.height.to_be_bytes());
        self.stream.write_all(&message).map_err(|e| format!("RFB request: {e}"))
    }

    pub fn press_pointer(&mut self, x: u16, y: u16) -> Fallible<()> {
        let mut message = Vec::new();
        for buttons in [0u8, 1] {
            message.extend_from_slice(&[5, buttons]);
            message.extend_from_slice(&x.to_be_bytes());
            message.extend_from_slice(&y.to_be_bytes());
        }
        self.stream.write_all(&message).map_err(|e| format!("RFB pointer: {e}"))
    }

    pub fn release_pointer(&mut self, x: u16, y: u16) -> Fallible<()> {
        let mut message = vec![5, 0];
        message.extend_from_slice(&x.to_be_bytes());
        message.extend_from_slice(&y.to_be_bytes());
        self.stream.write_all(&message).map_err(|e| format!("RFB pointer: {e}"))
    }

    pub fn tap_key(&mut self, keysym: u32) -> Fallible<()> {
        let mut message = Vec::new();
        for down in [1u8, 0] {
            message.extend_from_slice(&[4, down, 0, 0]);
            message.extend_from_slice(&keysym.to_be_bytes());
        }
        self.stream.write_all(&message).map_err(|e| format!("RFB key: {e}"))
    }

    pub fn next_update(&mut self, probe: &Probe) -> Fallible<Option<[u8; BYTES_PER_PIXEL]>> {
        let [kind] = read::<1>(&mut self.stream)?;
        match kind {
            0 => {
                let [_, rects_hi, rects_lo] = read::<3>(&mut self.stream)?;
                let mut seen = None;
                for _ in 0..u16::from_be_bytes([rects_hi, rects_lo]) {
                    let header = read::<8>(&mut self.stream)?;
                    let (x, y) = (u16::from_be_bytes([header[0], header[1]]), u16::from_be_bytes([header[2], header[3]]));
                    let (w, h) = (u16::from_be_bytes([header[4], header[5]]), u16::from_be_bytes([header[6], header[7]]));
                    match i32::from_be_bytes(read(&mut self.stream)?) {
                        ENCODING_RAW => {
                            let mut data = vec![0u8; usize::from(w) * usize::from(h) * BYTES_PER_PIXEL];
                            self.stream.read_exact(&mut data).map_err(|e| format!("RFB rect: {e}"))?;
                            if (x..x + w).contains(&probe.x) && (y..y + h).contains(&probe.y) {
                                let offset = ((usize::from(probe.y - y)) * usize::from(w) + usize::from(probe.x - x)) * BYTES_PER_PIXEL;
                                seen = Some([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]);
                            }
                        }
                        ENCODING_DESKTOP_SIZE => (self.width, self.height) = (w, h),
                        other => return Err(format!("server sent encoding {other}")),
                    }
                }
                Ok(seen)
            }
            1 => {
                let header = read::<5>(&mut self.stream)?;
                skip(&mut self.stream, usize::from(u16::from_be_bytes([header[3], header[4]])) * 6)?;
                Ok(None)
            }
            2 => Ok(None),
            3 => {
                skip(&mut self.stream, 3)?;
                let length = u32::from_be_bytes(read(&mut self.stream)?) as usize;
                skip(&mut self.stream, length)?;
                Ok(None)
            }
            other => Err(format!("unknown RFB server message {other}")),
        }
    }

    pub fn settle(&mut self, probe: &Probe) -> Fallible<[u8; BYTES_PER_PIXEL]> {
        self.request(false)?;
        loop {
            if let Some(pixel) = self.next_update(probe)? {
                return Ok(pixel);
            }
        }
    }

    pub fn time_to_change(
        &mut self,
        probe: &Probe,
        known: [u8; BYTES_PER_PIXEL],
        timeout: Duration,
        input: impl FnOnce(&mut Viewer) -> Fallible<()>,
    ) -> Fallible<(Duration, [u8; BYTES_PER_PIXEL])> {
        self.request(true)?;
        let started = Instant::now();
        input(self)?;
        loop {
            if started.elapsed() > timeout {
                return Err(format!("probe pixel did not change within {timeout:?}"));
            }
            match self.next_update(probe)? {
                Some(pixel) if pixel != known => return Ok((started.elapsed(), pixel)),
                _ => self.request(true)?,
            }
        }
    }
}
