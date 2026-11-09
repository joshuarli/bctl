use super::*;
use crate::shm::SurfacePixels;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use wayland_server::{Client, Resource};
use wayland_server::protocol::{wl_surface, wl_shm_pool};

struct Fixture {
    display: Display<State>,
    state: State,
    client: Client,
    socket: UnixStream,
}

impl Fixture {
    fn new() -> Self {
        Self::with_output((2, 1))
    }

    fn with_output(size: (u32, u32)) -> Self {
        let display = Display::new().unwrap();
        let mut dh = display.handle();
        let (socket, server) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let client = dh.insert_client(server, Arc::new(UdsClientData)).unwrap();
        let state = State::new(dh, size, Keyboard::new().unwrap());
        Self { display, state, client, socket }
    }

    fn filled_buffer(&mut self, width: i32, height: i32, fill: u8) -> wl_buffer::WlBuffer {
        let bytes = vec![fill; width as usize * height as usize * 4];
        let file = tempfile::tempfile().unwrap();
        file.set_len(bytes.len() as u64).unwrap();
        std::os::unix::fs::FileExt::write_all_at(&file, &bytes, 0).unwrap();
        let buffer = self.client.create_resource::<wl_buffer::WlBuffer, (), State>(&self.state.dh, 1, ()).unwrap();
        self.state.buffers.insert(buffer.id(), (buffer.clone(), ShmBufferData {
            file: Arc::new(file), offset: 0, width, height, stride: width * 4, format: ShmFormat::Xrgb8888,
        }));
        buffer
    }

    fn show(&mut self, surface: &wl_surface::WlSurface, width: i32, height: i32, fill: u8) -> Vec<(u32, u16, Vec<u32>)> {
        let buffer = self.filled_buffer(width, height, fill);
        self.request(surface, 1, &[buffer.id().protocol_id(), 0, 0]);
        messages(&self.request(surface, 6, &[]))
    }

    fn events(&mut self) -> Vec<(u32, u16, Vec<u32>)> {
        self.display.flush_clients().unwrap();
        messages(&drain_socket(&mut self.socket))
    }

    fn pointer_seat(&mut self) -> wayland_server::protocol::wl_pointer::WlPointer {
        use wayland_server::protocol::{wl_keyboard, wl_pointer};
        self.state.has_pointer = true;
        let seat = self.client.create_resource::<wl_seat::WlSeat, (), State>(&self.state.dh, 5, ()).unwrap();
        let mut seat = SeatData::new(seat);
        let pointer = self.client.create_resource::<wl_pointer::WlPointer, (), State>(&self.state.dh, 5, ()).unwrap();
        seat.pointers.push(pointer.clone());
        seat.keyboards.push(self.client.create_resource::<wl_keyboard::WlKeyboard, (), State>(&self.state.dh, 5, ()).unwrap());
        self.state.seats.push(seat);
        pointer
    }

    fn move_pointer(&mut self, x: f64, y: f64) {
        let (w, h) = self.state.out_size;
        self.state.pointer_frame(crate::input::PointerFrame {
            motion: Some(crate::input::Motion::Absolute { x: x / f64::from(w - 1), y: y / f64::from(h - 1) }),
            ..Default::default()
        });
    }

    fn press(&mut self, down: bool) {
        self.state.pointer_frame(crate::input::PointerFrame {
            buttons: vec![(crate::input::BTN_LEFT, down)],
            ..Default::default()
        });
    }

    fn surface(&mut self) -> wl_surface::WlSurface {
        let surface = self.client.create_resource::<wl_surface::WlSurface, (), State>(&self.state.dh, 4, ()).unwrap();
        self.state.surfaces.insert(surface.id(), SurfaceData::new(surface.clone()));
        surface
    }

    fn request(&mut self, object: &impl Resource, opcode: u16, args: &[u32]) -> Vec<u8> {
        self.request_id(object.id().protocol_id(), opcode, args)
    }

    fn request_id(&mut self, id: u32, opcode: u16, args: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(id.to_ne_bytes());
        bytes.extend((((8 + args.len() * 4) as u32) << 16 | opcode as u32).to_ne_bytes());
        for arg in args { bytes.extend(arg.to_ne_bytes()); }
        self.socket.write_all(&bytes).unwrap();
        self.display.dispatch_clients(&mut self.state).unwrap();
        self.display.flush_clients().unwrap();
        let mut response = Vec::new();
        match self.socket.read_to_end(&mut response) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("read response: {error}"),
        }
        response
    }

    fn buffer(&mut self, bytes: &[u8]) -> wl_buffer::WlBuffer {
        let file = tempfile::tempfile().unwrap();
        file.set_len(bytes.len() as u64).unwrap();
        std::os::unix::fs::FileExt::write_all_at(&file, bytes, 0).unwrap();
        let buffer = self.client.create_resource::<wl_buffer::WlBuffer, (), State>(&self.state.dh, 1, ()).unwrap();
        self.state.buffers.insert(buffer.id(), (buffer.clone(), ShmBufferData {
            file: Arc::new(file), offset: 0, width: 2, height: 1, stride: 8, format: ShmFormat::Xrgb8888,
        }));
        buffer
    }

    fn toplevel(&mut self, surface: &wl_surface::WlSurface) -> wayland_protocols::xdg::shell::server::xdg_surface::XdgSurface {
        use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel};
        let xdg = self.client.create_resource::<xdg_surface::XdgSurface, (), State>(&self.state.dh, 1, ()).unwrap();
        let base = self.client.create_resource::<xdg_wm_base::XdgWmBase, (), State>(&self.state.dh, 1, ()).unwrap();
        let mut xdg_data = XdgSurfaceData::new(base, surface.id());
        xdg_data.has_toplevel = true;
        self.state.xdg_surfaces.insert(xdg.id(), xdg_data);
        let top = self.client.create_resource::<xdg_toplevel::XdgToplevel, (), State>(&self.state.dh, 1, ()).unwrap();
        let mut data = ToplevelData::new(top.clone(), xdg.clone(), surface.id());
        data.configures.send(7);
        self.state.toplevels.insert(top.id(), data);
        xdg
    }
}

#[test]
fn synchronized_child_waits_for_parent_and_preserves_uncommitted_state() {
    let mut f = Fixture::new();
    let parent = f.surface();
    let child = f.surface();
    f.state.surfaces.get_mut(&parent.id()).unwrap().stacking.push(child.id());
    f.state.surfaces.get_mut(&child.id()).unwrap().parent = Some(parent.id());
    let buffer = f.buffer(&[1; 8]);
    f.request(&child, 1, &[buffer.id().protocol_id(), 0, 0]);
    f.request(&child, 6, &[]);
    assert!(f.state.surfaces[&child.id()].pixels.is_none());
    let pending = f.buffer(&[2; 8]);
    f.request(&child, 1, &[pending.id().protocol_id(), 0, 0]);
    f.request(&parent, 6, &[]);
    assert_eq!(f.state.surfaces[&child.id()].pixels.as_ref().unwrap().bytes, [1; 8]);
    assert!(f.state.surfaces[&child.id()].pending.buffer.is_some());
    f.request(&child, 6, &[]);
    f.request(&parent, 6, &[]);
    assert_eq!(f.state.surfaces[&child.id()].pixels.as_ref().unwrap().bytes, [2; 8]);
}

#[test]
fn release_allows_client_reuse_without_changing_retained_pixels() {
    let mut f = Fixture::new();
    let surface = f.surface();
    let buffer = f.buffer(&[1, 2, 3, 255, 4, 5, 6, 255]);
    f.request(&surface, 1, &[buffer.id().protocol_id(), 0, 0]);
    let release = f.request(&surface, 6, &[]);
    assert_eq!(&release[..4], &buffer.id().protocol_id().to_ne_bytes());
    f.state.buffers[&buffer.id()].1.file.set_len(0).unwrap();
    f.request(&buffer, 0, &[]);
    assert_eq!(f.state.surfaces[&surface.id()].pixels.as_ref().unwrap().bytes,
        [1, 2, 3, 255, 4, 5, 6, 255]);
}

#[test]
fn frame_callbacks_wait_for_commit_and_successful_presentation() {
    let mut f = Fixture::new();
    let surface = f.surface();
    let xdg = f.toplevel(&surface);
    f.request(&xdg, 4, &[7]);
    let buffer = f.buffer(&[1; 8]);
    f.request(&surface, 1, &[buffer.id().protocol_id(), 0, 0]);
    let pending = f.request(&surface, 3, &[2]);
    assert!(pending.is_empty());
    f.state.presented(100);
    assert!(f.state.surfaces[&surface.id()].callbacks.is_empty());
    f.request(&surface, 6, &[]);
    assert_eq!(f.state.surfaces[&surface.id()].callbacks.len(), 1);
    f.state.compose();
    assert_eq!(f.state.surfaces[&surface.id()].callbacks.len(), 1);
    f.state.presented(123);
    assert!(f.state.surfaces[&surface.id()].callbacks.is_empty());
    f.display.flush_clients().unwrap();
    let mut response = Vec::new();
    let _ = f.socket.read_to_end(&mut response);
    assert_eq!(&response[..12], &[2u32.to_ne_bytes(), (12u32 << 16).to_ne_bytes(), 123u32.to_ne_bytes()].concat());
}

#[test]
fn invalid_configure_serial_posts_protocol_error() {
    let mut f = Fixture::new();
    let surface = f.surface();
    let xdg = f.toplevel(&surface);
    let response = f.request(&xdg, 4, &[6]);
    assert_eq!(&response[..4], &1u32.to_ne_bytes());
    assert!(String::from_utf8_lossy(&response).contains("unknown configure serial"));
}

#[test]
fn invalid_stride_disconnects_client_instead_of_creating_buffer() {
    let mut f = Fixture::new();
    let pool = f.client.create_resource::<wl_shm_pool::WlShmPool, (), State>(&f.state.dh, 1, ()).unwrap();
    let file = tempfile::tempfile().unwrap();
    file.set_len(16).unwrap();
    f.state.pools.insert(pool.id(), ShmPoolData::new(file, 16).unwrap());
    let response = f.request(&pool, 0, &[2, 0, 2, 2, 4, 1]);
    assert!(String::from_utf8_lossy(&response).contains("stride smaller than row"));
    assert!(f.state.buffers.is_empty());
}

#[test]
fn destroying_child_detaches_parent_and_clears_keyboard_focus() {
    let mut f = Fixture::new();
    let parent = f.surface();
    let child = f.surface();
    f.state.surfaces.get_mut(&parent.id()).unwrap().stacking.push(child.id());
    f.state.surfaces.get_mut(&child.id()).unwrap().parent = Some(parent.id());
    let seat = f.client.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    let mut seat = SeatData::new(seat);
    seat.keyboard_focus = Some(child.id());
    f.state.seats.push(seat);
    f.request(&child, 0, &[]);
    assert!(!f.state.surfaces.contains_key(&child.id()));
    assert_eq!(f.state.surfaces[&parent.id()].stacking, [parent.id()]);
    assert!(f.state.seats[0].keyboard_focus.is_none());
}

#[test]
fn premultiplied_argb_blends_over_existing_pixels_and_clips() {
    let mut fb = vec![100, 80, 60, 255, 100, 80, 60, 255];
    blit_rows(&mut fb, 2, 1, &[20, 30, 40, 128, 20, 30, 40, 128], 2, 1, -1, 0, ShmFormat::Argb8888);
    assert_eq!(fb, [69, 69, 69, 255, 100, 80, 60, 255]);
    blit_rows(&mut fb, 2, 1, &[255; 8], 2, 1, i32::MAX, i32::MAX, ShmFormat::Xrgb8888);
    assert_eq!(fb[4..], [100, 80, 60, 255]);
}

#[test]
fn abgr_and_xbgr_swap_red_and_blue_into_the_framebuffer() {
    let mut fb = vec![0; 4];
    blit_rows(&mut fb, 1, 1, &[10, 20, 30, 0], 1, 1, 0, 0, ShmFormat::Xbgr8888);
    assert_eq!(fb, [30, 20, 10, 255]);
    let mut fb = vec![100, 80, 60, 255];
    blit_rows(&mut fb, 1, 1, &[40, 30, 20, 128], 1, 1, 0, 0, ShmFormat::Abgr8888);
    assert_eq!(fb, [69, 69, 69, 255]);
}

#[test]
fn buffer_commit_before_configure_ack_is_a_protocol_error() {
    let mut f = Fixture::new();
    let surface = f.surface();
    f.toplevel(&surface);
    let buffer = f.buffer(&[1; 8]);
    f.request(&surface, 1, &[buffer.id().protocol_id(), 0, 0]);
    let response = f.request(&surface, 6, &[]);
    assert!(String::from_utf8_lossy(&response).contains("buffer committed before configure ack"));
}

#[test]
fn subsurface_parent_cycle_is_rejected_without_recursion() {
    use wayland_server::protocol::wl_subcompositor;
    let mut f = Fixture::new();
    let parent = f.surface();
    let child = f.surface();
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&sub, 1, &[2, child.id().protocol_id(), parent.id().protocol_id()]);
    let response = f.request(&sub, 1, &[3, parent.id().protocol_id(), child.id().protocol_id()]);
    assert!(String::from_utf8_lossy(&response).contains("subsurface parent cycle"));
}

#[test]
fn positioner_rejects_nonpositive_size_without_panicking() {
    let mut f = Fixture::new();
    let base = f.client.create_resource::<xdg_wm_base::XdgWmBase, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&base, 1, &[2]);
    let mut bytes = Vec::new();
    for word in [2u32, 16u32 << 16 | 1, 0, 4] { bytes.extend(word.to_ne_bytes()); }
    f.socket.write_all(&bytes).unwrap();
    f.display.dispatch_clients(&mut f.state).unwrap();
    f.display.flush_clients().unwrap();
    let mut response = Vec::new();
    let _ = f.socket.read_to_end(&mut response);
    assert!(String::from_utf8_lossy(&response).contains("positioner size must be positive"));
}

#[test]
fn app_owner_kills_and_reaps_child_on_early_return() {
    let child = std::process::Command::new("sh").args(["-c", "kill -STOP $$"]).spawn().unwrap();
    let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
    drop(App(Some(child)));
    assert!(matches!(rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty()), Err(rustix::io::Errno::CHILD)));
}

#[test]
fn desync_applies_cached_commit_without_consuming_pending_attachment() {
    use wayland_server::protocol::{wl_subcompositor, wl_subsurface};
    let mut f = Fixture::new();
    let parent = f.surface();
    let child = f.surface();
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&sub, 1, &[2, child.id().protocol_id(), parent.id().protocol_id()]);
    let subsurface = f.state.subsurfaces.keys().next().unwrap().clone();
    let subsurface = wl_subsurface::WlSubsurface::from_id(&f.state.dh, subsurface).unwrap();
    let committed = f.buffer(&[1; 8]);
    f.request(&child, 1, &[committed.id().protocol_id(), 0, 0]);
    f.request(&child, 6, &[]);
    let uncommitted = f.buffer(&[2; 8]);
    f.request(&child, 1, &[uncommitted.id().protocol_id(), 0, 0]);
    f.request(&subsurface, 5, &[]);
    assert_eq!(f.state.surfaces[&child.id()].pixels.as_ref().unwrap().bytes, [1; 8]);
    assert!(f.state.surfaces[&child.id()].pending.buffer.is_some());
}

#[test]
fn clipboard_offers_follow_focus_and_source_destruction_clears_selection() {
    use wayland_server::protocol::{wl_data_device, wl_data_source};
    let mut f = Fixture::new();
    let surface = f.surface();
    let xdg = f.toplevel(&surface);
    let top_id = f.state.toplevels.iter().find(|(_, t)| t.surface == surface.id()).map(|(id, _)| id.clone()).unwrap();
    let seat = f.client.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats.push(SeatData::new(seat));
    let source = f.client.create_resource::<wl_data_source::WlDataSource, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.sources.insert(source.id(), crate::selection::DataSourceData {
        resource: source.clone(), mime_types: vec!["text/plain".into()],
    });
    let device = f.client.create_resource::<wl_data_device::WlDataDevice, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.devices.insert(device.id(), device.clone());
    f.request(&device, 1, &[source.id().protocol_id(), 0]);
    assert!(f.state.selection.selection.is_none());
    let _ = xdg;
    f.state.toplevels.get_mut(&top_id).unwrap().mapped = true;
    f.state.set_active(top_id);
    let response = f.request(&device, 1, &[source.id().protocol_id(), 0]);
    assert!(String::from_utf8_lossy(&response).contains("text/plain"));
    assert_eq!(f.state.selection.offers.len(), 1);
    let response = f.request(&source, 1, &[]);
    assert!(f.state.selection.selection.is_none());
    assert_eq!(&response[response.len() - 4..], &0u32.to_ne_bytes());
}

#[test]
fn subsurface_restacking_waits_for_parent_commit() {
    use wayland_server::protocol::{wl_subcompositor, wl_subsurface};
    let mut f = Fixture::new();
    let parent = f.surface();
    let child = f.surface();
    let xdg = f.toplevel(&parent);
    f.request(&xdg, 4, &[7]);
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&sub, 1, &[2, child.id().protocol_id(), parent.id().protocol_id()]);
    let id = f.state.subsurfaces.keys().next().unwrap().clone();
    let sub = wl_subsurface::WlSubsurface::from_id(&f.state.dh, id).unwrap();
    let lower = f.buffer(&[10; 8]);
    let upper = f.buffer(&[20; 8]);
    f.request(&parent, 1, &[lower.id().protocol_id(), 0, 0]);
    f.request(&child, 1, &[upper.id().protocol_id(), 0, 0]);
    f.request(&child, 6, &[]);
    f.request(&parent, 6, &[]);
    assert_eq!(f.state.compose()[0], 20);
    f.request(&sub, 3, &[parent.id().protocol_id()]);
    assert_eq!(f.state.compose()[0], 20);
    f.request(&parent, 6, &[]);
    assert_eq!(f.state.compose()[0], 10);
}

#[test]
fn output_bound_at_version_one_receives_only_version_one_events() {
    let mut f = Fixture::new();
    f.state.dh.create_global::<State, wl_output::WlOutput, ()>(2, ());
    let globals = f.request_id(1, 1, &[2]);
    let name = u32::from_ne_bytes(globals[8..12].try_into().unwrap());
    let mut bind = vec![name, 10];
    for bytes in b"wl_output\0\0\0".chunks_exact(4) { bind.push(u32::from_ne_bytes(bytes.try_into().unwrap())); }
    bind.extend([1, 3]);
    let response = f.request_id(2, 0, &bind);
    let mut events = Vec::new();
    let mut offset = 0;
    while offset < response.len() {
        let header = u32::from_ne_bytes(response[offset + 4..offset + 8].try_into().unwrap());
        events.push(header & 0xffff);
        offset += (header >> 16) as usize;
    }
    assert_eq!(events, [0, 1]);
}

#[test]
fn destroying_toplevel_unmaps_pixels_and_clears_keyboard_focus() {
    let mut f = Fixture::new();
    let surface = f.surface();
    let xdg = f.toplevel(&surface);
    let top = f.state.toplevels.values().next().unwrap().resource.clone();
    let seat = f.client.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats.push(SeatData::new(seat));
    f.request(&xdg, 4, &[7]);
    let buffer = f.buffer(&[9; 8]);
    f.request(&surface, 1, &[buffer.id().protocol_id(), 0, 0]);
    f.request(&surface, 6, &[]);
    assert!(f.state.seats[0].keyboard_focus.is_some());
    assert_eq!(f.state.compose()[0], 9);
    f.state.pending_present = false;
    f.request(&top, 0, &[]);
    assert!(f.state.seats[0].keyboard_focus.is_none());
    assert!(f.state.pending_present);
    assert_eq!(f.state.compose(), [0; 8]);
}

#[test]
fn reparenting_an_existing_subtree_cannot_bypass_the_depth_limit() {
    use wayland_server::protocol::wl_subcompositor;
    let mut f = Fixture::new();
    let mut root = f.surface();
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    for depth in 2..=65 {
        let parent = f.surface();
        let response = f.request(&sub, 1, &[depth, root.id().protocol_id(), parent.id().protocol_id()]);
        if depth == 65 {
            assert!(String::from_utf8_lossy(&response).contains("subsurface nesting exceeds 64 levels"));
        } else {
            assert!(response.is_empty(), "valid depth {depth} was rejected");
        }
        root = parent;
    }
}

fn drain_socket(socket: &mut UnixStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match socket.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("drain socket: {error}"),
        }
    }
    out
}

fn contains_opcode(data: &[u8], opcode: u16) -> bool {
    let mut offset = 0;
    while offset + 8 <= data.len() {
        let size = (u32::from_ne_bytes(data[offset + 4..offset + 8].try_into().unwrap()) >> 16) as usize;
        let found = u32::from_ne_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as u16 & 0xffff == opcode;
        if found {
            return true;
        }
        if size < 8 || offset + size > data.len() {
            break;
        }
        offset += size;
    }
    false
}

#[test]
fn nested_position_waits_for_immediate_parent_commit() {
    use wayland_server::protocol::{wl_subcompositor, wl_subsurface};
    let mut f = Fixture::new();
    let root = f.surface();
    let parent = f.surface();
    let grandchild = f.surface();
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&sub, 1, &[2, parent.id().protocol_id(), root.id().protocol_id()]);
    f.request(&sub, 1, &[3, grandchild.id().protocol_id(), parent.id().protocol_id()]);
    let grand_sub_id = f.state.subsurfaces.iter().find(|(_, surf)| **surf == grandchild.id()).map(|(id, _)| id.clone()).unwrap();
    let grand_sub = wl_subsurface::WlSubsurface::from_id(&f.state.dh, grand_sub_id).unwrap();
    f.request(&grand_sub, 1, &[5, 0]);
    f.request(&root, 6, &[]);
    assert_eq!(f.state.surfaces[&grandchild.id()].subsurface_pos, (0, 0));
    f.request(&parent, 6, &[]);
    f.request(&root, 6, &[]);
    assert_eq!(f.state.surfaces[&grandchild.id()].subsurface_pos, (5, 0));
}

#[test]
fn later_position_does_not_leak_into_earlier_transaction() {
    use wayland_server::protocol::{wl_subcompositor, wl_subsurface};
    let mut f = Fixture::new();
    let root = f.surface();
    let parent = f.surface();
    let grandchild = f.surface();
    let sub = f.client.create_resource::<wl_subcompositor::WlSubcompositor, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&sub, 1, &[2, parent.id().protocol_id(), root.id().protocol_id()]);
    f.request(&sub, 1, &[3, grandchild.id().protocol_id(), parent.id().protocol_id()]);
    let grand_sub_id = f.state.subsurfaces.iter().find(|(_, surf)| **surf == grandchild.id()).map(|(id, _)| id.clone()).unwrap();
    let grand_sub = wl_subsurface::WlSubsurface::from_id(&f.state.dh, grand_sub_id).unwrap();
    f.request(&grand_sub, 1, &[9, 0]);
    f.request(&parent, 6, &[]);
    f.request(&grand_sub, 1, &[20, 0]);
    f.request(&root, 6, &[]);
    assert_eq!(f.state.surfaces[&grandchild.id()].subsurface_pos, (9, 0));
    assert_eq!(f.state.surfaces[&grandchild.id()].pending_pos, Some((20, 0)));
    f.request(&parent, 6, &[]);
    f.request(&root, 6, &[]);
    assert_eq!(f.state.surfaces[&grandchild.id()].subsurface_pos, (20, 0));
}

#[test]
fn logical_focus_is_single_active_toplevel_across_two_clients() {
    use std::sync::Arc;
    use wayland_server::protocol::{wl_data_device, wl_data_source, wl_keyboard, wl_seat};
    let mut f = Fixture::new();
    let (mut socket2, server2) = UnixStream::pair().unwrap();
    socket2.set_nonblocking(true).unwrap();
    let client2 = f.state.dh.insert_client(server2, Arc::new(UdsClientData)).unwrap();

    let surface_a = f.surface();
    let surface_b = client2.create_resource::<wl_surface::WlSurface, (), State>(&f.state.dh, 4, ()).unwrap();
    f.state.surfaces.insert(surface_b.id(), SurfaceData::new(surface_b.clone()));

    let xdg_a = f.toplevel(&surface_a);
    let _ = xdg_a;
    let top_a = f.state.toplevels.iter().find(|(_, t)| t.surface == surface_a.id()).map(|(id, _)| id.clone()).unwrap();
    let xdg_b = client2.create_resource::<wayland_protocols::xdg::shell::server::xdg_surface::XdgSurface, (), State>(&f.state.dh, 1, ()).unwrap();
    let base_b = client2.create_resource::<xdg_wm_base::XdgWmBase, (), State>(&f.state.dh, 1, ()).unwrap();
    f.state.xdg_surfaces.insert(xdg_b.id(), XdgSurfaceData::new(base_b, surface_b.id()));
    let top_b_res = client2.create_resource::<wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel, (), State>(&f.state.dh, 1, ()).unwrap();
    let mut data_b = ToplevelData::new(top_b_res.clone(), xdg_b.clone(), surface_b.id());
    data_b.configures.send(7);
    data_b.configures.ack(7);
    let top_b = top_b_res.id();
    f.state.toplevels.insert(top_b.clone(), data_b);
    f.state.toplevels.get_mut(&top_a).unwrap().configures.ack(7);

    let seat_a = f.client.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats.push(SeatData::new(seat_a));
    let seat_b = client2.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats.push(SeatData::new(seat_b));
    let kb_a = f.client.create_resource::<wl_keyboard::WlKeyboard, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats[0].keyboards.push(kb_a);
    let kb_b = client2.create_resource::<wl_keyboard::WlKeyboard, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats[1].keyboards.push(kb_b);

    f.state.surfaces.get_mut(&surface_a.id()).unwrap().pixels = Some(SurfacePixels {
        bytes: vec![11; 8], width: 2, height: 1, format: ShmFormat::Xrgb8888,
    });
    f.state.surfaces.get_mut(&surface_b.id()).unwrap().pixels = Some(SurfacePixels {
        bytes: vec![22; 8], width: 2, height: 1, format: ShmFormat::Xrgb8888,
    });

    f.state.toplevels.get_mut(&top_a).unwrap().mapped = true;
    f.state.set_active(top_a.clone());
    f.display.flush_clients().unwrap();
    let _ = drain_socket(&mut f.socket);
    let _ = drain_socket(&mut socket2);
    assert_eq!(f.state.seats[0].keyboard_focus, Some(surface_a.id()));
    assert!(f.state.seats[1].keyboard_focus.is_none());
    assert_eq!(f.state.compose()[0], 11);

    f.state.toplevels.get_mut(&top_b).unwrap().mapped = true;
    f.state.set_active(top_b.clone());
    f.display.flush_clients().unwrap();
    let got_a = drain_socket(&mut f.socket);
    let got_b = drain_socket(&mut socket2);
    assert!(f.state.seats[0].keyboard_focus.is_none());
    assert_eq!(f.state.seats[1].keyboard_focus, Some(surface_b.id()));
    assert_eq!(f.state.compose()[0], 22);
    assert!(contains_opcode(&got_a, 2), "old client should receive leave");
    assert!(contains_opcode(&got_b, 1), "new client should receive enter");

    let _ = drain_socket(&mut f.socket);
    let _ = drain_socket(&mut socket2);
    f.state.key(30, true);
    f.display.flush_clients().unwrap();
    let keys_a = drain_socket(&mut f.socket);
    let keys_b = drain_socket(&mut socket2);
    assert!(keys_a.is_empty(), "unfocused client must not receive keys");
    assert!(contains_opcode(&keys_b, 3), "focused client must receive key");
    f.state.key(30, false);

    let source_a = f.client.create_resource::<wl_data_source::WlDataSource, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.sources.insert(source_a.id(), crate::selection::DataSourceData {
        resource: source_a.clone(), mime_types: vec!["text/plain".into()],
    });
    let device_a = f.client.create_resource::<wl_data_device::WlDataDevice, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.devices.insert(device_a.id(), device_a.clone());
    let source_b = client2.create_resource::<wl_data_source::WlDataSource, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.sources.insert(source_b.id(), crate::selection::DataSourceData {
        resource: source_b.clone(), mime_types: vec!["text/plain".into()],
    });
    let device_b = client2.create_resource::<wl_data_device::WlDataDevice, (), State>(&f.state.dh, 3, ()).unwrap();
    f.state.selection.devices.insert(device_b.id(), device_b.clone());
    let serial = f.state.serial;
    let response = f.request(&device_a, 1, &[source_a.id().protocol_id(), serial]);
    assert!(f.state.selection.selection.is_none(), "unfocused client must not set selection");
    assert!(!String::from_utf8_lossy(&response).contains("text/plain"));
    f.state.selection.selection = Some(source_b.id());
    f.state.send_selection();
    f.display.flush_clients().unwrap();
    let clip_a = drain_socket(&mut f.socket);
    let clip_b = drain_socket(&mut socket2);
    assert!(!String::from_utf8_lossy(&clip_a).contains("text/plain"), "unfocused client must not receive offer");
    assert!(String::from_utf8_lossy(&clip_b).contains("text/plain"), "focused client receives offer");
}

#[test]
fn two_toplevels_from_same_client_share_one_active_surface() {
    use wayland_server::protocol::{wl_keyboard, wl_seat};
    let mut f = Fixture::new();
    let first = f.surface();
    let second = f.surface();
    let _ = f.toplevel(&first);
    let _ = f.toplevel(&second);
    let top_first = f.state.toplevels.iter().find(|(_, t)| t.surface == first.id()).map(|(id, _)| id.clone()).unwrap();
    let top_second = f.state.toplevels.iter().find(|(_, t)| t.surface == second.id()).map(|(id, _)| id.clone()).unwrap();
    f.state.toplevels.get_mut(&top_first).unwrap().configures.ack(7);
    f.state.toplevels.get_mut(&top_second).unwrap().configures.ack(7);
    let seat = f.client.create_resource::<wl_seat::WlSeat, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats.push(SeatData::new(seat));
    let kb = f.client.create_resource::<wl_keyboard::WlKeyboard, (), State>(&f.state.dh, 5, ()).unwrap();
    f.state.seats[0].keyboards.push(kb);
    f.state.surfaces.get_mut(&first.id()).unwrap().pixels = Some(SurfacePixels {
        bytes: vec![33; 8], width: 2, height: 1, format: ShmFormat::Xrgb8888,
    });
    f.state.surfaces.get_mut(&second.id()).unwrap().pixels = Some(SurfacePixels {
        bytes: vec![44; 8], width: 2, height: 1, format: ShmFormat::Xrgb8888,
    });
    f.state.toplevels.get_mut(&top_first).unwrap().mapped = true;
    f.state.set_active(top_first.clone());
    assert_eq!(f.state.compose()[0], 33);
    assert_eq!(f.state.seats[0].keyboard_focus, Some(first.id()));
    f.state.toplevels.get_mut(&top_second).unwrap().mapped = true;
    f.state.set_active(top_second.clone());
    assert_eq!(f.state.compose()[0], 44);
    assert_eq!(f.state.seats[0].keyboard_focus, Some(second.id()));
    assert_eq!(f.state.active_surface(), Some(second.id()));
}

#[test]
fn per_client_memory_budget_rejects_without_harming_healthy_client() {
    use std::sync::Arc;
    let mut f = Fixture::new();
    let (mut socket2, server2) = UnixStream::pair().unwrap();
    socket2.set_nonblocking(true).unwrap();
    let client2 = f.state.dh.insert_client(server2, Arc::new(UdsClientData)).unwrap();
    let offender = f.surface();
    let healthy_surface = client2
        .create_resource::<wl_surface::WlSurface, (), State>(&f.state.dh, 4, ())
        .unwrap();
    f.state
        .surfaces
        .insert(healthy_surface.id(), SurfaceData::new(healthy_surface.clone()));
    let offender_buffer = f.buffer(&[1; 8]);
    let healthy_buffer = {
        let file = tempfile::tempfile().unwrap();
        file.set_len(8).unwrap();
        std::os::unix::fs::FileExt::write_all_at(&file, &[2; 8], 0).unwrap();
        let buffer = client2.create_resource::<wl_buffer::WlBuffer, (), State>(&f.state.dh, 1, ()).unwrap();
        f.state.buffers.insert(
            buffer.id(),
            (
                buffer.clone(),
                ShmBufferData {
                    file: Arc::new(file),
                    offset: 0,
                    width: 2,
                    height: 1,
                    stride: 8,
                    format: ShmFormat::Xrgb8888,
                },
            ),
        );
        buffer
    };
    let offender_client = f.client.id();
    f.state.client_bytes.insert(offender_client, crate::server::MAX_PER_CLIENT_BYTES - 4);
    f.state.total_bytes = crate::server::MAX_PER_CLIENT_BYTES - 4;
    f.request(&offender, 1, &[offender_buffer.id().protocol_id(), 0, 0]);
    let response = f.request(&offender, 6, &[]);
    assert!(String::from_utf8_lossy(&response).contains("memory budget"));
    assert!(f.state.surfaces.get(&offender.id()).map(|s| s.pixels.is_none()).unwrap_or(true));
    let mut header = Vec::new();
    header.extend(healthy_surface.id().protocol_id().to_ne_bytes());
    header.extend((((8 + 3 * 4) as u32) << 16 | 1).to_ne_bytes());
    header.extend(healthy_buffer.id().protocol_id().to_ne_bytes());
    header.extend(0u32.to_ne_bytes());
    header.extend(0u32.to_ne_bytes());
    socket2.write_all(&header).unwrap();
    let mut commit = Vec::new();
    commit.extend(healthy_surface.id().protocol_id().to_ne_bytes());
    commit.extend(((8u32) << 16 | 6).to_ne_bytes());
    socket2.write_all(&commit).unwrap();
    f.display.dispatch_clients(&mut f.state).unwrap();
    f.display.flush_clients().unwrap();
    assert!(f.state.surfaces[&healthy_surface.id()].pixels.is_some());
    let _ = drain_socket(&mut socket2);
}

#[test]
fn callback_bound_disconnects_offender_only() {
    let mut f = Fixture::new();
    let surface = f.surface();
    for new_id in 2..(2 + crate::server::MAX_CALLBACKS_PER_SURFACE as u32) {
        let response = f.request(&surface, 3, &[new_id]);
        assert!(response.is_empty(), "callback {new_id} should be accepted");
    }
    let response = f.request(&surface, 3, &[2 + crate::server::MAX_CALLBACKS_PER_SURFACE as u32]);
    assert!(String::from_utf8_lossy(&response).contains("too many outstanding frame callbacks"));
}

#[test]
fn scale_and_transform_subset_is_accepted_but_ignored() {
    let mut f = Fixture::new();
    let surface = f.surface();
    assert!(f.request(&surface, 8, &[2]).is_empty());
    assert!(f.request(&surface, 7, &[1]).is_empty());
    let mut g = Fixture::new();
    let bad_scale = g.surface();
    let response = g.request(&bad_scale, 8, &[0]);
    assert!(String::from_utf8_lossy(&response).contains("buffer scale must be positive"));
    let mut h = Fixture::new();
    let bad_transform = h.surface();
    let response = h.request(&bad_transform, 7, &[999]);
    assert!(String::from_utf8_lossy(&response).contains("unknown buffer transform"));
}

fn messages(data: &[u8]) -> Vec<(u32, u16, Vec<u32>)> {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset + 8 <= data.len() {
        let object = u32::from_ne_bytes(data[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(data[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        assert!(size >= 8 && offset + size <= data.len(), "truncated message");
        let args = data[offset + 8..offset + size].chunks(4).map(|word| u32::from_ne_bytes(word.try_into().unwrap())).collect();
        out.push((object, header as u16, args));
        offset += size;
    }
    out
}

fn sent<'a>(events: &'a [(u32, u16, Vec<u32>)], object: &impl Resource, opcode: u16) -> Vec<&'a [u32]> {
    let id = object.id().protocol_id();
    events.iter().filter(|(o, op, _)| *o == id && *op == opcode).map(|(_, _, args)| args.as_slice()).collect()
}

fn fixed(value: f64) -> u32 {
    ((value * 256.0) as i32) as u32
}

const POINTER_ENTER: u16 = 0;
const POINTER_LEAVE: u16 = 1;
const POINTER_MOTION: u16 = 2;
const POINTER_BUTTON: u16 = 3;
const POINTER_AXIS: u16 = 4;
const POPUP_CONFIGURE: u16 = 0;
const POPUP_DONE: u16 = 1;

type Events = Vec<(u32, u16, Vec<u32>)>;

fn mapped_toplevel(f: &mut Fixture, fill: u8) -> (wl_surface::WlSurface, wayland_protocols::xdg::shell::server::xdg_surface::XdgSurface, Events) {
    let surface = f.surface();
    let xdg = f.toplevel(&surface);
    f.request(&xdg, 4, &[7]);
    let (w, h) = f.state.out_size;
    let events = f.show(&surface, w as i32, h as i32, fill);
    (surface, xdg, events)
}

#[test]
fn popup_is_placed_inside_the_output_takes_pointer_input_and_is_dismissed_by_an_outside_click() {
    use wayland_protocols::xdg::shell::server::xdg_positioner::{Anchor, ConstraintAdjustment, Gravity};
    let mut f = Fixture::with_output((100, 50));
    let pointer = f.pointer_seat();
    let (parent, parent_xdg, _) = mapped_toplevel(&mut f, 10);
    let base = f.client.create_resource::<xdg_wm_base::XdgWmBase, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&base, 1, &[2]);
    f.request_id(2, 1, &[30, 20]);
    f.request_id(2, 2, &[10, 40, 20, 5]);
    f.request_id(2, 3, &[Anchor::BottomLeft as u32]);
    f.request_id(2, 4, &[Gravity::BottomRight as u32]);
    f.request_id(2, 5, &[(ConstraintAdjustment::FlipY | ConstraintAdjustment::SlideX).bits()]);
    let popup_surface = f.surface();
    f.request(&base, 2, &[3, popup_surface.id().protocol_id()]);
    f.request_id(3, 2, &[4, parent_xdg.id().protocol_id(), 2]);
    let events = messages(&f.request(&popup_surface, 6, &[]));
    let configure: Vec<_> = events.iter().filter(|(o, op, _)| *o == 4 && *op == POPUP_CONFIGURE).collect();
    assert_eq!(configure.len(), 1, "{events:?}");
    assert_eq!(configure[0].2, [10, 20, 30, 20], "flipped above the anchor to stay inside the output");
    let serial = events.iter().find(|(o, op, _)| *o == 3 && *op == 0).unwrap().2[0];
    f.request_id(3, 4, &[serial]);
    f.show(&popup_surface, 30, 20, 99);
    assert!(f.state.popups[0].live());
    let frame = f.state.compose();
    assert_eq!(frame[(25 * 100 + 15) * 4], 99, "popup composes over its parent");
    assert_eq!(frame[(5 * 100 + 80) * 4], 10);

    f.move_pointer(15.0, 25.0);
    let events = f.events();
    let enter = sent(&events, &pointer, POINTER_ENTER);
    assert_eq!(enter.len(), 1);
    assert_eq!(enter[0][1..], [popup_surface.id().protocol_id(), fixed(5.0), fixed(5.0)]);
    assert_eq!(sent(&events, &pointer, POINTER_LEAVE)[0][1], parent.id().protocol_id());

    f.press(true);
    f.move_pointer(90.0, 5.0);
    let events = f.events();
    assert_eq!(sent(&events, &pointer, POINTER_BUTTON).len(), 1);
    assert!(sent(&events, &pointer, POINTER_LEAVE).is_empty(), "a held button keeps the implicit grab");
    assert_eq!(sent(&events, &pointer, POINTER_MOTION)[0][1..], [fixed(80.0), fixed(-15.0)]);
    f.press(false);
    let events = f.events();
    assert_eq!(sent(&events, &pointer, POINTER_ENTER)[0][1], parent.id().protocol_id());
    assert!(events.iter().all(|(o, op, _)| !(*o == 4 && *op == POPUP_DONE)), "a click that started inside keeps the popup");

    f.press(true);
    let events = f.events();
    assert!(events.iter().any(|(o, op, _)| *o == 4 && *op == POPUP_DONE), "{events:?}");
    let button = sent(&events, &pointer, POINTER_BUTTON);
    assert_eq!(button.len(), 1, "the dismissing click still reaches the surface under the pointer");
    assert!(!f.state.popups[0].live());
    assert_eq!(f.state.compose()[(25 * 100 + 15) * 4], 10);
    f.press(false);
}

#[test]
fn popup_needs_a_complete_positioner_and_a_parent() {
    let mut f = Fixture::with_output((100, 50));
    let (_, parent_xdg, _) = mapped_toplevel(&mut f, 10);
    let base = f.client.create_resource::<xdg_wm_base::XdgWmBase, (), State>(&f.state.dh, 1, ()).unwrap();
    f.request(&base, 1, &[2]);
    f.request_id(2, 1, &[30, 20]);
    let popup_surface = f.surface();
    f.request(&base, 2, &[3, popup_surface.id().protocol_id()]);
    let response = f.request_id(3, 2, &[4, parent_xdg.id().protocol_id(), 2]);
    assert!(String::from_utf8_lossy(&response).contains("positioner needs a size and an anchor rectangle"));
    assert!(f.state.popups.is_empty());
}

#[test]
fn second_toplevel_covers_the_primary_and_focus_returns_when_it_closes() {
    let mut f = Fixture::with_output((4, 2));
    let pointer = f.pointer_seat();
    let keyboard = f.state.seats[0].keyboards[0].clone();
    let (primary, _, _) = mapped_toplevel(&mut f, 10);
    let primary_top = f.state.toplevel_for_surface(&primary.id()).unwrap();
    assert!(f.state.toplevels[&primary_top].activated);

    let (second, _, events) = mapped_toplevel(&mut f, 20);
    let second_top = f.state.toplevel_for_surface(&second.id()).unwrap();
    assert_eq!(f.state.compose()[0], 20);
    assert_eq!(f.state.seats[0].keyboard_focus, Some(second.id()));
    assert!(f.state.toplevels[&second_top].activated);
    assert!(!f.state.toplevels[&primary_top].activated);
    assert_eq!(sent(&events, &f.state.toplevels[&primary_top].resource, 0).len(), 1, "primary is reconfigured as inactive");
    assert_eq!(sent(&events, &keyboard, 2)[0][1], primary.id().protocol_id());
    assert_eq!(sent(&events, &keyboard, 1)[0][1], second.id().protocol_id());
    assert_eq!(sent(&events, &pointer, POINTER_ENTER)[0][1], second.id().protocol_id());

    let closing = f.state.toplevels[&second_top].resource.clone();
    let events = messages(&f.request(&closing, 0, &[]));
    assert_eq!(f.state.compose()[0], 10);
    assert_eq!(f.state.seats[0].keyboard_focus, Some(primary.id()));
    assert!(f.state.toplevels[&primary_top].activated);
    assert_eq!(sent(&events, &f.state.toplevels[&primary_top].resource, 0).len(), 1, "primary is reactivated");
    assert_eq!(sent(&events, &keyboard, 1)[0][1], primary.id().protocol_id());
    assert_eq!(sent(&events, &pointer, POINTER_ENTER)[0][1], primary.id().protocol_id());

    let buffer = f.filled_buffer(4, 2, 30);
    f.request(&primary, 1, &[buffer.id().protocol_id(), 0, 0]);
    let response = f.request(&primary, 6, &[]);
    assert!(!String::from_utf8_lossy(&response).contains("configure"), "commits before acking a reconfigure are allowed");
    assert_eq!(f.state.compose()[0], 30);
}

#[test]
fn cursor_surface_is_drawn_at_the_hotspot_for_the_focused_client_only() {
    let mut f = Fixture::with_output((8, 4));
    let pointer = f.pointer_seat();
    mapped_toplevel(&mut f, 10);
    f.move_pointer(5.0, 2.0);
    let serial = f.state.pointer.enter_serial;
    let cursor = f.surface();
    f.request(&pointer, 0, &[serial.wrapping_sub(1), cursor.id().protocol_id(), 1, 1]);
    assert!(f.state.pointer.cursor.is_none(), "a stale serial cannot set the cursor");
    f.request(&pointer, 0, &[serial, cursor.id().protocol_id(), 1, 1]);
    f.show(&cursor, 2, 2, 77);
    let frame = f.state.compose();
    assert_eq!(frame[(8 + 4) * 4], 77);
    assert_eq!(frame[(2 * 8 + 5) * 4], 77);
    assert_eq!(frame[(3 * 8 + 6) * 4], 10);
    let role = f.surface();
    f.state.surfaces.get_mut(&role.id()).unwrap().has_role = true;
    let response = f.request(&pointer, 0, &[serial, role.id().protocol_id(), 0, 0]);
    assert!(String::from_utf8_lossy(&response).contains("another role"));
}

#[test]
fn wheel_notches_become_discrete_axis_events_with_wayland_sign() {
    let mut f = Fixture::with_output((8, 4));
    let pointer = f.pointer_seat();
    mapped_toplevel(&mut f, 10);
    f.move_pointer(3.0, 3.0);
    f.events();
    f.state.pointer_frame(crate::input::PointerFrame { wheel: (-2, 0), ..Default::default() });
    let events = f.events();
    let axis = sent(&events, &pointer, POINTER_AXIS);
    assert_eq!(axis.len(), 1);
    assert_eq!(axis[0][1..], [0, fixed(20.0)], "REL_WHEEL down is a positive vertical scroll");
}
