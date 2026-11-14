use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

/// The guest's name for the host, as on SmolVM's default link: it reaches host loopback, and only when an
/// allowed private range covers it.
pub const HOST_FROM_GUEST: Ipv4Addr = Ipv4Addr::new(100, 96, 0, 1);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    network: u32,
    prefix: u8,
}

impl Range {
    pub fn parse(cidr: &str) -> Result<Range, String> {
        let (address, prefix) = cidr.split_once('/').ok_or_else(|| format!("{cidr:?} is not IPv4 CIDR"))?;
        let address: Ipv4Addr = address.parse().map_err(|_| format!("{cidr:?} is not IPv4 CIDR"))?;
        let prefix: u8 = prefix.parse().ok().filter(|p| *p <= 32).ok_or_else(|| format!("{cidr:?} has no valid prefix length"))?;
        let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
        Ok(Range { network: u32::from(address) & mask, prefix })
    }

    fn contains(&self, address: Ipv4Addr) -> bool {
        let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
        u32::from(address) & mask == self.network
    }
}

/// Public addresses are reachable; every other address only inside an allowed range.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    allowed: Vec<Range>,
}

fn private_v4(address: Ipv4Addr) -> bool {
    let [a, b, ..] = address.octets();
    address.is_unspecified()
        || address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_multicast()
        || address.is_documentation()
        || a == 0
        || (a == 100 && (b & 0xc0) == 64)
        || (a == 192 && b == 0)
        || (a == 198 && (b & 0xfe) == 18)
        || a >= 240
}

fn private_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return private_v4(v4);
    }
    let first = address.segments()[0];
    address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || first == 0x2001 && address.segments()[1] == 0x0db8
        || first == 0x64 && address.segments()[1] == 0xff9b
}

impl Policy {
    pub fn new(allowed: &[String]) -> Result<Policy, String> {
        Ok(Policy { allowed: allowed.iter().map(|r| Range::parse(r)).collect::<Result<_, _>>()? })
    }

    /// Where a connection to `address` goes, or why it is refused.
    pub fn route(&self, address: SocketAddr) -> Result<SocketAddr, String> {
        let ip = match address.ip() {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
            v4 => v4,
        };
        let allowed = |v4: Ipv4Addr| self.allowed.iter().any(|range| range.contains(v4));
        match ip {
            IpAddr::V4(v4) if v4 == HOST_FROM_GUEST => {
                if allowed(v4) {
                    Ok(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), address.port()))
                } else {
                    Err(format!("{v4} is the host and no allowed range covers it"))
                }
            }
            IpAddr::V4(v4) if private_v4(v4) && !allowed(v4) => Err(format!("{v4} is not a public address")),
            IpAddr::V6(v6) if private_v6(v6) => Err(format!("{v6} is not a public address")),
            ip => Ok(SocketAddr::new(ip, address.port())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    Address(SocketAddr),
    Name(String, u16),
}

const SOCKS_VERSION: u8 = 5;
const NO_AUTHENTICATION: u8 = 0;
const CONNECT: u8 = 1;
const ATYP_IPV4: u8 = 1;
const ATYP_NAME: u8 = 3;
const ATYP_IPV6: u8 = 4;
const REPLY_OK: u8 = 0;
const REPLY_NOT_ALLOWED: u8 = 2;
const REPLY_HOST_UNREACHABLE: u8 = 4;
const REPLY_COMMAND_UNSUPPORTED: u8 = 7;
const REPLY_ADDRESS_UNSUPPORTED: u8 = 8;

fn read_exact(stream: &mut impl Read, n: usize, what: &str) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, format!("the client closed after {filled} of the {n} bytes of {what}"))),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(io::Error::new(error.kind(), format!("reading {what}: {error}"))),
        }
    }
    Ok(buf)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

/// Reads a SOCKS5 greeting and CONNECT request, answering the greeting. An unsupported command or address type
/// is answered and returned as an error.
pub fn read_request(stream: &mut (impl Read + Write)) -> io::Result<Destination> {
    let greeting = read_exact(stream, 2, "the greeting")?;
    if greeting[0] != SOCKS_VERSION {
        return Err(invalid("not SOCKS5"));
    }
    let methods = read_exact(stream, usize::from(greeting[1]), "the greeting's methods")?;
    if !methods.contains(&NO_AUTHENTICATION) {
        stream.write_all(&[SOCKS_VERSION, 0xff])?;
        return Err(invalid("the client offers no unauthenticated method"));
    }
    stream.write_all(&[SOCKS_VERSION, NO_AUTHENTICATION])?;
    let head = read_exact(stream, 4, "the request header")?;
    if head[0] != SOCKS_VERSION {
        return Err(invalid("not SOCKS5"));
    }
    if head[1] != CONNECT {
        reply(stream, REPLY_COMMAND_UNSUPPORTED)?;
        return Err(invalid("only CONNECT is supported"));
    }
    let destination = match head[3] {
        ATYP_IPV4 => {
            let raw = read_exact(stream, 6, "the IPv4 address")?;
            let ip = Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
            Destination::Address(SocketAddr::new(IpAddr::V4(ip), u16::from_be_bytes([raw[4], raw[5]])))
        }
        ATYP_IPV6 => {
            let raw = read_exact(stream, 18, "the IPv6 address")?;
            let octets: [u8; 16] = raw[..16].try_into().expect("16 bytes");
            Destination::Address(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(octets)), u16::from_be_bytes([raw[16], raw[17]])))
        }
        ATYP_NAME => {
            let length = read_exact(stream, 1, "the host name length")?[0];
            let name = read_exact(stream, usize::from(length), "the host name")?;
            let port = read_exact(stream, 2, "the port")?;
            let name = String::from_utf8(name).map_err(|_| invalid("the host name is not UTF-8"))?;
            Destination::Name(name, u16::from_be_bytes([port[0], port[1]]))
        }
        _ => {
            reply(stream, REPLY_ADDRESS_UNSUPPORTED)?;
            return Err(invalid("unknown address type"));
        }
    };
    Ok(destination)
}

fn reply(stream: &mut impl Write, code: u8) -> io::Result<()> {
    stream.write_all(&[SOCKS_VERSION, code, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
}

/// Resolves the destination on the host and picks the first address the policy allows, so a name cannot steer the
/// connection to an address that was not checked.
pub fn resolve(policy: &Policy, destination: &Destination) -> Result<SocketAddr, String> {
    let candidates: Vec<SocketAddr> = match destination {
        Destination::Address(address) => vec![*address],
        Destination::Name(name, port) => {
            (name.as_str(), *port).to_socket_addrs().map_err(|e| format!("cannot resolve {name}: {e}"))?.collect()
        }
    };
    let mut refusal = String::from("no address");
    for candidate in candidates {
        match policy.route(candidate) {
            Ok(route) => return Ok(route),
            Err(reason) => refusal = reason,
        }
    }
    Err(refusal)
}

/// Serves one guest connection: SOCKS5 CONNECT, the policy, then a byte pipe both ways.
pub fn serve(mut client: UnixStream, policy: &Policy) {
    let destination = match read_request(&mut client) {
        Ok(destination) => destination,
        Err(error) => {
            eprintln!("bctl-nanos-vm: egress request: {error}");
            return;
        }
    };
    let target = match resolve(policy, &destination) {
        Ok(target) => target,
        Err(reason) => {
            eprintln!("bctl-nanos-vm: egress to {destination:?} refused: {reason}");
            let _ = reply(&mut client, REPLY_NOT_ALLOWED);
            return;
        }
    };
    let upstream = match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(upstream) => {
            let _ = upstream.set_nodelay(true);
            upstream
        }
        Err(error) => {
            eprintln!("bctl-nanos-vm: egress to {target}: {error}");
            let _ = reply(&mut client, REPLY_HOST_UNREACHABLE);
            return;
        }
    };
    if reply(&mut client, REPLY_OK).is_err() {
        return;
    }
    let (Ok(mut client_reader), Ok(mut upstream_writer)) = (client.try_clone(), upstream.try_clone()) else { return };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_reader, &mut upstream_writer);
        let _ = upstream_writer.shutdown(Shutdown::Write);
    });
    let (mut upstream_reader, mut client_writer) = (upstream, client);
    let _ = io::copy(&mut upstream_reader, &mut client_writer);
    let _ = client_writer.shutdown(Shutdown::Write);
    let _ = forward.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_truncated_greeting_names_how_far_the_client_got() {
        let mut stream = std::io::Cursor::new(vec![SOCKS_VERSION]);
        let error = read_request(&mut stream).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(error.to_string(), "the client closed after 1 of the 2 bytes of the greeting");
    }

    #[test]
    fn public_addresses_pass_and_private_ones_need_an_allowed_range() {
        let blocked = Policy::new(&[]).unwrap();
        assert_eq!(blocked.route(addr("93.184.216.34:443")), Ok(addr("93.184.216.34:443")));
        for private in ["127.0.0.1:80", "10.1.2.3:80", "192.168.1.1:80", "172.16.0.1:80", "169.254.169.254:80", "100.64.0.1:80", "0.0.0.0:80"] {
            assert!(blocked.route(addr(private)).is_err(), "{private} reached");
        }
        for private in ["[::1]:80", "[fd00::1]:80", "[fe80::1]:80", "[::ffff:10.0.0.1]:80"] {
            assert!(blocked.route(addr(private)).is_err(), "{private} reached");
        }
        let allowed = Policy::new(&["10.1.0.0/16".to_string()]).unwrap();
        assert_eq!(allowed.route(addr("10.1.2.3:80")), Ok(addr("10.1.2.3:80")));
        assert!(allowed.route(addr("10.2.0.1:80")).is_err());
    }

    #[test]
    fn the_guests_host_address_reaches_host_loopback_only_when_allowed() {
        assert!(Policy::new(&[]).unwrap().route(addr("100.96.0.1:8080")).is_err());
        let allowed = Policy::new(&["100.96.0.0/30".to_string()]).unwrap();
        assert_eq!(allowed.route(addr("100.96.0.1:8080")), Ok(addr("127.0.0.1:8080")));
        assert_eq!(allowed.route(addr("[::ffff:100.96.0.1]:8080")), Ok(addr("127.0.0.1:8080")));
        assert!(allowed.route(addr("127.0.0.1:8080")).is_err());
    }

    #[test]
    fn malformed_ranges_are_rejected() {
        for bad in ["10.0.0.0", "10.0.0.0/33", "host/8", "::1/128"] {
            assert!(Range::parse(bad).is_err(), "{bad} parsed");
        }
    }

    #[test]
    fn a_socks5_connect_request_is_read_by_name_or_address() {
        let mut wire = std::io::Cursor::new(vec![5, 1, 0, 5, 1, 0, 3, 11, b'e', b'x', b'a', b'm', b'p', b'l', b'e', b'.', b'o', b'r', b'g', 1, 187]);
        struct Duplex<'a>(&'a mut std::io::Cursor<Vec<u8>>, Vec<u8>);
        impl Read for Duplex<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                self.0.read(buf)
            }
        }
        impl Write for Duplex<'_> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.1.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut duplex = Duplex(&mut wire, Vec::new());
        assert_eq!(read_request(&mut duplex).unwrap(), Destination::Name("example.org".into(), 443));
        assert_eq!(duplex.1, vec![5, 0]);
        let mut wire = std::io::Cursor::new(vec![5, 1, 0, 5, 2, 0, 1, 1, 2, 3, 4, 0, 80]);
        let mut duplex = Duplex(&mut wire, Vec::new());
        assert!(read_request(&mut duplex).is_err());
        assert_eq!(duplex.1, vec![5, 0, 5, REPLY_COMMAND_UNSUPPORTED, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0]);
    }
}
