use super::{create_globals, State, UdsClientData};
use crate::input::{Keyboard, Motion, PointerFrame};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use wayland_server::Display;

pub struct Headless {
    display: Display<State>,
    state: State,
}

impl Headless {
    pub fn new(size: (u32, u32)) -> Result<Self, String> {
        let display = Display::new().map_err(|error| format!("wayland display: {error}"))?;
        let dh = display.handle();
        create_globals(&dh);
        let keyboard = Keyboard::new()?;
        let mut state = State::new(dh, size, keyboard);
        state.has_pointer = true;
        Ok(Self { display, state })
    }

    pub fn connect(&mut self) -> Result<UnixStream, String> {
        let (client, server) = UnixStream::pair().map_err(|error| format!("socketpair: {error}"))?;
        self.display
            .handle()
            .insert_client(server, Arc::new(UdsClientData))
            .map_err(|error| format!("insert client: {error}"))?;
        Ok(client)
    }

    pub fn display_fd(&self) -> BorrowedFd<'_> {
        self.display.as_fd()
    }

    pub fn move_pointer_to(&mut self, x: f64, y: f64) {
        let (width, height) = self.state.out_size;
        let motion = Motion::Absolute {
            x: x / f64::from(width.max(2) - 1),
            y: y / f64::from(height.max(2) - 1),
        };
        self.state.pointer_frame(PointerFrame { motion: Some(motion), ..Default::default() });
    }

    pub fn move_pointer_by(&mut self, dx: f64, dy: f64) {
        self.state.pointer_frame(PointerFrame {
            motion: Some(Motion::Relative { dx, dy }),
            ..Default::default()
        });
    }

    pub fn button(&mut self, code: u16, down: bool) {
        self.state.pointer_frame(PointerFrame { buttons: vec![(code, down)], ..Default::default() });
    }

    pub fn key(&mut self, code: u16, down: bool) {
        self.state.key(code, down);
    }

    pub fn pump(&mut self) -> Result<(), String> {
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|error| format!("dispatch: {error}"))?;
        if self.state.pending_present {
            self.state.compose();
            self.state.pending_present = false;
            self.state.presented(super::now_ms());
        }
        self.display
            .flush_clients()
            .map_err(|error| format!("flush: {error}"))
    }
}
