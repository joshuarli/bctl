use super::State;
use crate::positioner::Rect;
use wayland_server::backend::ObjectId;
use wayland_server::Resource;

const MAX_POPUP_DEPTH: usize = 64;

impl State {
    pub(super) fn popup_index(&self, surf_id: &ObjectId) -> Option<usize> {
        self.popups.iter().position(|popup| &popup.surface == surf_id)
    }

    fn is_toplevel_surface(&self, surf_id: &ObjectId) -> bool {
        self.toplevels.values().any(|top| &top.surface == surf_id)
    }

    pub(super) fn popup_root(&self, surf_id: &ObjectId) -> Option<ObjectId> {
        let mut cursor = surf_id.clone();
        for _ in 0..MAX_POPUP_DEPTH {
            if self.is_toplevel_surface(&cursor) {
                return Some(cursor);
            }
            cursor = self.popups.iter().find(|popup| popup.surface == cursor)?.parent.clone();
        }
        None
    }

    pub(super) fn geometry_origin(&self, surf_id: &ObjectId) -> Option<(i32, i32)> {
        let mut cursor = surf_id.clone();
        let mut origin = (0i32, 0i32);
        for _ in 0..MAX_POPUP_DEPTH {
            if self.is_toplevel_surface(&cursor) {
                return Some(origin);
            }
            let popup = self.popups.iter().find(|popup| popup.surface == cursor)?;
            origin = (origin.0.saturating_add(popup.geometry.x), origin.1.saturating_add(popup.geometry.y));
            cursor = popup.parent.clone();
        }
        None
    }

    fn window_offset(&self, surf_id: &ObjectId) -> (i32, i32) {
        self.xdg_surfaces
            .values()
            .find(|xdg| &xdg.surface == surf_id)
            .and_then(|xdg| xdg.geometry)
            .map_or((0, 0), |(x, y, _, _)| (x, y))
    }

    pub(super) fn surface_origin(&self, surf_id: &ObjectId) -> Option<(i32, i32)> {
        let (x, y) = self.geometry_origin(surf_id)?;
        let (gx, gy) = self.window_offset(surf_id);
        Some((x.saturating_sub(gx), y.saturating_sub(gy)))
    }

    pub(super) fn live_popups_of(&self, root: &ObjectId) -> Vec<ObjectId> {
        self.popups
            .iter()
            .filter(|popup| popup.live() && self.popup_root(&popup.surface).as_ref() == Some(root))
            .map(|popup| popup.surface.clone())
            .collect()
    }

    pub(super) fn configure_popup(&mut self, index: usize) {
        let parent_origin = self.geometry_origin(&self.popups[index].parent).unwrap_or((0, 0));
        let (width, height) = self.out_size;
        let bounds = Rect { x: -parent_origin.0, y: -parent_origin.1, width: width as i32, height: height as i32 };
        let serial = self.next_serial();
        let popup = &mut self.popups[index];
        popup.geometry = popup.placement.place(bounds);
        let Rect { x, y, width, height } = popup.geometry;
        popup.resource.configure(x, y, width, height);
        popup.xdg_surface.configure(serial);
        popup.configures.send(serial);
    }

    pub(super) fn update_popup_mapping(&mut self, surf_id: &ObjectId) {
        let Some(index) = self.popup_index(surf_id) else { return };
        let has_pixels = self.surfaces.get(surf_id).is_some_and(|surface| surface.pixels.is_some());
        let popup = &mut self.popups[index];
        if has_pixels && popup.configures.acked() && !popup.mapped && !popup.dismissed {
            popup.mapped = true;
            let Rect { x, y, width, height } = popup.geometry;
            eprintln!("TINYLAND_POPUP_MAPPED x={x} y={y} width={width} height={height}");
        } else if !has_pixels && popup.mapped {
            popup.mapped = false;
            popup.configures.reset();
        } else {
            return;
        }
        self.pending_present = true;
        self.refocus_pointer();
    }

    pub(super) fn dismiss_popups(&mut self, root: &ObjectId) {
        let doomed: Vec<usize> = (0..self.popups.len())
            .rev()
            .filter(|index| !self.popups[*index].dismissed && self.popup_root(&self.popups[*index].surface).as_ref() == Some(root))
            .collect();
        for index in doomed {
            let popup = &mut self.popups[index];
            popup.dismissed = true;
            popup.resource.popup_done();
            self.pending_present = true;
        }
    }

    pub(super) fn dismiss_popups_outside_pointer(&mut self) {
        let (x, y) = self.pointer.position;
        let hit = self.surface_at(x, y);
        if hit.as_ref().is_some_and(|surface| self.popup_index(surface).is_some()) {
            return;
        }
        if let Some(root) = self.active_surface() {
            self.dismiss_popups(&root);
        }
    }

    pub(super) fn remove_popups_where(&mut self, doomed: impl Fn(&crate::shell::PopupData) -> bool) {
        let before = self.popups.len();
        self.popups.retain(|popup| !doomed(popup));
        if self.popups.len() != before {
            self.dismiss_orphans();
            self.pending_present = true;
            self.refocus_pointer();
        }
    }

    pub(super) fn dismiss_orphans(&mut self) {
        let orphans: Vec<usize> = (0..self.popups.len())
            .rev()
            .filter(|index| !self.popups[*index].dismissed && self.popup_root(&self.popups[*index].surface).is_none())
            .collect();
        for index in orphans {
            let popup = &mut self.popups[index];
            popup.dismissed = true;
            if popup.resource.is_alive() {
                popup.resource.popup_done();
            }
            self.pending_present = true;
        }
    }
}
