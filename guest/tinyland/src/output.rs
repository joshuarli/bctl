use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::connector::{self};
use drm::control::dumbbuffer::DumbBuffer;
use drm::control::framebuffer;
use drm::control::{AtomicCommitFlags, Device as ControlDevice};
use drm::Device;
use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::path::Path;

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    fn open(path: &Path) -> std::io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true);
        Ok(Card(opts.open(path)?))
    }

    fn from_file(file: File) -> Self {
        Card(file)
    }

    fn receive_flip_events(&self) -> std::io::Result<drm::control::Events> {
        match self.receive_events() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(drm::control::Events::with_event_buf([0; 1024], 0)),
            result => result,
        }
    }
}

#[derive(Clone, Copy)]
enum AtomicStage { Capability, Commit }

fn atomic_unsupported(stage: AtomicStage, error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(38 | 95))
        || matches!(stage, AtomicStage::Capability) && error.raw_os_error() == Some(22)
}

struct AtomicError {
    operation: &'static str,
    source: std::io::Error,
}

impl AtomicError {
    fn new(operation: &'static str, source: std::io::Error) -> Self { Self { operation, source } }
}

impl std::fmt::Display for AtomicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.source)
    }
}

struct ScanoutBuffer {
    card: std::sync::Arc<Card>,
    db: DumbBuffer,
    fb: framebuffer::Handle,
}

impl ScanoutBuffer {
    fn new(card: std::sync::Arc<Card>, width: u32, height: u32) -> Result<Self, String> {
        let db = card.create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)
            .map_err(|error| format!("create dumb {width}x{height}: {error}"))?;
        let fb = match card.add_framebuffer(&db, 24, 32) {
            Ok(fb) => fb,
            Err(error) => {
                if let Err(cleanup) = card.destroy_dumb_buffer(db) { eprintln!("destroy dumb after add fb failure: {cleanup}"); }
                return Err(format!("add fb: {error}"));
            }
        };
        Ok(Self { card, db, fb })
    }
}

impl Drop for ScanoutBuffer {
    fn drop(&mut self) {
        if let Err(error) = self.card.destroy_framebuffer(self.fb) { eprintln!("destroy framebuffer: {error}"); }
        if let Err(error) = self.card.destroy_dumb_buffer(self.db) { eprintln!("destroy dumb buffer: {error}"); }
    }
}

pub struct DrmOutput {
    card: std::sync::Arc<Card>,
    buffers: [ScanoutBuffer; 2],
    front: usize,
    crtc: drm::control::crtc::Handle,
    width: u32,
    height: u32,
    mode_path: &'static str,
    in_flight: Option<InFlight>,
}

struct InFlight {
    back: usize,
    deadline: std::time::Instant,
}

impl AsFd for DrmOutput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.card.as_fd()
    }
}

fn select_compatible_crtc(
    card: &Card,
    res: &drm::control::ResourceHandles,
    con: &connector::Info,
) -> Result<drm::control::crtc::Handle, String> {
    if let Some(enc_handle) = con.current_encoder() {
        if let Ok(enc) = card.get_encoder(enc_handle) {
            if let Some(crtc) = enc.crtc() {
                if res.crtcs().contains(&crtc) {
                    return Ok(crtc);
                }
            }
        }
    }
    for enc_handle in con.encoders() {
        if let Ok(enc) = card.get_encoder(*enc_handle) {
            let compatible = res.filter_crtcs(enc.possible_crtcs());
            if let Some(crtc) = compatible.into_iter().find(|crtc| res.crtcs().contains(crtc)) {
                return Ok(crtc);
            }
        }
    }
    res.crtcs()
        .first()
        .copied()
        .inspect(|_| eprintln!("TINYLAND_CRTC_FALLBACK using first resource crtc"))
        .ok_or_else(|| "no crtc".to_string())
}

impl DrmOutput {
    pub fn open(device: &Path, want_w: u32, want_h: u32) -> Result<Self, String> {
        let file = Card::open(device).map_err(|e| format!("open {}: {e}", device.display()))?.0;
        Self::open_from_file(file, want_w, want_h, Some(device.display().to_string()))
    }

    pub fn open_from_file(
        file: File,
        want_w: u32,
        want_h: u32,
        origin: Option<String>,
    ) -> Result<Self, String> {
        let card = std::sync::Arc::new(Card::from_file(file));
        if let Err(error) = card.set_client_capability(drm::ClientCapability::UniversalPlanes, true) {
            if !atomic_unsupported(AtomicStage::Capability, &error) { return Err(format!("universal planes: {error}")); }
        }
        let atomic_ok = match card.set_client_capability(drm::ClientCapability::Atomic, true) {
            Ok(()) => true,
            Err(error) if atomic_unsupported(AtomicStage::Capability, &error) => {
                eprintln!("TINYLAND_ATOMIC_UNSUPPORTED capability: {error}");
                false
            }
            Err(error) => return Err(format!("atomic capability: {error}")),
        };

        let res = card
            .resource_handles()
            .map_err(|e| format!("resource handles: {e}"))?;
        let connectors: Vec<connector::Info> = res
            .connectors()
            .iter()
            .flat_map(|h| card.get_connector(*h, false))
            .collect();
        if connectors.is_empty() {
            return Err("no connectors".into());
        }
        let con = connectors
            .iter()
            .find(|c| c.state() == connector::State::Connected)
            .or_else(|| {
                eprintln!("TINYLAND_CONNECTOR_FALLBACK no connected connector, using first");
                connectors.first()
            })
            .ok_or("no connector")?;
        let con_handle = con.handle();
        let mode = con
            .modes()
            .first()
            .copied()
            .ok_or_else(|| {
                format!(
                    "connector {:?} has no modes; requested {want_w}x{want_h}",
                    con.handle()
                )
            })?;
        let (mw, mh) = mode.size();
        log::info!("drm connector {:?} mode {mw}x{mh}", con.handle());
        let (w, h): (u32, u32) = (mw.into(), mh.into());
        if w < want_w || h < want_h {
            return Err(format!(
                "mode {w}x{h} smaller than requested {want_w}x{want_h}"
            ));
        }

        let crtc = select_compatible_crtc(card.as_ref(), &res, con)?;
        eprintln!("TINYLAND_CRTC_SELECTED connector={con_handle:?} crtc={crtc:?}");
        let front = ScanoutBuffer::new(card.clone(), w, h)?;
        let back = ScanoutBuffer::new(card.clone(), w, h)?;
        eprintln!("TINYLAND_SCANOUT format=XRGB8888 buffers=2 size={w}x{h}");
        let mut out = DrmOutput {
            card, buffers: [front, back], front: 0, crtc,
            width: w, height: h, mode_path: "unset", in_flight: None,
        };
        if let Some(origin) = origin {
            eprintln!("TINYLAND_DRM_ORIGIN {origin}");
        }
        out.modeset(&mode, con_handle, atomic_ok)?;
        Ok(out)
    }

    fn modeset(
        &mut self,
        mode: &drm::control::Mode,
        con: connector::Handle,
        atomic_ok: bool,
    ) -> Result<(), String> {
        if atomic_ok {
            match self.atomic_modeset(mode, con) {
                Ok(()) => {
                    self.mode_path = "atomic";
                    log::info!("drm modeset path=atomic");
                    return Ok(());
                }
                Err(error) if atomic_unsupported(AtomicStage::Commit, &error.source) => {
                    eprintln!("TINYLAND_ATOMIC_UNSUPPORTED {error}");
                }
                Err(error) => return Err(format!("atomic modeset: {error}")),
            }
        }
        let crtc_h = self.crtc;
        self.card
            .set_crtc(crtc_h, Some(self.buffers[self.front].fb), (0, 0), &[con], Some(*mode))
            .map_err(|e| format!("legacy set_crtc: {e}"))?;
        self.mode_path = "legacy";
        log::info!("drm modeset path=legacy");
        Ok(())
    }

    fn atomic_modeset(
        &self,
        mode: &drm::control::Mode,
        con: connector::Handle,
    ) -> Result<(), AtomicError> {
        use drm::control::{atomic, property};
        let res = self
            .card
            .resource_handles()
            .map_err(|error| AtomicError::new("resources", error))?;
        let crtc_h = self.crtc;
        let planes = self.card.plane_handles().map_err(|error| AtomicError::new("planes", error))?;
        let mut primary: Option<drm::control::plane::Handle> = None;
        for p in &planes {
            let info = self
                .card
                .get_plane(*p)
                .map_err(|error| AtomicError::new("plane info", error))?;
            if !res.filter_crtcs(info.possible_crtcs()).contains(&crtc_h) {
                continue;
            }
            let props = self
                .card
                .get_properties(*p)
                .map_err(|error| AtomicError::new("plane props", error))?;
            let mut is_primary = false;
            for (id, val) in props.iter() {
                let pinfo = self
                    .card
                    .get_property(*id)
                    .map_err(|error| AtomicError::new("prop info", error))?;
                if pinfo.name().to_str().map(|x| x == "type").unwrap_or(false)
                    && *val == (drm::control::PlaneType::Primary as u64).into()
                {
                    is_primary = true;
                }
            }
            if is_primary {
                primary = Some(*p);
                break;
            }
        }
        let plane = primary.ok_or_else(|| AtomicError::new("plane", std::io::Error::other("no primary plane for crtc")))?;
        let con_props = self
            .card
            .get_properties(con)
            .map_err(|error| AtomicError::new("con props", error))?
            .as_hashmap(self.card.as_ref())
            .map_err(|error| AtomicError::new("con props map", error))?;
        let crtc_props = self
            .card
            .get_properties(crtc_h)
            .map_err(|error| AtomicError::new("crtc props", error))?
            .as_hashmap(self.card.as_ref())
            .map_err(|error| AtomicError::new("crtc props map", error))?;
        let plane_props = self
            .card
            .get_properties(plane)
            .map_err(|error| AtomicError::new("plane props", error))?
            .as_hashmap(self.card.as_ref())
            .map_err(|error| AtomicError::new("plane props map", error))?;
        let blob = self
            .card
            .create_property_blob(mode)
            .map_err(|error| AtomicError::new("blob", error))?;
        let mut req = atomic::AtomicModeReq::new();
        req.add_property(
            con,
            con_props.get("CRTC_ID").ok_or_else(|| AtomicError::new("atomic property CRTC_ID", std::io::Error::other("missing property")))?.handle(),
            property::Value::CRTC(Some(crtc_h)),
        );
        req.add_property(crtc_h, crtc_props.get("MODE_ID").ok_or_else(|| AtomicError::new("atomic property MODE_ID", std::io::Error::other("missing property")))?.handle(), blob);
        req.add_property(
            crtc_h,
            crtc_props.get("ACTIVE").ok_or_else(|| AtomicError::new("atomic property ACTIVE", std::io::Error::other("missing property")))?.handle(),
            property::Value::Boolean(true),
        );
        req.add_property(
            plane,
            plane_props.get("FB_ID").ok_or_else(|| AtomicError::new("atomic property FB_ID", std::io::Error::other("missing property")))?.handle(),
            property::Value::Framebuffer(Some(self.buffers[self.front].fb)),
        );
        req.add_property(
            plane,
            plane_props.get("CRTC_ID").ok_or_else(|| AtomicError::new("atomic property CRTC_ID", std::io::Error::other("missing property")))?.handle(),
            property::Value::CRTC(Some(crtc_h)),
        );
        req.add_property(plane, plane_props.get("SRC_X").ok_or_else(|| AtomicError::new("atomic property SRC_X", std::io::Error::other("missing property")))?.handle(), property::Value::UnsignedRange(0));
        req.add_property(plane, plane_props.get("SRC_Y").ok_or_else(|| AtomicError::new("atomic property SRC_Y", std::io::Error::other("missing property")))?.handle(), property::Value::UnsignedRange(0));
        let (mw, mh) = mode.size();
        req.add_property(
            plane,
            plane_props.get("SRC_W").ok_or_else(|| AtomicError::new("atomic property SRC_W", std::io::Error::other("missing property")))?.handle(),
            property::Value::UnsignedRange((mw as u64) << 16),
        );
        req.add_property(
            plane,
            plane_props.get("SRC_H").ok_or_else(|| AtomicError::new("atomic property SRC_H", std::io::Error::other("missing property")))?.handle(),
            property::Value::UnsignedRange((mh as u64) << 16),
        );
        req.add_property(
            plane,
            plane_props.get("CRTC_X").ok_or_else(|| AtomicError::new("atomic property CRTC_X", std::io::Error::other("missing property")))?.handle(),
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            plane_props.get("CRTC_Y").ok_or_else(|| AtomicError::new("atomic property CRTC_Y", std::io::Error::other("missing property")))?.handle(),
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            plane_props.get("CRTC_W").ok_or_else(|| AtomicError::new("atomic property CRTC_W", std::io::Error::other("missing property")))?.handle(),
            property::Value::UnsignedRange(mw as u64),
        );
        req.add_property(
            plane,
            plane_props.get("CRTC_H").ok_or_else(|| AtomicError::new("atomic property CRTC_H", std::io::Error::other("missing property")))?.handle(),
            property::Value::UnsignedRange(mh as u64),
        );
        self.card
            .atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)
            .map_err(|error| AtomicError::new("atomic commit", error))?;
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn mode_path(&self) -> &str {
        &self.mode_path
    }

    pub fn fill_solid(&mut self, r: u8, g: u8, b: u8) -> Result<u32, String> {
        let mut pixels = vec![0; self.width as usize * self.height as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) { pixel.copy_from_slice(&[b, g, r, 255]); }
        self.present_pixels(&pixels, self.width, self.height)
    }

    pub fn has_pending_flip(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn flip_deadline(&self) -> Option<std::time::Instant> {
        self.in_flight.as_ref().map(|flight| flight.deadline)
    }

    pub fn queue_present(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<(), String> {
        if self.in_flight.is_some() {
            return Err("page flip already pending".into());
        }
        if (width, height) != self.size() || pixels.len() != width as usize * height as usize * 4 {
            return Err("presentation must cover the complete output".into());
        }
        let back = 1 - self.front;
        let buffer = &mut self.buffers[back];
        let stride = buffer.db.pitch() as usize;
        let row_bytes = width as usize * 4;
        let mut map = self.card.map_dumb_buffer(&mut buffer.db).map_err(|error| format!("map scanout: {error}"))?;
        for y in 0..height as usize {
            map[y * stride..y * stride + row_bytes].copy_from_slice(&pixels[y * row_bytes..(y + 1) * row_bytes]);
        }
        drop(map);
        let rect = drm::control::ClipRect::new(0, 0, width as u16, height as u16);
        self.card.dirty_framebuffer(buffer.fb, &[rect]).map_err(|error| format!("dirty framebuffer: {error}"))?;
        self.card.page_flip(self.crtc, buffer.fb, drm::control::PageFlipFlags::EVENT, None)
            .map_err(|error| format!("queue page flip: {error}"))?;
        self.in_flight = Some(InFlight { back, deadline: std::time::Instant::now() + std::time::Duration::from_secs(2) });
        Ok(())
    }

    pub fn poll_flip(&mut self) -> Result<Option<u32>, String> {
        let (back, deadline) = match &self.in_flight {
            Some(flight) => (flight.back, flight.deadline),
            None => return Ok(None),
        };
        let events = self.card.receive_flip_events().map_err(|error| format!("page flip events: {error}"))?;
        for event in events {
            if let drm::control::Event::PageFlip(event) = event {
                if event.crtc != self.crtc { return Err("page flip on unexpected crtc".into()); }
                self.front = back;
                self.in_flight = None;
                eprintln!("TINYLAND_PRESENTED sequence={} time_ms={} path={}", event.frame, event.duration.as_millis(), self.mode_path);
                return Ok(Some(event.duration.as_millis() as u32));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("page flip timed out".into());
        }
        Ok(None)
    }

    pub fn present_pixels(&mut self, pixels: &[u8], width: u32, height: u32) -> Result<u32, String> {
        self.queue_present(pixels, width, height)?;
        let deadline = self.flip_deadline().ok_or("page flip timed out")?;
        loop {
            let timeout = deadline.checked_duration_since(std::time::Instant::now()).ok_or("page flip timed out")?;
            let timeout = rustix::time::Timespec::try_from(timeout).map_err(|error| format!("page flip timeout: {error}"))?;
            let mut fds = [rustix::event::PollFd::new(self.card.as_ref(), rustix::event::PollFlags::IN)];
            rustix::event::poll(&mut fds, Some(&timeout)).map_err(|error| format!("page flip poll: {error}"))?;
            if !fds[0].revents().contains(rustix::event::PollFlags::IN) {
                return Err("page flip timed out or device disconnected".into());
            }
            if let Some(time) = self.poll_flip()? {
                return Ok(time);
            }
        }
    }
}

pub fn drm_proof(device: &Path, width: u32, height: u32, shutdown: &crate::shutdown::Shutdown) -> i32 {
    match DrmOutput::open(device, width, height) {
        Ok(mut out) => {
            let (w, h) = out.size();
            if out.fill_solid(0x1A, 0x99, 0xCC).is_err() {
                eprintln!("drm-proof: fill failed");
                return 1;
            }
            println!("DRM_PROOF_OK size={w}x{h} path={}", out.mode_path());
            while shutdown.signal().is_none() {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            0
        }
        Err(e) => {
            eprintln!("drm-proof failed: {e}");
            1
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonblocking_page_flip_poll_waits_for_an_event() {
        use std::io::Write;
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        let (reader, mut writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let card = Card(File::from(OwnedFd::from(reader)));
        assert_eq!(card.receive_flip_events().unwrap().count(), 0);
        let event: Vec<u8> = [2u32, 32, 0, 0, 1, 0, 7, 39].into_iter().flat_map(u32::to_ne_bytes).collect();
        writer.write_all(&event).unwrap();
        let events: Vec<_> = card.receive_flip_events().unwrap().collect();
        assert_eq!(events.len(), 1);
        let drm::control::Event::PageFlip(event) = &events[0] else { panic!("expected page flip") };
        assert_eq!(event.frame, 7);
        assert_eq!(u32::from(event.crtc), 39);
        let invalid = Card(File::open(".").unwrap());
        assert_eq!(invalid.receive_flip_events().err().unwrap().kind(), std::io::ErrorKind::IsADirectory);
    }

    #[test]
    fn only_unsupported_atomic_failures_allow_legacy_fallback() {
        for errno in [38, 95] {
            assert!(atomic_unsupported(AtomicStage::Capability, &std::io::Error::from_raw_os_error(errno)));
            assert!(atomic_unsupported(AtomicStage::Commit, &std::io::Error::from_raw_os_error(errno)));
        }
        assert!(atomic_unsupported(AtomicStage::Capability, &std::io::Error::from_raw_os_error(22)));
        for errno in [1, 5, 12, 13, 19, 22] {
            assert!(!atomic_unsupported(AtomicStage::Commit, &std::io::Error::from_raw_os_error(errno)));
        }
        assert!(!atomic_unsupported(AtomicStage::Capability, &std::io::Error::from_raw_os_error(13)));
    }
}
