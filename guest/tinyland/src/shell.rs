use crate::positioner::{Complete, Rect};
use wayland_protocols::xdg::shell::server::{xdg_popup, xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_server::backend::ObjectId;

pub const MAX_UNACKED_CONFIGURES: usize = 32;

#[derive(Debug, Default)]
pub struct Configures {
    sent: Vec<u32>,
    acked: Option<u32>,
}

impl Configures {
    pub fn send(&mut self, serial: u32) {
        if self.sent.len() == MAX_UNACKED_CONFIGURES {
            self.sent.remove(0);
        }
        self.sent.push(serial);
    }

    pub fn ack(&mut self, serial: u32) -> bool {
        match self.sent.iter().position(|sent| *sent == serial) {
            Some(index) => {
                self.sent.drain(..=index);
                self.acked = Some(serial);
                true
            }
            None => false,
        }
    }

    pub fn acked(&self) -> bool {
        self.acked.is_some()
    }

    pub fn started(&self) -> bool {
        self.acked.is_some() || !self.sent.is_empty()
    }

    pub fn reset(&mut self) {
        *self = Configures::default();
    }
}

#[derive(Debug)]
pub struct ToplevelData {
    pub resource: xdg_toplevel::XdgToplevel,
    pub xdg_surface: xdg_surface::XdgSurface,
    pub surface: ObjectId,
    pub configures: Configures,
    pub activated: bool,
    pub mapped: bool,
}

impl ToplevelData {
    pub fn new(
        resource: xdg_toplevel::XdgToplevel,
        xdg_surface: xdg_surface::XdgSurface,
        surface: ObjectId,
    ) -> Self {
        ToplevelData {
            resource,
            xdg_surface,
            surface,
            configures: Configures::default(),
            activated: false,
            mapped: false,
        }
    }
}

#[derive(Debug)]
pub struct PopupData {
    pub resource: xdg_popup::XdgPopup,
    pub xdg_surface: xdg_surface::XdgSurface,
    pub surface: ObjectId,
    pub parent: ObjectId,
    pub placement: Complete,
    pub geometry: Rect,
    pub configures: Configures,
    pub mapped: bool,
    pub dismissed: bool,
}

impl PopupData {
    pub fn live(&self) -> bool {
        self.mapped && !self.dismissed
    }
}

#[derive(Debug)]
pub struct XdgSurfaceData {
    pub wm_base: xdg_wm_base::XdgWmBase,
    pub surface: ObjectId,
    pub has_toplevel: bool,
    pub has_popup: bool,
    pub geometry: Option<(i32, i32, i32, i32)>,
}

impl XdgSurfaceData {
    pub fn new(wm_base: xdg_wm_base::XdgWmBase, surface: ObjectId) -> Self {
        XdgSurfaceData {
            wm_base,
            surface,
            has_toplevel: false,
            has_popup: false,
            geometry: None,
        }
    }
}
