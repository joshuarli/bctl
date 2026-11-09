use std::fmt;

/// Client-to-server RFB messages, parsed only far enough to know where each one ends.
const SET_PIXEL_FORMAT: u8 = 0;
const SET_ENCODINGS: u8 = 2;
const FRAMEBUFFER_UPDATE_REQUEST: u8 = 3;
const KEY_EVENT: u8 = 4;
const POINTER_EVENT: u8 = 5;
const CLIENT_CUT_TEXT: u8 = 6;
const ENABLE_CONTINUOUS_UPDATES: u8 = 150;

/// The client's handshake bytes before normal messages: version (12), security choice (1), ClientInit (1).
const HANDSHAKE_BYTES: usize = 14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMessage(pub u8);

impl fmt::Display for UnknownMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "client sent unsupported RFB message type {}", self.0)
    }
}

impl std::error::Error for UnknownMessage {}

/// Whether a viewer's keyboard and pointer reach the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPolicy {
    Forward,
    Drop,
}

/// Relays a viewer's byte stream to the RFB server, removing key and pointer events when input is not allowed.
/// Bytes may arrive split anywhere; incomplete messages wait for the rest.
#[derive(Debug, Default)]
pub struct ClientFilter {
    handshake_left: Option<usize>,
    pending: Vec<u8>,
}

impl ClientFilter {
    pub fn new() -> ClientFilter {
        ClientFilter { handshake_left: Some(HANDSHAKE_BYTES), pending: Vec::new() }
    }

    /// Appends what may be forwarded to `out` and returns how many input events were dropped.
    pub fn push(&mut self, bytes: &[u8], policy: InputPolicy, out: &mut Vec<u8>) -> Result<usize, UnknownMessage> {
        let mut bytes = bytes;
        if let Some(left) = self.handshake_left {
            let take = left.min(bytes.len());
            out.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            self.handshake_left = (left > take).then_some(left - take);
            if self.handshake_left.is_some() {
                return Ok(0);
            }
        }
        self.pending.extend_from_slice(bytes);
        let mut dropped = 0;
        let mut start = 0;
        while let Some(len) = message_len(&self.pending[start..])? {
            let message = &self.pending[start..start + len];
            let is_input = matches!(message[0], KEY_EVENT | POINTER_EVENT);
            if is_input && policy == InputPolicy::Drop {
                dropped += 1;
            } else {
                out.extend_from_slice(message);
            }
            start += len;
        }
        self.pending.drain(..start);
        Ok(dropped)
    }
}

/// Length of the first complete message in `buf`, or `None` if more bytes are needed.
fn message_len(buf: &[u8]) -> Result<Option<usize>, UnknownMessage> {
    let Some(&kind) = buf.first() else { return Ok(None) };
    let len = match kind {
        SET_PIXEL_FORMAT => 20,
        FRAMEBUFFER_UPDATE_REQUEST | ENABLE_CONTINUOUS_UPDATES => 10,
        KEY_EVENT => 8,
        POINTER_EVENT => 6,
        SET_ENCODINGS => {
            let Some(count) = buf.get(2..4) else { return Ok(None) };
            4 + 4 * usize::from(u16::from_be_bytes([count[0], count[1]]))
        }
        CLIENT_CUT_TEXT => {
            let Some(length) = buf.get(4..8) else { return Ok(None) };
            8 + u32::from_be_bytes([length[0], length[1], length[2], length[3]]) as usize
        }
        other => return Err(UnknownMessage(other)),
    };
    Ok((buf.len() >= len).then_some(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Vec<u8> {
        let mut h = b"RFB 003.008\n".to_vec();
        h.extend_from_slice(&[1, 1]);
        h
    }

    fn stream() -> Vec<u8> {
        let mut s = handshake();
        s.extend_from_slice(&[2, 0, 0, 2, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x21]);
        s.extend_from_slice(&[4, 1, 0, 0, 0, 0, 0, 0x61]);
        s.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 10, 0, 10]);
        s.extend_from_slice(&[5, 1, 0, 7, 0, 9]);
        s.extend_from_slice(&[6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']);
        s
    }

    fn run(policy: InputPolicy, chunk: usize) -> (Vec<u8>, usize) {
        let mut filter = ClientFilter::new();
        let mut out = Vec::new();
        let mut dropped = 0;
        for piece in stream().chunks(chunk) {
            dropped += filter.push(piece, policy, &mut out).unwrap();
        }
        (out, dropped)
    }

    #[test]
    fn forwarding_passes_every_byte_however_it_is_split() {
        for chunk in [1, 3, 7, 64] {
            assert_eq!(run(InputPolicy::Forward, chunk), (stream(), 0), "chunk {chunk}");
        }
    }

    #[test]
    fn dropping_removes_exactly_the_key_and_pointer_events() {
        let mut expected = handshake();
        expected.extend_from_slice(&[2, 0, 0, 2, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x21]);
        expected.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 10, 0, 10]);
        expected.extend_from_slice(&[6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']);
        for chunk in [1, 5, 64] {
            assert_eq!(run(InputPolicy::Drop, chunk), (expected.clone(), 2), "chunk {chunk}");
        }
    }

    #[test]
    fn an_unknown_message_type_is_refused_rather_than_guessed_past() {
        let mut filter = ClientFilter::new();
        let mut bytes = handshake();
        bytes.push(255);
        assert_eq!(filter.push(&bytes, InputPolicy::Forward, &mut Vec::new()), Err(UnknownMessage(255)));
    }
}
