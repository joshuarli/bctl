use super::State;
use crate::input::SeatData;
use crate::positioner::{Positioner, Rect};
use crate::shell::{Configures, PopupData, ToplevelData, XdgSurfaceData};
use crate::shm::{self, ShmBufferData, ShmFormat};
use crate::surface::SurfaceData;
use std::os::unix::io::OwnedFd;
use wayland_protocols::xdg::shell::server::{xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_server::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_data_device_manager, wl_data_device, wl_data_source, wl_data_offer, wl_keyboard, wl_output, wl_pointer, wl_region, wl_seat,
    wl_shm, wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use wayland_server::backend::ObjectId;

impl GlobalDispatch<wl_compositor::WlCompositor, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_compositor::WlCompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &wl_compositor::WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                use crate::server::MAX_SURFACES_PER_CLIENT;
                if state.count_client_surfaces(&client.id()) >= MAX_SURFACES_PER_CLIENT {
                    _resource.post_error(0u32, "too many surfaces for client");
                    return;
                }
                let surf = data_init.init(id, ());
                state.surfaces.insert(surf.id(), SurfaceData::new(surf));
            }
            wl_compositor::Request::CreateRegion { id } => {
                use crate::server::MAX_REGIONS_PER_CLIENT;
                let regions_for_client = state
                    .regions
                    .keys()
                    .filter(|rid| {
                        state
                            .dh
                            .get_client((*rid).clone())
                            .map(|c| c.id() == client.id())
                            .unwrap_or(false)
                    })
                    .count();
                if regions_for_client >= MAX_REGIONS_PER_CLIENT {
                    _resource.post_error(0u32, "too many regions for client");
                    return;
                }
                let region = data_init.init(id, ());
                state.regions.insert(region.id(), Vec::new());
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_compositor::WlCompositor,
        _data: &(),
    ) {
    }
}

impl Dispatch<wl_region::WlRegion, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wl_region::WlRegion,
        request: wl_region::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let entry = state.regions.entry(resource.id()).or_default();
        match request {
            wl_region::Request::Add { x, y, width, height } => {
                if width > 0 && height > 0 {
                    use crate::server::MAX_REGION_RECTS;
                    if entry.len() >= MAX_REGION_RECTS {
                        resource.post_error(0u32, "too many region rectangles");
                        return;
                    }
                    entry.push((x, y, width, height));
                }
            }
            wl_region::Request::Subtract { .. } => {}
            wl_region::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_region::WlRegion,
        _data: &(),
    ) {
        state.regions.remove(&resource.id());
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wl_surface::WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let id = resource.id();
        match request {
            wl_surface::Request::Attach { buffer, x, y } => {
                if let Some(s) = state.surfaces.get_mut(&id) {
                    s.pending.buffer = Some(buffer.map(|b| b.id()));
                    let _ = (x, y);
                }
            }
            wl_surface::Request::Damage { .. } | wl_surface::Request::DamageBuffer { .. } => {}
            wl_surface::Request::Frame { callback: cb_id } => {
                use crate::server::MAX_CALLBACKS_PER_SURFACE;
                let pending = state.surfaces.get(&id).map(|s| s.pending.callbacks.len()).unwrap_or(0);
                let current = state.surfaces.get(&id).map(|s| s.callbacks.len()).unwrap_or(0);
                let cached = state
                    .surfaces
                    .get(&id)
                    .and_then(|s| s.cached.as_ref())
                    .map(|c| c.callbacks.len())
                    .unwrap_or(0);
                if pending + current + cached >= MAX_CALLBACKS_PER_SURFACE {
                    resource.post_error(0u32, "too many outstanding frame callbacks");
                    return;
                }
                let cb = data_init.init(cb_id, ());
                if let Some(s) = state.surfaces.get_mut(&id) {
                    s.pending.callbacks.push(cb);
                }
            }
            wl_surface::Request::SetOpaqueRegion { .. } => {}
            wl_surface::Request::SetInputRegion { .. } => {}
            wl_surface::Request::Commit => {
                state.apply_commit(&id);
            }
            wl_surface::Request::SetBufferTransform { transform } => {
                match transform {
                    wayland_server::WEnum::Value(wl_output::Transform::Normal) => {}
                    wayland_server::WEnum::Value(_) => {
                        eprintln!("TINYLAND_TRANSFORM_IGNORED only normal transform rendered");
                    }
                    wayland_server::WEnum::Unknown(_) => {
                        resource.post_error(
                            wl_surface::Error::InvalidTransform as u32,
                            "unknown buffer transform",
                        );
                    }
                }
            }
            wl_surface::Request::SetBufferScale { scale } => {
                if scale <= 0 {
                    resource.post_error(
                        wl_surface::Error::InvalidScale as u32,
                        "buffer scale must be positive",
                    );
                    return;
                }
                if scale != 1 {
                    eprintln!("TINYLAND_SCALE_IGNORED only scale 1 rendered");
                }
            }
            wl_surface::Request::Offset { x, y } => {
                let _ = (x, y);
            }
            wl_surface::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_surface::WlSurface,
        _data: &(),
    ) {
        let id = resource.id();
        if let Some(s) = state.surfaces.remove(&id) {
            let client = s.resource.client().map(|client| client.id());
            let current = s.pixels.as_ref().map(|pixels| pixels.bytes.len()).unwrap_or(0);
            let cached = s.cached.as_ref().and_then(|cached| cached.pixels.as_ref()).and_then(|pixels| pixels.as_ref()).map(|pixels| pixels.bytes.len()).unwrap_or(0);
            if let Some(client) = client {
                if current > 0 {
                    state.release_bytes(client.clone(), current);
                }
                if cached > 0 {
                    state.release_bytes(client, cached);
                }
            }
            if let Some(parent) = s.parent {
                if let Some(parent) = state.surfaces.get_mut(&parent) { parent.stacking.retain(|child| child != &id);
                    if let Some(order) = &mut parent.pending_stacking { order.retain(|child| child != &id); }
                    if let Some(order) = &mut parent.cached_stacking { order.retain(|child| child != &id); } }
            }
            for seat in &mut state.seats {
                if seat.keyboard_focus.as_ref() == Some(&id) { seat.keyboard_focus = None; }
            }
            if state.pointer.focus.as_ref() == Some(&id) { state.pointer.focus = None; }
            if state.pointer.cursor.as_ref().is_some_and(|cursor| cursor.surface.as_ref() == Some(&id)) { state.pointer.cursor = None; }
            for child in s.stacking {
                if let Some(c) = state.surfaces.get_mut(&child) {
                    c.parent = None;
                }
            }
        }
        let removed_toplevels: Vec<ObjectId> = state
            .toplevels
            .iter()
            .filter(|(_, t)| t.surface == id)
            .map(|(top_id, _)| top_id.clone())
            .collect();
        state.toplevels.retain(|_, t| t.surface != id);
        for top_id in removed_toplevels {
            state.toplevel_order.retain(|order| order != &top_id);
        }
        if state.active_surface().as_ref() == Some(&id) {
            state.deactivate_surface(&id);
        }
        state.remove_popups_where(|popup| popup.surface == id);
        state.dismiss_orphans();
        state.refocus_pointer();
        state.pending_present = true;
    }
}

impl GlobalDispatch<wl_subcompositor::WlSubcompositor, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_subcompositor::WlSubcompositor>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<wl_subcompositor::WlSubcompositor, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &wl_subcompositor::WlSubcompositor,
        request: wl_subcompositor::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_subcompositor::Request::GetSubsurface { id, surface, parent } => {
                let surf_id = surface.id();
                let parent_id = parent.id();
                if surf_id == parent_id {
                    _resource.post_error(
                        wl_subcompositor::Error::BadSurface as u32,
                        "subsurface cannot be its own parent",
                    );
                    return;
                }
                let sub = data_init.init(id, ());
                let sub_id = sub.id();
                let mut ancestor = Some(parent_id.clone());
                let mut depth = 0;
                while let Some(id) = ancestor {
                    depth += 1;
                    if depth >= 64 {
                        _resource.post_error(wl_subcompositor::Error::BadSurface as u32, "subsurface nesting exceeds 64 levels");
                        return;
                    }
                    if id == surf_id {
                        _resource.post_error(wl_subcompositor::Error::BadSurface as u32, "subsurface parent cycle");
                        return;
                    }
                    ancestor = state.surfaces.get(&id).and_then(|surface| surface.parent.clone());
                }
                let mut subtree = vec![(surf_id.clone(), depth + 1)];
                while let Some((id, depth)) = subtree.pop() {
                    if depth > 64 {
                        _resource.post_error(wl_subcompositor::Error::BadSurface as u32, "subsurface nesting exceeds 64 levels");
                        return;
                    }
                    if let Some(surface) = state.surfaces.get(&id) {
                        subtree.extend(surface.stacking.iter().filter(|child| **child != id).map(|child| (child.clone(), depth + 1)));
                    }
                }
                let valid = match (state.surfaces.get(&surf_id), state.surfaces.get(&parent_id)) {
                    (Some(s), Some(p)) => !s.has_role && !p.is_popup,
                    _ => false,
                };
                if !valid {
                    _resource.post_error(
                        wl_subcompositor::Error::BadSurface as u32,
                        "bad parent or surface already has role",
                    );
                    return;
                }
                if let Some(s) = state.surfaces.get_mut(&surf_id) {
                    s.has_role = true;
                    s.parent = Some(parent_id.clone());
                    s.subsurface_sync = true;
                }
                if let Some(p) = state.surfaces.get_mut(&parent_id) {
                    p.stacking.push(surf_id.clone());
                    if let Some(order) = &mut p.pending_stacking { order.push(surf_id.clone()); }
                }
                state.subsurfaces.insert(sub_id, surf_id);
            }
            wl_subcompositor::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_subcompositor::WlSubcompositor,
        _data: &(),
    ) {
    }
}

fn restack(state: &mut State, resource: &wl_subsurface::WlSubsurface,
    id: &wayland_server::backend::ObjectId, sibling: &wayland_server::backend::ObjectId, above: bool) {
    let parent = state.surfaces.get(id).and_then(|surface| surface.parent.clone());
    let valid = parent.as_ref().and_then(|parent| state.surfaces.get(parent))
        .is_some_and(|parent| id != sibling && parent.stacking.contains(sibling));
    if !valid { resource.post_error(wl_subsurface::Error::BadSurface as u32, "restack target is not a sibling or parent"); return; }
    let parent = state.surfaces.get_mut(&parent.unwrap()).unwrap();
    let order = parent.pending_stacking.get_or_insert_with(|| parent.stacking.clone());
    order.retain(|child| child != id);
    let position = order.iter().position(|child| child == sibling).unwrap() + usize::from(above);
    order.insert(position, id.clone());
}

impl Dispatch<wl_subsurface::WlSubsurface, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wl_subsurface::WlSubsurface,
        request: wl_subsurface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let surf_id = match state.subsurfaces.get(&resource.id()).cloned() {
            Some(id) => id,
            None => return,
        };
        match request {
            wl_subsurface::Request::SetPosition { x, y } => {
                if let Some(s) = state.surfaces.get_mut(&surf_id) {
                    s.pending_pos = Some((x, y));
                }
            }
            wl_subsurface::Request::SetSync => {
                if let Some(s) = state.surfaces.get_mut(&surf_id) {
                    s.subsurface_sync = true;
                }
            }
            wl_subsurface::Request::SetDesync => state.desynchronize(&surf_id),
            wl_subsurface::Request::PlaceAbove { sibling } => restack(state, resource, &surf_id, &sibling.id(), true),
            wl_subsurface::Request::PlaceBelow { sibling } => restack(state, resource, &surf_id, &sibling.id(), false),
            wl_subsurface::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_subsurface::WlSubsurface,
        _data: &(),
    ) {
        if let Some(surf_id) = state.subsurfaces.remove(&resource.id()) {
            let (parent, cached_bytes, client) = match state.surfaces.get_mut(&surf_id) {
                Some(surface) => {
                    let cached_bytes = surface
                        .cached
                        .as_ref()
                        .and_then(|cached| cached.pixels.as_ref())
                        .and_then(|pixels| pixels.as_ref())
                        .map(|pixels| pixels.bytes.len())
                        .unwrap_or(0);
                    let client = surface.resource.client().map(|client| client.id());
                    surface.cached = None;
                    surface.cached_pos = None;
                    surface.cached_stacking = None;
                    surface.pending_pos = None;
                    (surface.parent.take(), cached_bytes, client)
                }
                None => (None, 0, None),
            };
            if cached_bytes > 0 {
                if let Some(client) = client {
                    state.release_bytes(client, cached_bytes);
                }
            }
            if let Some(parent) = parent {
                if let Some(parent) = state.surfaces.get_mut(&parent) { parent.stacking.retain(|child| child != &surf_id);
                    if let Some(order) = &mut parent.pending_stacking { order.retain(|child| child != &surf_id); }
                    if let Some(order) = &mut parent.cached_stacking { order.retain(|child| child != &surf_id); } }
            }
            state.pending_present = true;
        }
    }
}

impl GlobalDispatch<wl_shm::WlShm, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<wl_shm::WlShm>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shm = data_init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
        shm.format(wl_shm::Format::Abgr8888);
        shm.format(wl_shm::Format::Xbgr8888);
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &wl_shm::WlShm,
        request: wl_shm::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm::Request::CreatePool { id, fd, size } => {
                use crate::server::MAX_POOLS_PER_CLIENT;
                if state.count_client_pools(&client.id()) >= MAX_POOLS_PER_CLIENT {
                    _resource.post_error(0u32, "too many shm pools for client");
                    return;
                }
                let pool_size = match shm::validate_pool_size(size) {
                    Ok(n) => n,
                    Err(msg) => {
                        _resource.post_error(wl_shm::Error::InvalidFd as u32, msg.to_string());
                        return;
                    }
                };
                let file = std::fs::File::from(fd as OwnedFd);
                let pool_data = match shm::ShmPoolData::new(file, pool_size as i32) {
                    Ok(pool) => pool,
                    Err(error) => { _resource.post_error(wl_shm::Error::InvalidFd as u32, error); return; }
                };
                let pool = data_init.init(id, ());
                state.pools.insert(pool.id(), pool_data);
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_shm::WlShm,
        _data: &(),
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wl_shm_pool::WlShmPool,
        request: wl_shm_pool::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, format } => {
                use crate::server::MAX_BUFFERS_PER_CLIENT;
                if state.count_client_buffers(&client.id()) >= MAX_BUFFERS_PER_CLIENT {
                    resource.post_error(0u32, "too many buffers for client");
                    return;
                }
                let pool_id = resource.id();
                let pool_size = match state.pools.get(&pool_id) {
                    Some(p) => p.size,
                    None => {
                        resource.post_error(0u32, "unknown pool");
                        return;
                    }
                };
                let fmt = match format {
                    wayland_server::WEnum::Value(wl_shm::Format::Argb8888) => ShmFormat::Argb8888,
                    wayland_server::WEnum::Value(wl_shm::Format::Xrgb8888) => ShmFormat::Xrgb8888,
                    wayland_server::WEnum::Value(wl_shm::Format::Abgr8888) => ShmFormat::Abgr8888,
                    wayland_server::WEnum::Value(wl_shm::Format::Xbgr8888) => ShmFormat::Xbgr8888,
                    other => {
                        resource.post_error(
                            wl_shm_pool::Error::InvalidFormat as u32,
                            format!("only ARGB8888, XRGB8888, ABGR8888 and XBGR8888 are supported, got {other:?}"),
                        );
                        return;
                    }
                };
                if let Err(msg) = shm::validate_buffer(pool_size, offset, width, height, stride) {
                    let code = if msg.contains("format") {
                        wl_shm_pool::Error::InvalidFormat as u32
                    } else {
                        wl_shm_pool::Error::InvalidStride as u32
                    };
                    resource.post_error(code, msg.to_string());
                    return;
                }
                let buf = data_init.init(id, ());
                let buf_id = buf.id();
                state.buffers.insert(
                    buf_id,
                    (
                        buf,
                        ShmBufferData { file: state.pools[&pool_id].file.clone(), offset, width, height, stride, format: fmt },
                    ),
                );
            }
            wl_shm_pool::Request::Resize { size } => {
                if let Some(pool) = state.pools.get_mut(&resource.id()) {
                    if let Err(error) = pool.resize(size) {
                        resource.post_error(wl_shm::Error::InvalidFd as u32, error);
                    }
                }
            }
            wl_shm_pool::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_shm_pool::WlShmPool,
        _data: &(),
    ) {
        state.pools.remove(&resource.id());
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_buffer::WlBuffer,
        request: wl_buffer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_buffer::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_buffer::WlBuffer,
        _data: &(),
    ) {
        state.buffers.remove(&resource.id());

    }
}

impl GlobalDispatch<wl_output::WlOutput, ()> for State {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        client: &Client,
        resource: New<wl_output::WlOutput>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        use crate::server::MAX_OUTPUTS_PER_CLIENT;
        let existing = state.outputs.iter().filter(|(id, _)| id == &client.id()).count();
        if existing >= MAX_OUTPUTS_PER_CLIENT {
            let out = data_init.init(resource, ());
            out.post_error(0u32, "too many outputs for client");
            return;
        }
        let out = data_init.init(resource, ());
        out.geometry(
            0,
            0,
            0,
            0,
            wl_output::Subpixel::Unknown,
            "tinyland".to_string(),
            "virtio-gpu".to_string(),
            wl_output::Transform::Normal,
        );
        let (w, h) = state.out_size;
        out.mode(wl_output::Mode::Current | wl_output::Mode::Preferred, w as i32, h as i32, 60000);
        if out.version() >= 2 { out.scale(1); out.done(); }
        state.outputs.push((client.id(), out));
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_output::WlOutput,
        request: wl_output::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_output::Request::Release => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_output::WlOutput,
        _data: &(),
    ) {
        let id = resource.id();
        state.outputs.retain(|(_, o)| o.id() != id);
    }
}

impl GlobalDispatch<wl_seat::WlSeat, ()> for State {
    fn bind(
        state: &mut Self,
        _dh: &DisplayHandle,
        client: &Client,
        resource: New<wl_seat::WlSeat>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        use crate::server::MAX_SEATS_PER_CLIENT;
        let existing = state
            .seats
            .iter()
            .filter(|seat| seat.resource.client().map(|c| c.id() == client.id()).unwrap_or(false))
            .count();
        if existing >= MAX_SEATS_PER_CLIENT {
            let seat = data_init.init(resource, ());
            seat.post_error(0u32, "too many seats for client");
            return;
        }
        let seat = data_init.init(resource, ());
        let capabilities = if state.has_pointer {
            wl_seat::Capability::Keyboard | wl_seat::Capability::Pointer
        } else {
            wl_seat::Capability::Keyboard
        };
        seat.capabilities(capabilities);
        if seat.version() >= 2 { seat.name("seat0".to_string()); }
        state.seats.push(SeatData::new(seat));
        let _ = client;
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &wl_seat::WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetPointer { id } => {
                if !state.has_pointer {
                    resource.post_error(wl_seat::Error::MissingCapability as u32, "seat has no pointer");
                    return;
                }
                let pointer = data_init.init(id, ());
                let Some(index) = state.seats.iter().position(|seat| seat.resource == *resource) else { return };
                state.seats[index].pointers.push(pointer.clone());
                state.enter_pointer(&pointer);
            }
            wl_seat::Request::GetTouch { .. } => {
                resource.post_error(wl_seat::Error::MissingCapability as u32, "seat has no touch");
            }
            wl_seat::Request::GetKeyboard { id } => {
                let kb = data_init.init(id, ());
                use std::os::fd::AsFd;
                kb.keymap(wl_keyboard::KeymapFormat::XkbV1, state.keyboard.keymap.as_fd(), state.keyboard.keymap_size);
                if kb.version() >= 4 { kb.repeat_info(25, 600); }
                let seat_index = state.seats.iter().position(|seat| seat.resource == *resource);
                if let Some(index) = seat_index {
                    state.seats[index].keyboards.push(kb);
                    if let Some(focused) = state.active_surface() {
                        let focused_client = state
                            .surfaces
                            .get(&focused)
                            .and_then(|surface| surface.resource.client());
                        let seat_client = state.seats[index].resource.client();
                        if seat_client == focused_client {
                            let serial = state.next_serial();
                            state.seats[index].keyboard_focus = Some(focused.clone());
                            if let Some(surface) = state.surfaces.get(&focused) {
                                let [depressed, latched, locked, group] =
                                    state.keyboard.modifiers();
                                for keyboard in &state.seats[index].keyboards {
                                    keyboard.enter(serial, &surface.resource, Vec::new());
                                    keyboard.modifiers(serial, depressed, latched, locked, group);
                                }
                            }
                        }
                    }
                }
            }
            wl_seat::Request::Release => {}
            _ => {}
        }
        let _ = client;
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_seat::WlSeat,
        _data: &(),
    ) {
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &wl_pointer::WlPointer,
        request: wl_pointer::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_pointer::Request::SetCursor { serial, surface, hotspot_x, hotspot_y } = request {
            state.set_cursor(resource, serial, surface.map(|surface| surface.id()), (hotspot_x, hotspot_y));
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_pointer::WlPointer,
        _data: &(),
    ) {
        let id = resource.id();
        for seat in &mut state.seats {
            seat.pointers.retain(|pointer| pointer.id() != id);
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_keyboard::WlKeyboard,
        request: wl_keyboard::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_keyboard::Request::Release => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &wl_keyboard::WlKeyboard,
        _data: &(),
    ) {
        let id = resource.id();
        for seat in &mut state.seats {
            seat.keyboards.retain(|k| k.id() != id);
        }
    }
}

impl State {
    pub(super) fn send_selection(&mut self) {
        let focused_client = self
            .active_surface()
            .and_then(|surface| self.surfaces.get(&surface))
            .and_then(|surface| surface.resource.client());
        let devices: Vec<_> = self.selection.devices.values().cloned().collect();
        for device in devices {
            let Some(client) = device.client() else { continue; };
            if Some(client.clone()) != focused_client {
                continue;
            }
            let source = self.selection.selection.as_ref().and_then(|id| self.selection.sources.get(id));
            if let Some(source) = source {
                let offer = match client.create_resource::<wl_data_offer::WlDataOffer, (), State>(&self.dh, device.version(), ()) {
                    Ok(offer) => offer,
                    Err(error) => { eprintln!("clipboard offer creation: {error}"); continue; }
                };
                device.data_offer(&offer);
                for mime in &source.mime_types { offer.offer(mime.clone()); }
                device.selection(Some(&offer));
                self.selection.offers.insert(offer.id(), source.resource.clone());
            } else {
                device.selection(None);
            }
        }
    }
}

impl GlobalDispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn bind(_state: &mut Self, _dh: &DisplayHandle, _client: &Client,
        resource: New<wl_data_device_manager::WlDataDeviceManager>, _data: &(), data_init: &mut DataInit<'_, Self>) {
        data_init.init(resource, ());
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for State {
    fn request(state: &mut Self, client: &Client, _resource: &wl_data_device_manager::WlDataDeviceManager,
        request: wl_data_device_manager::Request, _data: &(), _dh: &DisplayHandle, data_init: &mut DataInit<'_, Self>) {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                use crate::server::MAX_SOURCES_PER_CLIENT;
                if state.count_client_sources(client) >= MAX_SOURCES_PER_CLIENT {
                    let source = data_init.init(id, ());
                    source.post_error(0u32, "too many data sources for client");
                    return;
                }
                let source = data_init.init(id, ());
                state.selection.sources.insert(source.id(), crate::selection::DataSourceData { resource: source, mime_types: Vec::new() });
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                use crate::server::MAX_DEVICES_PER_CLIENT;
                if state.count_client_devices(client) >= MAX_DEVICES_PER_CLIENT {
                    let device = data_init.init(id, ());
                    device.post_error(0u32, "too many data devices for client");
                    return;
                }
                let device = data_init.init(id, ());
                state.selection.devices.insert(device.id(), device);
                state.send_selection();
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for State {
    fn request(state: &mut Self, _client: &Client, resource: &wl_data_source::WlDataSource,
        request: wl_data_source::Request, _data: &(), _dh: &DisplayHandle, _data_init: &mut DataInit<'_, Self>) {
        if let wl_data_source::Request::Offer { mime_type } = request {
            if let Some(source) = state.selection.sources.get_mut(&resource.id()) {
                use crate::server::MAX_MIME_TYPES_PER_SOURCE;
                if source.mime_types.len() >= MAX_MIME_TYPES_PER_SOURCE {
                    resource.post_error(0u32, "too many mime types for source");
                    return;
                }
                source.mime_types.push(mime_type);
            }
        }
    }

    fn destroyed(state: &mut Self, _client: wayland_server::backend::ClientId,
        resource: &wl_data_source::WlDataSource, _data: &()) {
        state.selection.sources.remove(&resource.id());
        if state.selection.selection.as_ref() == Some(&resource.id()) {
            state.selection.selection = None;
            state.send_selection();
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for State {
    fn request(state: &mut Self, client: &Client, resource: &wl_data_device::WlDataDevice,
        request: wl_data_device::Request, _data: &(), _dh: &DisplayHandle, _data_init: &mut DataInit<'_, Self>) {
        match request {
            wl_data_device::Request::SetSelection { source, serial } => {
                let focused_matches = state
                    .active_surface()
                    .and_then(|surface| state.surfaces.get(&surface))
                    .and_then(|surface| surface.resource.client())
                    .as_ref()
                    == Some(client);
                if !focused_matches || serial > state.serial { return; }
                let next = source.map(|source| source.id());
                if next == state.selection.selection { return; }
                if let Some(old) = state.selection.selection.take().and_then(|id| state.selection.sources.get(&id)) { old.resource.cancelled(); }
                state.selection.selection = next;
                state.send_selection();
            }
            wl_data_device::Request::StartDrag { source, .. } => {
                if let Some(source) = source { source.cancelled(); }
                resource.leave();
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: wayland_server::backend::ClientId,
        resource: &wl_data_device::WlDataDevice, _data: &()) { state.selection.devices.remove(&resource.id()); }
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for State {
    fn request(state: &mut Self, _client: &Client, resource: &wl_data_offer::WlDataOffer,
        request: wl_data_offer::Request, _data: &(), _dh: &DisplayHandle, _data_init: &mut DataInit<'_, Self>) {
        let Some(source) = state.selection.offers.get(&resource.id()).filter(|source| source.is_alive()) else { return; };
        match request {
            wl_data_offer::Request::Accept { mime_type, .. } => source.target(mime_type),
            wl_data_offer::Request::Receive { mime_type, fd } => {
                use std::os::fd::AsFd;
                source.send(mime_type, fd.as_fd());
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: wayland_server::backend::ClientId,
        resource: &wl_data_offer::WlDataOffer, _data: &()) { state.selection.offers.remove(&resource.id()); }
}

impl GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<xdg_wm_base::XdgWmBase>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &xdg_wm_base::XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                let positioner = data_init.init(id, ());
                state.positioners.insert(positioner.id(), Positioner::default());
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                route_xdg_wm_base_get_surface(state, resource, id, surface, data_init);
            }
            xdg_wm_base::Request::Pong { .. } => {}
            xdg_wm_base::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &xdg_wm_base::XdgWmBase,
        _data: &(),
    ) {
    }
}

pub fn handle_get_xdg_surface(
    state: &mut State,
    wm_base: &xdg_wm_base::XdgWmBase,
    id: New<xdg_surface::XdgSurface>,
    surface: wl_surface::WlSurface,
    data_init: &mut DataInit<'_, State>,
) {
    let surf_id = surface.id();
    let has_role = state.surfaces.get(&surf_id).map(|s| s.has_role).unwrap_or(true);
    if has_role {
        return;
    }
    let xsurf = data_init.init(id, ());
    let xsurf_id = xsurf.id();
    state.xdg_surfaces.insert(xsurf_id, XdgSurfaceData::new(wm_base.clone(), surf_id.clone()));
    if let Some(s) = state.surfaces.get_mut(&surf_id) {
        s.has_role = true;
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &xdg_surface::XdgSurface,
        request: xdg_surface::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                use crate::server::MAX_TOPLEVELS_PER_CLIENT;
                if state.count_client_toplevels(&client.id()) >= MAX_TOPLEVELS_PER_CLIENT {
                    resource.post_error(0u32, "too many toplevels for client");
                    return;
                }
                let xsurf_id = resource.id();
                let surf_id = match state.xdg_surfaces.get(&xsurf_id) {
                    Some(x) => {
                        if x.has_toplevel || x.has_popup {
                            resource.post_error(
                                xdg_surface::Error::AlreadyConstructed as u32,
                                "role already assigned".to_string(),
                            );
                            return;
                        }
                        x.surface.clone()
                    }
                    None => return,
                };
                let top = data_init.init(id, ());
                let top_id = top.id();
                state.toplevels.insert(
                    top_id.clone(),
                    ToplevelData::new(top.clone(), resource.clone(), surf_id.clone()),
                );
                if let Some(x) = state.xdg_surfaces.get_mut(&xsurf_id) {
                    x.has_toplevel = true;
                }

            }
            xdg_surface::Request::GetPopup { id, parent, positioner } => {
                let xsurf_id = resource.id();
                let (surf_id, wm_base) = match state.xdg_surfaces.get(&xsurf_id) {
                    Some(x) => {
                        if x.has_toplevel || x.has_popup {
                            resource.post_error(
                                xdg_surface::Error::AlreadyConstructed as u32,
                                "role already assigned".to_string(),
                            );
                            return;
                        }
                        (x.surface.clone(), x.wm_base.clone())
                    }
                    None => return,
                };
                let Some(placement) = state.positioners.get(&positioner.id()).and_then(Positioner::complete) else {
                    wm_base.post_error(xdg_wm_base::Error::InvalidPositioner as u32, "positioner needs a size and an anchor rectangle");
                    return;
                };
                let parent_surface = parent
                    .and_then(|parent| state.xdg_surfaces.get(&parent.id()).filter(|x| x.has_toplevel || x.has_popup).map(|x| x.surface.clone()));
                let Some(parent_surface) = parent_surface else {
                    wm_base.post_error(xdg_wm_base::Error::InvalidPopupParent as u32, "popup parent must be a toplevel or popup");
                    return;
                };
                let popup = data_init.init(id, ());
                if let Some(x) = state.xdg_surfaces.get_mut(&xsurf_id) {
                    x.has_popup = true;
                }
                if let Some(s) = state.surfaces.get_mut(&surf_id) {
                    s.is_popup = true;
                }
                state.popups.push(PopupData {
                    resource: popup,
                    xdg_surface: resource.clone(),
                    surface: surf_id,
                    parent: parent_surface,
                    placement,
                    geometry: Rect { x: 0, y: 0, width: 0, height: 0 },
                    configures: Configures::default(),
                    mapped: false,
                    dismissed: false,
                });
            }
            xdg_surface::Request::SetWindowGeometry { x: gx, y: gy, width, height } => {
                let xsurf_id = resource.id();
                if width <= 0 || height <= 0 {
                    resource.post_error(
                        xdg_surface::Error::InvalidSize as u32,
                        "non-positive geometry".to_string(),
                    );
                    return;
                }
                if let Some(x) = state.xdg_surfaces.get_mut(&xsurf_id) {
                    x.geometry = Some((gx, gy, width, height));
                }
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let xsurf_id = resource.id();
                let surf_id = match state.xdg_surfaces.get(&xsurf_id) {
                    Some(x) => x.surface.clone(),
                    None => return,
                };
                let configures = match state.toplevels.values_mut().find(|t| t.surface == surf_id) {
                    Some(top) => Some(&mut top.configures),
                    None => state.popups.iter_mut().find(|p| p.surface == surf_id).map(|p| &mut p.configures),
                };
                let Some(configures) = configures else {
                    resource.post_error(xdg_surface::Error::NotConstructed as u32, "xdg_surface has no role".to_string());
                    return;
                };
                if !configures.ack(serial) {
                    resource.post_error(
                        xdg_surface::Error::InvalidSerial as u32,
                        "unknown configure serial".to_string(),
                    );
                    return;
                }
                state.try_map(&surf_id);
            }
            xdg_surface::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_surface::XdgSurface,
        _data: &(),
    ) {
        let id = resource.id();
        if let Some(x) = state.xdg_surfaces.remove(&id) {
            let removed: Vec<ObjectId> = state
                .toplevels
                .iter()
                .filter(|(_, t)| t.surface == x.surface)
                .map(|(top_id, _)| top_id.clone())
                .collect();
            state.toplevels.retain(|_, t| t.surface != x.surface);
            for top_id in removed {
                state.toplevel_order.retain(|order| order != &top_id);
            }
            state.clear_focus(&x.surface);
            state.remove_popups_where(|popup| popup.surface == x.surface);
            state.dismiss_orphans();
            state.pending_present = true;
        }
    }
}

impl Dispatch<xdg_positioner::XdgPositioner, ()> for State {
    fn request(
        state: &mut Self, _client: &Client, resource: &xdg_positioner::XdgPositioner,
        request: xdg_positioner::Request, _data: &(), _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let invalid = |message: &str| resource.post_error(xdg_positioner::Error::InvalidInput as u32, message.to_string());
        let Some(positioner) = state.positioners.get_mut(&resource.id()) else { return };
        match request {
            xdg_positioner::Request::SetSize { width, height } => {
                if width <= 0 || height <= 0 {
                    invalid("positioner size must be positive");
                    return;
                }
                positioner.size = Some((width, height));
            }
            xdg_positioner::Request::SetAnchorRect { x, y, width, height } => {
                if width < 0 || height < 0 {
                    invalid("anchor rectangle must not be negative");
                    return;
                }
                positioner.anchor_rect = Some(Rect { x, y, width, height });
            }
            xdg_positioner::Request::SetAnchor { anchor } => match anchor {
                wayland_server::WEnum::Value(anchor) => positioner.anchor = anchor,
                wayland_server::WEnum::Unknown(_) => invalid("unknown anchor"),
            },
            xdg_positioner::Request::SetGravity { gravity } => match gravity {
                wayland_server::WEnum::Value(gravity) => positioner.gravity = gravity,
                wayland_server::WEnum::Unknown(_) => invalid("unknown gravity"),
            },
            xdg_positioner::Request::SetConstraintAdjustment { constraint_adjustment } => {
                positioner.constraint_adjustment = match constraint_adjustment {
                    wayland_server::WEnum::Value(adjustment) => adjustment,
                    wayland_server::WEnum::Unknown(bits) => xdg_positioner::ConstraintAdjustment::from_bits_truncate(bits),
                };
            }
            xdg_positioner::Request::SetOffset { x, y } => positioner.offset = (x, y),
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_positioner::XdgPositioner,
        _data: &(),
    ) {
        state.positioners.remove(&resource.id());
    }
}

impl Dispatch<xdg_popup::XdgPopup, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &xdg_popup::XdgPopup,
        _request: xdg_popup::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_popup::XdgPopup,
        _data: &(),
    ) {
        let id = resource.id();
        state.remove_popups_where(|popup| popup.resource.id() == id);
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &xdg_toplevel::XdgToplevel,
        request: xdg_toplevel::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &xdg_toplevel::XdgToplevel,
        _data: &(),
    ) {
        if let Some(top) = state.toplevels.remove(&resource.id()) {
            state.toplevel_order.retain(|order| order != &resource.id());
            state.clear_focus(&top.surface);
            state.dismiss_orphans();
            state.pending_present = true;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &wl_callback::WlCallback,
        _request: wl_callback::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        _state: &mut Self,
        _client: wayland_server::backend::ClientId,
        _resource: &wl_callback::WlCallback,
        _data: &(),
    ) {
    }
}

pub fn route_xdg_wm_base_get_surface(
    state: &mut State,
    resource: &xdg_wm_base::XdgWmBase,
    id: New<xdg_surface::XdgSurface>,
    surface: wl_surface::WlSurface,
    data_init: &mut DataInit<'_, State>,
) {
    let surf_id = surface.id();
    let has_role = state.surfaces.get(&surf_id).map(|s| s.has_role).unwrap_or(true);
    if has_role {
        resource.post_error(
            xdg_wm_base::Error::Role as u32,
            "surface already has role".to_string(),
        );
        return;
    }
    handle_get_xdg_surface(state, resource, id, surface, data_init);
}
