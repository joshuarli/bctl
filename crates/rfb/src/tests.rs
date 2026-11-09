use super::*;
use std::net::TcpListener;
use std::thread;

fn bgrx(r: u8, g: u8, b: u8) -> [u8; 4] {
    [b, g, r, 0]
}

fn serve(script: impl FnOnce(TcpStream) + Send + 'static) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        script(stream);
    });
    (addr, handle)
}

fn server_handshake(stream: &mut TcpStream, width: u16, height: u16) {
    stream.write_all(PROTOCOL_VERSION).unwrap();
    let mut version = [0; 12];
    stream.read_exact(&mut version).unwrap();
    assert_eq!(&version, PROTOCOL_VERSION);
    stream.write_all(&[1, SECURITY_NONE]).unwrap();
    let mut choice = [0; 1];
    stream.read_exact(&mut choice).unwrap();
    assert_eq!(choice, [SECURITY_NONE]);
    stream.write_all(&0u32.to_be_bytes()).unwrap();
    let mut shared = [0; 1];
    stream.read_exact(&mut shared).unwrap();
    let mut init = Vec::new();
    init.extend_from_slice(&width.to_be_bytes());
    init.extend_from_slice(&height.to_be_bytes());
    init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend_from_slice(&6u32.to_be_bytes());
    init.extend_from_slice(b"smolvm");
    stream.write_all(&init).unwrap();
    let mut set_encodings = [0; 12];
    stream.read_exact(&mut set_encodings).unwrap();
    assert_eq!(set_encodings[0], 2);
}

#[test]
fn a_raw_update_lands_at_its_rectangle() {
    let (addr, server) = serve(|mut stream| {
        server_handshake(&mut stream, 3, 2);
        let mut request = [0; 10];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, [3, 0, 0, 0, 0, 0, 0, 3, 0, 2]);
        let mut update = vec![0, 0, 0, 1];
        for v in [1u16, 1, 2, 1] {
            update.extend_from_slice(&v.to_be_bytes());
        }
        update.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        update.extend_from_slice(&bgrx(255, 0, 255));
        update.extend_from_slice(&bgrx(10, 20, 30));
        stream.write_all(&update).unwrap();
    });

    let mut client = Client::connect(addr, Duration::from_secs(5)).unwrap();
    assert_eq!(client.name(), "smolvm");
    let frame = client.refresh().unwrap();
    assert_eq!((frame.width(), frame.height()), (3, 2));
    assert_eq!(frame.pixel(0, 1), Rgb(0, 0, 0));
    assert_eq!(frame.pixel(1, 1), Rgb(255, 0, 255));
    assert_eq!(frame.pixel(2, 1), Rgb(10, 20, 30));
    assert_eq!(frame.find(Rgb(255, 0, 255)), Some((1, 1)));
    server.join().unwrap();
}

#[test]
fn a_rectangle_outside_the_framebuffer_is_a_protocol_error() {
    let (addr, server) = serve(|mut stream| {
        server_handshake(&mut stream, 2, 2);
        let mut request = [0; 10];
        stream.read_exact(&mut request).unwrap();
        let mut update = vec![0, 0, 0, 1];
        for v in [1u16, 1, 2, 2] {
            update.extend_from_slice(&v.to_be_bytes());
        }
        update.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        stream.write_all(&update).unwrap();
    });

    let mut client = Client::connect(addr, Duration::from_secs(5)).unwrap();
    let error = client.refresh().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    server.join().unwrap();
}

#[test]
fn shifted_text_and_clicks_send_us_layout_events() {
    let (addr, server) = serve(|mut stream| {
        server_handshake(&mut stream, 1, 1);
        let mut sent = vec![0; 8 * 6 + 6 * 3];
        stream.read_exact(&mut sent).unwrap();
        let keys: Vec<(u8, u32)> = sent[..48]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|m| {
                assert_eq!(m[0], 4);
                (m[1], u32::from_be_bytes([m[4], m[5], m[6], m[7]]))
            })
            .collect();
        assert_eq!(
            keys,
            [(1, KEY_SHIFT), (1, u32::from('A')), (0, u32::from('A')), (0, KEY_SHIFT), (1, u32::from('b')), (0, u32::from('b'))]
        );
        let pointer = sent[48..].as_chunks::<6>().0;
        assert_eq!(pointer, [[5, 0, 0, 7, 0, 9], [5, 1, 0, 7, 0, 9], [5, 0, 0, 7, 0, 9]]);
    });

    let mut client = Client::connect(addr, Duration::from_secs(5)).unwrap();
    client.type_text("Ab").unwrap();
    client.click(7, 9).unwrap();
    server.join().unwrap();
}

#[test]
fn changed_pixels_counts_only_colour_differences() {
    let mut a = Frame::blank(2, 1);
    let mut b = Frame::blank(2, 1);
    a.paint(0, 0, 2, &[bgrx(1, 2, 3), bgrx(4, 5, 6)].concat());
    b.paint(0, 0, 2, &[[3, 2, 1, 99], bgrx(9, 9, 9)].concat());
    assert_eq!(a.changed_pixels(&b), 1);
    assert_eq!(a.changed_pixels(&Frame::blank(1, 1)), 2);
}
