use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel};
use wayland_server::backend::ObjectId;

#[derive(Debug)]
pub struct ToplevelData {
    pub resource: xdg_toplevel::XdgToplevel,
    pub xdg_surface: xdg_surface::XdgSurface,
    pub surface: ObjectId,
    pub last_configure: u32,
    pub acked_configure: u32,
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
            last_configure: 0,
            acked_configure: 0,
            mapped: false,
        }
    }
}

#[derive(Debug)]
pub struct XdgSurfaceData {
    pub surface: ObjectId,
    pub has_toplevel: bool,
    pub has_popup: bool,
    pub geometry: Option<(i32, i32, i32, i32)>,
}

impl XdgSurfaceData {
    pub fn new(surface: ObjectId) -> Self {
        XdgSurfaceData {
            surface,
            has_toplevel: false,
            has_popup: false,
            geometry: None,
        }
    }
}
