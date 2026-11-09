use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::host::Fallible;
use crate::net::Stream;

const BYTES_PER_PIXEL: usize = 4;
const ENCODING_RAW: i32 = 0;
const ENCODING_DESKTOP_SIZE: i32 = -223;

pub struct Viewer {
    stream: Counted,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

struct Counted {
    inner: Stream,
    read: u64,
}

impl Read for Counted {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

impl Write for Counted {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct Probe {
    pub x: u16,
    pub y: u16,
}

fn read<R: Read, const N: usize>(stream: &mut R) -> Fallible<[u8; N]> {
    let mut bytes = [0u8; N];
    stream.read_exact(&mut bytes).map_err(|e| format!("RFB read: {e}"))?;
    Ok(bytes)
}

fn skip<R: Read>(stream: &mut R, bytes: usize) -> Fallible<()> {
    let mut sink = vec![0u8; bytes];
    stream.read_exact(&mut sink).map_err(|e| format!("RFB read: {e}"))
}

impl Viewer {
    pub fn handshake(stream: Stream) -> Fallible<Viewer> {
        let mut stream = Counted { inner: stream, read: 0 };
        let io = |e: std::io::Error| format!("RFB handshake: {e}");
        read::<_, 12>(&mut stream)?;
        stream.write_all(b"RFB 003.008\n").map_err(io)?;
        let [count] = read::<_, 1>(&mut stream)?;
        skip(&mut stream, usize::from(count))?;
        stream.write_all(&[1]).map_err(io)?;
        if u32::from_be_bytes(read(&mut stream)?) != 0 {
            return Err("RFB security handshake failed".into());
        }
        stream.write_all(&[1]).map_err(io)?;
        let width = u16::from_be_bytes(read(&mut stream)?);
        let height = u16::from_be_bytes(read(&mut stream)?);
        let format = read::<_, 16>(&mut stream)?;
        if format[0] != 32 {
            return Err(format!("server pixel format is {} bits per pixel, expected 32", format[0]));
        }
        let name_length = u32::from_be_bytes(read(&mut stream)?) as usize;
        skip(&mut stream, name_length)?;
        let mut set_encodings = vec![2, 0, 0, 2];
        set_encodings.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        set_encodings.extend_from_slice(&ENCODING_DESKTOP_SIZE.to_be_bytes());
        stream.write_all(&set_encodings).map_err(io)?;
        let pixels = vec![0u8; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL];
        Ok(Viewer { stream, width, height, pixels })
    }

    pub fn bytes_received(&self) -> u64 {
        self.stream.read
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

    fn paint(&mut self, x: u16, y: u16, width: u16, height: u16, data: &[u8]) -> Fallible<()> {
        if u32::from(x) + u32::from(width) > u32::from(self.width) || u32::from(y) + u32::from(height) > u32::from(self.height) {
            return Err("rectangle outside the framebuffer".into());
        }
        let row = usize::from(width) * BYTES_PER_PIXEL;
        for line in 0..usize::from(height) {
            let source = &data[line * row..(line + 1) * row];
            let offset = ((usize::from(y) + line) * usize::from(self.width) + usize::from(x)) * BYTES_PER_PIXEL;
            self.pixels[offset..offset + row].copy_from_slice(source);
        }
        Ok(())
    }

    pub fn read_update_message(&mut self) -> Fallible<bool> {
        let [kind] = read::<_, 1>(&mut self.stream)?;
        match kind {
            0 => {
                let [_, rects_hi, rects_lo] = read::<_, 3>(&mut self.stream)?;
                for _ in 0..u16::from_be_bytes([rects_hi, rects_lo]) {
                    let header = read::<_, 8>(&mut self.stream)?;
                    let (x, y) = (u16::from_be_bytes([header[0], header[1]]), u16::from_be_bytes([header[2], header[3]]));
                    let (width, height) = (u16::from_be_bytes([header[4], header[5]]), u16::from_be_bytes([header[6], header[7]]));
                    match i32::from_be_bytes(read(&mut self.stream)?) {
                        ENCODING_RAW => {
                            let mut data = vec![0u8; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL];
                            self.stream.read_exact(&mut data).map_err(|e| format!("RFB rect: {e}"))?;
                            self.paint(x, y, width, height, &data)?;
                        }
                        ENCODING_DESKTOP_SIZE => {
                            self.width = width;
                            self.height = height;
                            self.pixels = vec![0u8; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL];
                        }
                        other => return Err(format!("server sent encoding {other}")),
                    }
                }
                Ok(true)
            }
            1 => {
                let header = read::<_, 5>(&mut self.stream)?;
                skip(&mut self.stream, usize::from(u16::from_be_bytes([header[3], header[4]])) * 6)?;
                Ok(false)
            }
            2 => Ok(false),
            3 => {
                skip(&mut self.stream, 3)?;
                let length = u32::from_be_bytes(read(&mut self.stream)?) as usize;
                skip(&mut self.stream, length)?;
                Ok(false)
            }
            other => Err(format!("unknown RFB server message {other}")),
        }
    }

    pub fn next_update(&mut self, probe: &Probe) -> Fallible<Option<[u8; BYTES_PER_PIXEL]>> {
        if !self.read_update_message()? {
            return Ok(None);
        }
        if probe.x >= self.width || probe.y >= self.height {
            return Err(format!("probe {}x{} is outside the {}x{} framebuffer", probe.x, probe.y, self.width, self.height));
        }
        let offset = (usize::from(probe.y) * usize::from(self.width) + usize::from(probe.x)) * BYTES_PER_PIXEL;
        Ok(Some([self.pixels[offset], self.pixels[offset + 1], self.pixels[offset + 2], self.pixels[offset + 3]]))
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

    pub fn find(&self, colour: [u8; 3]) -> Option<(u16, u16)> {
        (0..self.height).flat_map(|y| (0..self.width).map(move |x| (x, y))).find(|&(x, y)| {
            let offset = (usize::from(y) * usize::from(self.width) + usize::from(x)) * BYTES_PER_PIXEL;
            self.pixels[offset] == colour[0] && self.pixels[offset + 1] == colour[1] && self.pixels[offset + 2] == colour[2]
        })
    }

    pub fn decode_strip(&self, origin: (u16, u16), scale: f64) -> Option<u16> {
        let cell = (16.0 * scale).round() as u16;
        if cell == 0 {
            return None;
        }
        let mut value = 0u16;
        for bit in 0..16u16 {
            let x = origin.0.checked_add(cell * (bit + 1))?.checked_add(cell / 2)?;
            let y = origin.1.checked_add(cell / 2)?;
            if x >= self.width || y >= self.height {
                return None;
            }
            let offset = (usize::from(y) * usize::from(self.width) + usize::from(x)) * BYTES_PER_PIXEL;
            if self.pixels[offset] > 127 && self.pixels[offset + 1] > 127 && self.pixels[offset + 2] > 127 {
                value |= 1 << bit;
            }
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(width: u16, height: u16, pixels: Vec<u8>) -> Viewer {
        Viewer { stream: Counted { inner: Box::new(std::io::Cursor::new(Vec::new())), read: 0 }, width, height, pixels }
    }

    #[test]
    fn decode_reads_the_marker_strip() {
        let (width, height) = (17 * 16, 16);
        let mut pixels = vec![0u8; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL];
        for y in 0..16usize {
            for x in 0..16usize {
                let offset = (y * usize::from(width) + x) * BYTES_PER_PIXEL;
                pixels[offset] = 255;
                pixels[offset + 2] = 255;
            }
        }
        let value: u16 = 0b0000_0000_1010_0101;
        for bit in 0..16u16 {
            if value >> bit & 1 == 1 {
                for y in 0..16usize {
                    for x in 0..16usize {
                        let offset = (y * usize::from(width) + 16 * usize::from(bit + 1) + x) * BYTES_PER_PIXEL;
                        pixels[offset] = 255;
                        pixels[offset + 1] = 255;
                        pixels[offset + 2] = 255;
                    }
                }
            }
        }
        let viewer = viewer(width, height, pixels);
        assert_eq!(viewer.find([255, 0, 255]), Some((0, 0)));
        assert_eq!(viewer.decode_strip((0, 0), 1.0), Some(value));
    }
}
