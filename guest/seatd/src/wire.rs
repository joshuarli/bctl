pub const MAX_PATH_LEN: usize = 256;
pub const MAX_SEAT_LEN: usize = 64;
pub const MAX_SEAT_DEVICES: usize = 128;

pub const CLIENT_OPEN_SEAT: u16 = 1;
pub const CLIENT_CLOSE_SEAT: u16 = 2;
pub const CLIENT_OPEN_DEVICE: u16 = 3;
pub const CLIENT_CLOSE_DEVICE: u16 = 4;
pub const CLIENT_DISABLE_SEAT: u16 = 5;
pub const CLIENT_SWITCH_SESSION: u16 = 6;
pub const CLIENT_PING: u16 = 7;

const SERVER_BIT: u16 = 1 << 15;
pub const SERVER_SEAT_OPENED: u16 = 1 + SERVER_BIT;
pub const SERVER_SEAT_CLOSED: u16 = 2 + SERVER_BIT;
pub const SERVER_DEVICE_OPENED: u16 = 3 + SERVER_BIT;
pub const SERVER_DEVICE_CLOSED: u16 = 4 + SERVER_BIT;
pub const SERVER_DISABLE_SEAT: u16 = 5 + SERVER_BIT;
pub const SERVER_ENABLE_SEAT: u16 = 6 + SERVER_BIT;
pub const SERVER_PONG: u16 = 7 + SERVER_BIT;
pub const SERVER_SESSION_SWITCHED: u16 = 8 + SERVER_BIT;
pub const SERVER_SEAT_DISABLED: u16 = 9 + SERVER_BIT;
pub const SERVER_ERROR: u16 = 0x7fff + SERVER_BIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub opcode: u16,
    pub size: u16,
}

impl Header {
    pub fn encode(self) -> [u8; 4] {
        let mut out = [0u8; 4];
        out[0..2].copy_from_slice(&self.opcode.to_ne_bytes());
        out[2..4].copy_from_slice(&self.size.to_ne_bytes());
        out
    }

    pub fn decode(raw: &[u8; 4]) -> Header {
        Header {
            opcode: u16::from_ne_bytes([raw[0], raw[1]]),
            size: u16::from_ne_bytes([raw[2], raw[3]]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_opcode_values_match_c_header() {
        assert_eq!(CLIENT_OPEN_SEAT, 1);
        assert_eq!(CLIENT_PING, 7);
        assert_eq!(SERVER_SEAT_OPENED, 0x8001);
        assert_eq!(SERVER_PONG, 0x8007);
        assert_eq!(SERVER_SESSION_SWITCHED, 0x8008);
        assert_eq!(SERVER_SEAT_DISABLED, 0x8009);
        assert_eq!(SERVER_ERROR, 0xffff);
    }

    #[test]
    fn wire_header_round_trip_native_endian() {
        let h = Header {
            opcode: SERVER_DEVICE_OPENED,
            size: 4,
        };
        let raw = h.encode();
        assert_eq!(raw, [3u8, 0x80u8, 4u8, 0u8]);
        assert_eq!(Header::decode(&raw), h);
    }

    #[test]
    fn wire_header_explicit_bytes_not_struct_cast() {
        let raw = [7u8, 0u8, 0u8, 0u8];
        let h = Header::decode(&raw);
        assert_eq!(h.opcode, CLIENT_PING);
        assert_eq!(h.size, 0);
    }
}
