use std::ffi::CString;
use wayland_server::backend::protocol::Interface;
use wayland_server::Resource;

static WL_DISPLAY: Interface = Interface { name: "wl_display", version: 1, requests: &[], events: &[], c_ptr: None };

const WL_DISPLAY_ID: u32 = 1;

#[derive(Clone, Copy)]
pub(super) enum DisplayError {
    NoMemory = 2,
    Implementation = 3,
}

pub(super) fn post(resource: &impl Resource, error: DisplayError, message: &str) {
    let Some(handle) = resource.handle().upgrade() else { return };
    let Ok(client) = handle.get_client(resource.id()) else { return };
    let display = handle
        .object_for_protocol_id(client, &WL_DISPLAY, WL_DISPLAY_ID)
        .expect("every client has a wl_display at id 1");
    let message = CString::new(message).expect("error messages have no NUL");
    handle.post_error(display, error as u32, message);
}
