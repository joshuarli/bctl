mod filter;

pub use filter::{ClientFilter, InputPolicy, UnknownMessage};

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const PROTOCOL_VERSION: &[u8; 12] = b"RFB 003.008\n";
const SECURITY_NONE: u8 = 1;
const ENCODING_RAW: i32 = 0;
const ENCODING_DESKTOP_SIZE: i32 = -223;
const BYTES_PER_PIXEL: usize = 4;

pub const KEY_RETURN: u32 = 0xff0d;
pub const KEY_DOWN: u32 = 0xff54;
pub const KEY_SHIFT: u32 = 0xffe1;
pub const KEY_ESCAPE: u32 = 0xff1b;

/// Pointer button mask bits as RFB defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buttons(u8);

impl Buttons {
    pub const NONE: Buttons = Buttons(0);
    pub const LEFT: Buttons = Buttons(1);
    pub const RIGHT: Buttons = Buttons(1 << 2);
    pub const WHEEL_UP: Buttons = Buttons(1 << 3);
    pub const WHEEL_DOWN: Buttons = Buttons(1 << 4);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A copy of the remote framebuffer in the server's default pixel format:
/// 32 bits per pixel, little-endian, red at bit 16, green at 8, blue at 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    width: u16,
    height: u16,
    pixels: Vec<u8>,
}

impl Frame {
    fn blank(width: u16, height: u16) -> Frame {
        Frame { width, height, pixels: vec![0; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL] }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn pixel(&self, x: u16, y: u16) -> Rgb {
        let offset = (usize::from(y) * usize::from(self.width) + usize::from(x)) * BYTES_PER_PIXEL;
        let p = &self.pixels[offset..offset + BYTES_PER_PIXEL];
        Rgb(p[2], p[1], p[0])
    }

    /// The first pixel, scanning rows top to bottom, whose colour is `colour`.
    pub fn find(&self, colour: Rgb) -> Option<(u16, u16)> {
        (0..self.height).flat_map(|y| (0..self.width).map(move |x| (x, y))).find(|&(x, y)| self.pixel(x, y) == colour)
    }

    /// Number of pixels whose colour differs from `other`'s; frames of different sizes differ everywhere.
    pub fn changed_pixels(&self, other: &Frame) -> usize {
        if self.width != other.width || self.height != other.height {
            return usize::from(self.width) * usize::from(self.height);
        }
        self.pixels
            .as_chunks::<BYTES_PER_PIXEL>()
            .0
            .iter()
            .zip(other.pixels.as_chunks::<BYTES_PER_PIXEL>().0)
            .filter(|(a, b)| a[..3] != b[..3])
            .count()
    }

    fn paint(&mut self, x: u16, y: u16, width: u16, data: &[u8]) {
        let row_bytes = usize::from(width) * BYTES_PER_PIXEL;
        for (row, line) in data.chunks_exact(row_bytes).enumerate() {
            let offset = ((usize::from(y) + row) * usize::from(self.width) + usize::from(x)) * BYTES_PER_PIXEL;
            self.pixels[offset..offset + row_bytes].copy_from_slice(line);
        }
    }
}

pub struct Client {
    stream: TcpStream,
    frame: Frame,
    name: String,
}

fn protocol_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl Client {
    pub fn connect(addr: impl ToSocketAddrs, timeout: Duration) -> io::Result<Client> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_nodelay(true)?;
        let mut client = Client { stream, frame: Frame::blank(0, 0), name: String::new() };
        client.handshake()?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    fn handshake(&mut self) -> io::Result<()> {
        let mut version = [0; 12];
        self.stream.read_exact(&mut version)?;
        if &version[..4] != b"RFB " {
            return Err(protocol_error("server did not send an RFB version"));
        }
        self.stream.write_all(PROTOCOL_VERSION)?;

        let count = self.read_u8()?;
        if count == 0 {
            let reason = self.read_string()?;
            return Err(protocol_error(format!("server refused the connection: {reason}")));
        }
        let mut types = vec![0; usize::from(count)];
        self.stream.read_exact(&mut types)?;
        if !types.contains(&SECURITY_NONE) {
            return Err(protocol_error(format!("server offers no unauthenticated security type: {types:?}")));
        }
        self.stream.write_all(&[SECURITY_NONE])?;
        if self.read_u32()? != 0 {
            let reason = self.read_string()?;
            return Err(protocol_error(format!("security handshake failed: {reason}")));
        }

        self.stream.write_all(&[1])?;
        let width = self.read_u16()?;
        let height = self.read_u16()?;
        let mut pixel_format = [0; 16];
        self.stream.read_exact(&mut pixel_format)?;
        self.name = self.read_string()?;
        self.frame = Frame::blank(width, height);

        let mut set_encodings = vec![2, 0];
        set_encodings.extend_from_slice(&2u16.to_be_bytes());
        set_encodings.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        set_encodings.extend_from_slice(&ENCODING_DESKTOP_SIZE.to_be_bytes());
        self.stream.write_all(&set_encodings)
    }

    /// Requests the whole framebuffer and applies the next update the server sends.
    pub fn refresh(&mut self) -> io::Result<&Frame> {
        let mut request = vec![3, 0, 0, 0, 0, 0];
        request.extend_from_slice(&self.frame.width.to_be_bytes());
        request.extend_from_slice(&self.frame.height.to_be_bytes());
        self.stream.write_all(&request)?;
        loop {
            match self.read_u8()? {
                0 => {
                    self.read_framebuffer_update()?;
                    return Ok(&self.frame);
                }
                1 => self.skip_colour_map_entries()?,
                2 => {}
                3 => self.skip_cut_text()?,
                other => return Err(protocol_error(format!("unknown server message type {other}"))),
            }
        }
    }

    pub fn key(&mut self, keysym: u32, down: bool) -> io::Result<()> {
        let mut message = vec![4, u8::from(down), 0, 0];
        message.extend_from_slice(&keysym.to_be_bytes());
        self.stream.write_all(&message)
    }

    pub fn tap(&mut self, keysym: u32) -> io::Result<()> {
        self.key(keysym, true)?;
        self.key(keysym, false)
    }

    /// Types printable ASCII the way a US-layout client does: shifted symbols travel with real Shift events.
    pub fn type_text(&mut self, text: &str) -> io::Result<()> {
        for c in text.chars() {
            if !c.is_ascii_graphic() && c != ' ' {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("cannot type {c:?}")));
            }
            let shifted = c.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(c);
            if shifted {
                self.key(KEY_SHIFT, true)?;
            }
            self.tap(u32::from(c))?;
            if shifted {
                self.key(KEY_SHIFT, false)?;
            }
        }
        Ok(())
    }

    pub fn pointer(&mut self, x: u16, y: u16, buttons: Buttons) -> io::Result<()> {
        let mut message = vec![5, buttons.0];
        message.extend_from_slice(&x.to_be_bytes());
        message.extend_from_slice(&y.to_be_bytes());
        self.stream.write_all(&message)
    }

    pub fn click(&mut self, x: u16, y: u16) -> io::Result<()> {
        self.pointer(x, y, Buttons::NONE)?;
        self.pointer(x, y, Buttons::LEFT)?;
        self.pointer(x, y, Buttons::NONE)
    }

    pub fn scroll(&mut self, x: u16, y: u16, wheel: Buttons, notches: usize) -> io::Result<()> {
        for _ in 0..notches {
            self.pointer(x, y, wheel)?;
            self.pointer(x, y, Buttons::NONE)?;
        }
        Ok(())
    }

    fn read_framebuffer_update(&mut self) -> io::Result<()> {
        self.read_u8()?;
        let rects = self.read_u16()?;
        for _ in 0..rects {
            let x = self.read_u16()?;
            let y = self.read_u16()?;
            let width = self.read_u16()?;
            let height = self.read_u16()?;
            match self.read_i32()? {
                ENCODING_RAW => {
                    if u32::from(x) + u32::from(width) > u32::from(self.frame.width)
                        || u32::from(y) + u32::from(height) > u32::from(self.frame.height)
                    {
                        return Err(protocol_error("rectangle outside the framebuffer"));
                    }
                    let mut data = vec![0; usize::from(width) * usize::from(height) * BYTES_PER_PIXEL];
                    self.stream.read_exact(&mut data)?;
                    self.frame.paint(x, y, width, &data);
                }
                ENCODING_DESKTOP_SIZE => self.frame = Frame::blank(width, height),
                other => return Err(protocol_error(format!("server sent unrequested encoding {other}"))),
            }
        }
        Ok(())
    }

    fn skip_colour_map_entries(&mut self) -> io::Result<()> {
        let mut header = [0; 5];
        self.stream.read_exact(&mut header)?;
        let count = u16::from_be_bytes([header[3], header[4]]);
        self.skip(usize::from(count) * 6)
    }

    fn skip_cut_text(&mut self) -> io::Result<()> {
        self.skip(3)?;
        let length = self.read_u32()?;
        self.skip(length as usize)
    }

    fn skip(&mut self, bytes: usize) -> io::Result<()> {
        io::copy(&mut (&mut self.stream).take(bytes as u64), &mut io::sink()).map(drop)
    }

    fn read_string(&mut self) -> io::Result<String> {
        let length = self.read_u32()?;
        let mut bytes = vec![0; length as usize];
        self.stream.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| protocol_error("string is not UTF-8"))
    }

    fn read_u8(&mut self) -> io::Result<u8> {
        let mut b = [0; 1];
        self.stream.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn read_u16(&mut self) -> io::Result<u16> {
        let mut b = [0; 2];
        self.stream.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn read_u32(&mut self) -> io::Result<u32> {
        let mut b = [0; 4];
        self.stream.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    fn read_i32(&mut self) -> io::Result<i32> {
        let mut b = [0; 4];
        self.stream.read_exact(&mut b)?;
        Ok(i32::from_be_bytes(b))
    }
}

#[cfg(test)]
mod tests;
