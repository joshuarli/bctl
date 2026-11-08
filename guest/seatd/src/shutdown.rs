use crate::admit;
use crate::r#loop::Server;
use crate::{log_error, log_info};
use std::fs::Metadata;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

pub struct SocketPath {
    path: String,
    metadata: Metadata,
}

impl SocketPath {
    pub fn capture(path: &str) -> io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bound socket path was replaced"));
        }
        Ok(Self { path: path.to_string(), metadata })
    }

    fn unlink(&self) -> io::Result<()> {
        let current = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if current.dev() == self.metadata.dev() && current.ino() == self.metadata.ino() {
            admit::unlink_path(&self.path)?;
        }
        Ok(())
    }
}

pub fn shutdown(server: Server, socket: SocketPath) {
    server.finish();
    if let Err(e) = socket.unlink() {
        log_error!("Could not remove socket: {e}");
    }
    log_info!("seatd stopped");
}
