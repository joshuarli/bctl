use crate::shm::SurfacePixels;
use wayland_server::{backend::ObjectId, Resource};
use wayland_server::protocol::{wl_callback, wl_surface};

#[derive(Debug, Default)]
pub struct PendingState {
    pub buffer: Option<Option<ObjectId>>,
    pub callbacks: Vec<wl_callback::WlCallback>,
}

#[derive(Debug, Default)]
pub struct SurfaceUpdate {
    pub pixels: Option<Option<SurfacePixels>>,
    pub callbacks: Vec<wl_callback::WlCallback>,
}

impl SurfaceUpdate {
    pub fn merge(&mut self, mut newer: Self) {
        if newer.pixels.is_some() { self.pixels = newer.pixels; }
        self.callbacks.append(&mut newer.callbacks);
    }
}

#[derive(Debug)]
pub struct SurfaceData {
    pub resource: wl_surface::WlSurface,
    pub pending: PendingState,
    pub cached: Option<SurfaceUpdate>,
    pub cached_pos: Option<(i32, i32)>,
    pub cached_stacking: Option<Vec<ObjectId>>,
    pub pixels: Option<SurfacePixels>,
    pub callbacks: Vec<wl_callback::WlCallback>,
    pub parent: Option<ObjectId>,
    pub stacking: Vec<ObjectId>,
    pub pending_stacking: Option<Vec<ObjectId>>,
    pub subsurface_pos: (i32, i32),
    pub pending_pos: Option<(i32, i32)>,
    pub subsurface_sync: bool,
    pub has_role: bool,
    pub is_popup: bool,
}

impl SurfaceData {
    pub fn new(resource: wl_surface::WlSurface) -> Self {
        let id = resource.id();
        Self {
            resource, pending: PendingState::default(), cached: None, cached_pos: None,
            cached_stacking: None, pixels: None,
            callbacks: Vec::new(), parent: None, stacking: vec![id], pending_stacking: None,
            subsurface_pos: (0, 0), pending_pos: None, subsurface_sync: true,
            has_role: false, is_popup: false,
        }
    }

    pub fn apply(&mut self, mut update: SurfaceUpdate) {
        if let Some(pixels) = update.pixels { self.pixels = pixels; }
        self.callbacks.append(&mut update.callbacks);
    }
}
