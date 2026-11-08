use std::collections::HashMap;
use wayland_server::backend::ObjectId;
use wayland_server::protocol::{wl_data_device, wl_data_source};

#[derive(Debug)]
pub struct DataSourceData {
    pub resource: wl_data_source::WlDataSource,
    pub mime_types: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SelectionState {
    pub sources: HashMap<ObjectId, DataSourceData>,
    pub devices: HashMap<ObjectId, wl_data_device::WlDataDevice>,
    pub offers: HashMap<ObjectId, wl_data_source::WlDataSource>,
    pub selection: Option<ObjectId>,
}
