use std::ffi::{c_char, c_int, c_void};
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use tinyland::server::headless::Headless;

const OUTPUT_SIZE: (u32, u32) = (1024, 768);
const WL_FIXED_SCALE: f64 = 256.0;

#[repr(C)]
pub struct WlcsExtensionDescriptor {
    name: *const c_char,
    version: u32,
}

#[repr(C)]
pub struct WlcsIntegrationDescriptor {
    version: u32,
    num_extensions: usize,
    supported_extensions: *const WlcsExtensionDescriptor,
}

#[repr(C)]
pub struct WlcsPointer {
    version: u32,
    move_absolute: extern "C" fn(*mut WlcsPointer, i32, i32),
    move_relative: extern "C" fn(*mut WlcsPointer, i32, i32),
    button_up: extern "C" fn(*mut WlcsPointer, c_int),
    button_down: extern "C" fn(*mut WlcsPointer, c_int),
    destroy: extern "C" fn(*mut WlcsPointer),
}

#[repr(C)]
pub struct WlcsTouch {
    version: u32,
    touch_down: extern "C" fn(*mut WlcsTouch, i32, i32),
    touch_move: extern "C" fn(*mut WlcsTouch, i32, i32),
    touch_up: extern "C" fn(*mut WlcsTouch),
    destroy: extern "C" fn(*mut WlcsTouch),
}

#[repr(C)]
pub struct WlcsDisplayServer {
    version: u32,
    start: Option<extern "C" fn(*mut WlcsDisplayServer)>,
    stop: Option<extern "C" fn(*mut WlcsDisplayServer)>,
    create_client_socket: Option<extern "C" fn(*mut WlcsDisplayServer) -> c_int>,
    position_window_absolute:
        Option<extern "C" fn(*mut WlcsDisplayServer, *mut c_void, *mut c_void, c_int, c_int)>,
    create_pointer: Option<extern "C" fn(*mut WlcsDisplayServer) -> *mut WlcsPointer>,
    create_touch: Option<extern "C" fn(*mut WlcsDisplayServer) -> *mut WlcsTouch>,
    get_descriptor:
        Option<extern "C" fn(*const WlcsDisplayServer) -> *const WlcsIntegrationDescriptor>,
    start_on_this_thread: Option<extern "C" fn(*mut WlcsDisplayServer, *mut c_void)>,
}

#[repr(C)]
pub struct WlcsServerIntegration {
    version: u32,
    create_server: extern "C" fn(c_int, *const *const c_char) -> *mut WlcsDisplayServer,
    destroy_server: extern "C" fn(*mut WlcsDisplayServer),
}

#[repr(transparent)]
pub struct Exported<T>(T);
unsafe impl<T> Sync for Exported<T> {}

enum PointerAction {
    MoveTo(f64, f64),
    MoveBy(f64, f64),
    Button(u16, bool),
}

enum Command {
    Connect(Sender<UnixStream>),
    Pointer(PointerAction, Sender<()>),
    Stop(Sender<()>),
}

struct Control {
    commands: Sender<Command>,
    wake: UnixStream,
}

impl Control {
    fn send(&self, command: Command) {
        self.commands.send(command).expect("server thread is gone");
        (&self.wake).write_all(&[1]).expect("wake server thread");
    }

    fn call<T>(&self, build: impl FnOnce(Sender<T>) -> Command) -> T {
        let (reply, answer) = channel();
        self.send(build(reply));
        answer.recv().expect("server thread dropped the reply")
    }
}

#[repr(C)]
struct Server {
    base: WlcsDisplayServer,
    running: Option<(Arc<Control>, JoinHandle<()>)>,
}

#[repr(C)]
struct Pointer {
    base: WlcsPointer,
    control: Arc<Control>,
}

fn serve(commands: Receiver<Command>, mut wake: UnixStream, ready: Sender<()>) {
    let mut headless = Headless::new(OUTPUT_SIZE).expect("headless tinyland");
    ready.send(()).expect("start waiter is gone");
    loop {
        {
            let display_fd = headless.display_fd();
            let mut fds = [
                rustix::event::PollFd::new(&display_fd, rustix::event::PollFlags::IN),
                rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(error) => panic!("poll: {error}"),
            }
        }
        let mut drained = [0u8; 64];
        match wake.read(&mut drained) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("wake read: {error}"),
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::Connect(reply) => {
                    let client = headless.connect().expect("connect client");
                    reply.send(client).expect("connect waiter is gone");
                }
                Command::Pointer(action, reply) => {
                    match action {
                        PointerAction::MoveTo(x, y) => headless.move_pointer_to(x, y),
                        PointerAction::MoveBy(dx, dy) => headless.move_pointer_by(dx, dy),
                        PointerAction::Button(code, down) => headless.button(code, down),
                    }
                    headless.pump().expect("pump after pointer");
                    reply.send(()).expect("pointer waiter is gone");
                }
                Command::Stop(reply) => {
                    reply.send(()).expect("stop waiter is gone");
                    return;
                }
            }
        }
        headless.pump().expect("pump");
    }
}

extern "C" fn create_server(_argc: c_int, _argv: *const *const c_char) -> *mut WlcsDisplayServer {
    let server = Box::new(Server {
        base: WlcsDisplayServer {
            version: 3,
            start: Some(start),
            stop: Some(stop),
            create_client_socket: Some(create_client_socket),
            position_window_absolute: Some(position_window_absolute),
            create_pointer: Some(create_pointer),
            create_touch: Some(create_touch),
            get_descriptor: Some(get_descriptor),
            start_on_this_thread: None,
        },
        running: None,
    });
    Box::into_raw(server).cast()
}

extern "C" fn destroy_server(server: *mut WlcsDisplayServer) {
    let mut server = unsafe { Box::from_raw(server.cast::<Server>()) };
    stop_running(&mut server);
}

fn stop_running(server: &mut Server) {
    if let Some((control, thread)) = server.running.take() {
        control.call(Command::Stop);
        thread.join().expect("server thread panicked");
    }
}

extern "C" fn start(server: *mut WlcsDisplayServer) {
    let server = unsafe { &mut *server.cast::<Server>() };
    assert!(server.running.is_none(), "server already started");
    let (commands, receiver) = channel();
    let (wake, wake_receiver) = UnixStream::pair().expect("wake pair");
    wake_receiver.set_nonblocking(true).expect("wake nonblocking");
    let (ready, started) = channel();
    let thread = std::thread::spawn(move || serve(receiver, wake_receiver, ready));
    started.recv().expect("server thread failed to start");
    server.running = Some((Arc::new(Control { commands, wake }), thread));
}

extern "C" fn stop(server: *mut WlcsDisplayServer) {
    stop_running(unsafe { &mut *server.cast::<Server>() });
}

fn control_of(server: *mut WlcsDisplayServer) -> Arc<Control> {
    let server = unsafe { &*server.cast::<Server>() };
    server.running.as_ref().expect("server not started").0.clone()
}

extern "C" fn create_client_socket(server: *mut WlcsDisplayServer) -> c_int {
    control_of(server).call(Command::Connect).into_raw_fd()
}

extern "C" fn position_window_absolute(
    _server: *mut WlcsDisplayServer,
    _client: *mut c_void,
    _surface: *mut c_void,
    _x: c_int,
    _y: c_int,
) {
}

extern "C" fn create_pointer(server: *mut WlcsDisplayServer) -> *mut WlcsPointer {
    let pointer = Box::new(Pointer {
        base: WlcsPointer {
            version: 1,
            move_absolute: pointer_move_absolute,
            move_relative: pointer_move_relative,
            button_up: pointer_button_up,
            button_down: pointer_button_down,
            destroy: pointer_destroy,
        },
        control: control_of(server),
    });
    Box::into_raw(pointer).cast()
}

extern "C" fn create_touch(_server: *mut WlcsDisplayServer) -> *mut WlcsTouch {
    Box::into_raw(Box::new(WlcsTouch {
        version: 1,
        touch_down: touch_move,
        touch_move,
        touch_up: touch_release,
        destroy: touch_destroy,
    }))
}

extern "C" fn touch_move(_touch: *mut WlcsTouch, _x: i32, _y: i32) {}

extern "C" fn touch_release(_touch: *mut WlcsTouch) {}

extern "C" fn touch_destroy(touch: *mut WlcsTouch) {
    drop(unsafe { Box::from_raw(touch) });
}

fn pointer_action(pointer: *mut WlcsPointer, action: PointerAction) {
    let pointer = unsafe { &*pointer.cast::<Pointer>() };
    pointer.control.call(|reply| Command::Pointer(action, reply));
}

fn fixed(value: i32) -> f64 {
    f64::from(value) / WL_FIXED_SCALE
}

extern "C" fn pointer_move_absolute(pointer: *mut WlcsPointer, x: i32, y: i32) {
    pointer_action(pointer, PointerAction::MoveTo(fixed(x), fixed(y)));
}

extern "C" fn pointer_move_relative(pointer: *mut WlcsPointer, dx: i32, dy: i32) {
    pointer_action(pointer, PointerAction::MoveBy(fixed(dx), fixed(dy)));
}

extern "C" fn pointer_button_up(pointer: *mut WlcsPointer, button: c_int) {
    pointer_action(pointer, PointerAction::Button(u16::try_from(button).expect("button code"), false));
}

extern "C" fn pointer_button_down(pointer: *mut WlcsPointer, button: c_int) {
    pointer_action(pointer, PointerAction::Button(u16::try_from(button).expect("button code"), true));
}

extern "C" fn pointer_destroy(pointer: *mut WlcsPointer) {
    drop(unsafe { Box::from_raw(pointer.cast::<Pointer>()) });
}

const fn extension(name: &'static [u8], version: u32) -> Exported<WlcsExtensionDescriptor> {
    Exported(WlcsExtensionDescriptor { name: name.as_ptr().cast(), version })
}

static EXTENSIONS: [Exported<WlcsExtensionDescriptor>; 7] = [
    extension(b"wl_compositor\0", 4),
    extension(b"wl_subcompositor\0", 1),
    extension(b"wl_shm\0", 1),
    extension(b"wl_output\0", 2),
    extension(b"wl_seat\0", 5),
    extension(b"wl_data_device_manager\0", 3),
    extension(b"xdg_wm_base\0", 1),
];

static DESCRIPTOR: Exported<WlcsIntegrationDescriptor> = Exported(WlcsIntegrationDescriptor {
    version: 1,
    num_extensions: EXTENSIONS.len(),
    supported_extensions: EXTENSIONS.as_ptr().cast(),
});

extern "C" fn get_descriptor(_server: *const WlcsDisplayServer) -> *const WlcsIntegrationDescriptor {
    &DESCRIPTOR.0
}

#[no_mangle]
pub static wlcs_server_integration: Exported<WlcsServerIntegration> = Exported(WlcsServerIntegration {
    version: 1,
    create_server,
    destroy_server,
});
