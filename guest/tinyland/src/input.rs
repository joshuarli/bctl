use evdev::{AbsoluteAxisCode, Device, EventType, KeyCode, RelativeAxisCode, SynchronizationCode};
use std::fs::File;
use std::io::Write;
use wayland_server::protocol::{wl_keyboard, wl_pointer, wl_seat};

#[derive(Debug)]
pub struct SeatData {
    pub resource: wl_seat::WlSeat,
    pub keyboards: Vec<wl_keyboard::WlKeyboard>,
    pub pointers: Vec<wl_pointer::WlPointer>,
    pub keyboard_focus: Option<wayland_server::backend::ObjectId>,
}

impl SeatData {
    pub fn new(resource: wl_seat::WlSeat) -> Self {
        Self { resource, keyboards: Vec::new(), pointers: Vec::new(), keyboard_focus: None }
    }
}

pub struct Keyboard {
    machine: kbvm::state_machine::StateMachine,
    state: kbvm::state_machine::State,
    components: kbvm::Components,
    #[allow(dead_code)]
    lookup: kbvm::lookup::LookupTable,
    pub keymap: File,
    pub keymap_size: u32,
}

impl Keyboard {
    pub fn new() -> Result<Self, String> {
        let mut builder = kbvm::xkb::Context::builder();
        if let Some(root) = std::env::var_os("TINYLAND_XKB_ROOT") {
            let path = std::path::PathBuf::from(&root);
            if path.exists() {
                builder.prepend_path(&path);
                eprintln!("TINYLAND_XKB_ROOT {}", path.display());
            } else {
                return Err(format!("TINYLAND_XKB_ROOT {} missing", path.display()));
            }
        }
        let context = builder.build();
        let groups = [kbvm::xkb::rmlvo::Group { layout: "us", variant: "" }];
        let keymap = context.keymap_from_names(
            kbvm::xkb::diagnostic::WriteToLog,
            Some("evdev"),
            Some("pc105"),
            Some(&groups),
            Some(&[]),
        );
        if keymap.keys().count() == 0 {
            let system = std::path::Path::new("/usr/share/X11/xkb/rules/evdev");
            return Err(format!(
                "compile US XKB keymap (override={:?}, system-rules-present={})",
                std::env::var_os("TINYLAND_XKB_ROOT").or_else(|| std::env::var_os("XKB_CONFIG_ROOT")),
                system.exists(),
            ));
        }
        let text = format!("{}\n", keymap.format());
        let mut bytes = text.into_bytes();
        bytes.push(0);
        let mut file = File::from(rustix::fs::memfd_create("tinyland-keymap", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| format!("keymap memfd: {e}"))?);
        file.write_all(&bytes).map_err(|e| format!("keymap write: {e}"))?;
        let size = bytes.len() as u32;
        let agent = keymap.to_builder();
        let machine = agent.build_state_machine();
        let lookup = agent.build_lookup_table();
        let state = machine.create_state();
        Ok(Self { machine, state, components: kbvm::Components::default(), lookup, keymap: file, keymap_size: size })
    }

    pub fn update(&mut self, code: u16, down: bool) -> [u32; 4] {
        let key = kbvm::Keycode::from_evdev(u32::from(code));
        let direction = if down {
            kbvm::state_machine::Direction::Down
        } else {
            kbvm::state_machine::Direction::Up
        };
        let mut events = Vec::new();
        self.machine.handle_key(&mut self.state, &mut events, key, direction);
        for event in events {
            self.components.apply_event(event);
        }
        self.modifiers()
    }

    pub fn modifiers(&self) -> [u32; 4] {
        [self.components.mods_pressed.0, self.components.mods_latched.0, self.components.mods_locked.0, self.components.group.0]
    }

    #[allow(dead_code)]
    pub fn key_utf8(&self, xkb_code: u32) -> String {
        let evdev = xkb_code.saturating_sub(8);
        let key = kbvm::Keycode::from_evdev(evdev);
        self.lookup.lookup(self.components.group, self.components.mods, key).into_iter().flat_map(|p| p.char()).collect()
    }

    #[allow(dead_code)]
    pub fn reset_stuck(&mut self) -> [u32; 4] {
        self.state = self.machine.create_state();
        self.components = kbvm::Components::default();
        self.modifiers()
    }
}

pub const BTN_LEFT: u16 = 0x110;
const BTN_TASK: u16 = 0x117;
const BTN_MISC: u16 = 0x100;
const BTN_GEAR_UP: u16 = 0x151;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AbsRange {
    minimum: i32,
    maximum: i32,
}

impl AbsRange {
    fn new(minimum: i32, maximum: i32) -> Option<Self> {
        (maximum > minimum).then_some(AbsRange { minimum, maximum })
    }

    fn normalize(&self, value: i32) -> f64 {
        let clamped = value.clamp(self.minimum, self.maximum);
        f64::from(clamped - self.minimum) / f64::from(self.maximum - self.minimum)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Absolute { x: f64, y: f64 },
    Relative { dx: f64, dy: f64 },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PointerFrame {
    pub motion: Option<Motion>,
    pub buttons: Vec<(u16, bool)>,
    pub wheel: (i32, i32),
}

impl PointerFrame {
    fn is_empty(&self) -> bool {
        self.motion.is_none() && self.buttons.is_empty() && self.wheel == (0, 0)
    }
}

#[derive(Debug)]
enum Axes {
    Absolute { x: AbsRange, y: AbsRange, position: (f64, f64) },
    Relative,
}

#[derive(Debug)]
pub struct PointerReader {
    axes: Axes,
    pending: PointerFrame,
}

impl PointerReader {
    fn absolute(x: AbsRange, y: AbsRange, start: (i32, i32)) -> Self {
        let position = (x.normalize(start.0), y.normalize(start.1));
        PointerReader { axes: Axes::Absolute { x, y, position }, pending: PointerFrame::default() }
    }

    fn relative() -> Self {
        PointerReader { axes: Axes::Relative, pending: PointerFrame::default() }
    }

    fn feed(&mut self, event: &evdev::InputEvent) -> Option<PointerFrame> {
        match (event.event_type(), &mut self.axes) {
            (EventType::ABSOLUTE, Axes::Absolute { x, y, position }) => {
                if event.code() == AbsoluteAxisCode::ABS_X.0 {
                    position.0 = x.normalize(event.value());
                } else if event.code() == AbsoluteAxisCode::ABS_Y.0 {
                    position.1 = y.normalize(event.value());
                } else {
                    return None;
                }
                self.pending.motion = Some(Motion::Absolute { x: position.0, y: position.1 });
            }
            (EventType::RELATIVE, axes) => {
                let code = RelativeAxisCode(event.code());
                let value = event.value();
                let delta = match code {
                    RelativeAxisCode::REL_X => (f64::from(value), 0.0),
                    RelativeAxisCode::REL_Y => (0.0, f64::from(value)),
                    RelativeAxisCode::REL_WHEEL => {
                        self.pending.wheel.0 += value;
                        return None;
                    }
                    RelativeAxisCode::REL_HWHEEL => {
                        self.pending.wheel.1 += value;
                        return None;
                    }
                    _ => return None,
                };
                if matches!(axes, Axes::Relative) {
                    let (dx, dy) = match self.pending.motion {
                        Some(Motion::Relative { dx, dy }) => (dx, dy),
                        _ => (0.0, 0.0),
                    };
                    self.pending.motion = Some(Motion::Relative { dx: dx + delta.0, dy: dy + delta.1 });
                }
            }
            (EventType::KEY, _) if (BTN_LEFT..=BTN_TASK).contains(&event.code()) && event.value() != 2 => {
                self.pending.buttons.push((event.code(), event.value() != 0));
            }
            (EventType::SYNCHRONIZATION, _) if event.code() == SynchronizationCode::SYN_REPORT.0 => {
                let frame = std::mem::take(&mut self.pending);
                return (!frame.is_empty()).then_some(frame);
            }
            _ => {}
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Key { code: u16, down: bool },
    Pointer(PointerFrame),
}

fn is_button(code: u16) -> bool {
    (BTN_MISC..=BTN_GEAR_UP).contains(&code)
}

pub struct InputDevice {
    device: Device,
    pointer: Option<PointerReader>,
}

impl std::os::fd::AsFd for InputDevice {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.device.as_fd()
    }
}

impl InputDevice {
    pub fn is_pointer(&self) -> bool {
        self.pointer.is_some()
    }

    pub fn is_keyboard(&self) -> bool {
        is_keyboard(&self.device)
    }

    fn translate(&mut self, events: impl Iterator<Item = evdev::InputEvent>) -> Vec<InputEvent> {
        let mut out = Vec::new();
        for event in events {
            if event.event_type() == EventType::KEY && event.value() != 2 && !is_button(event.code()) {
                out.push(InputEvent::Key { code: event.code(), down: event.value() != 0 });
                continue;
            }
            if let Some(frame) = self.pointer.as_mut().and_then(|pointer| pointer.feed(&event)) {
                out.push(InputEvent::Pointer(frame));
            }
        }
        out
    }

    pub fn read(&mut self) -> Result<Vec<InputEvent>, std::io::Error> {
        let events: Vec<evdev::InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(self.translate(events.into_iter()))
    }
}

fn is_keyboard(device: &Device) -> bool {
    device.supported_keys().is_some_and(|keys|
        [KeyCode::KEY_A, KeyCode::KEY_Z, KeyCode::KEY_ENTER].iter().all(|key| keys.contains(*key)))
}

fn pointer_reader(device: &Device, origin: &str) -> Result<Option<PointerReader>, String> {
    if !device.supported_keys().is_some_and(|keys| keys.contains(KeyCode::BTN_LEFT)) {
        return Ok(None);
    }
    let absolute = device.supported_absolute_axes().is_some_and(|axes|
        axes.contains(AbsoluteAxisCode::ABS_X) && axes.contains(AbsoluteAxisCode::ABS_Y));
    if absolute {
        let mut x = None;
        let mut y = None;
        for (axis, info) in device.get_absinfo().map_err(|e| format!("input {origin} absinfo: {e}"))? {
            let range = AbsRange::new(info.minimum(), info.maximum()).map(|range| (range, info.value()));
            if axis == AbsoluteAxisCode::ABS_X { x = range; }
            if axis == AbsoluteAxisCode::ABS_Y { y = range; }
        }
        let (Some((x, x_value)), Some((y, y_value))) = (x, y) else {
            return Err(format!("input {origin} reports an empty absolute pointer range"));
        };
        return Ok(Some(PointerReader::absolute(x, y, (x_value, y_value))));
    }
    let relative = device.supported_relative_axes().is_some_and(|axes|
        axes.contains(RelativeAxisCode::REL_X) && axes.contains(RelativeAxisCode::REL_Y));
    Ok(relative.then(PointerReader::relative))
}

pub fn device_from_fd(fd: std::os::fd::OwnedFd, origin: &str) -> Result<Option<InputDevice>, String> {
    let device = Device::from_fd(fd).map_err(|e| format!("input from {origin}: {e}"))?;
    adopt(device, origin)
}

fn adopt(mut device: Device, origin: &str) -> Result<Option<InputDevice>, String> {
    let keyboard = is_keyboard(&device);
    let pointer = pointer_reader(&device, origin)?;
    if !keyboard && pointer.is_none() {
        return Ok(None);
    }
    device.set_nonblocking(true).map_err(|e| format!("input nonblocking: {e}"))?;
    device.grab().map_err(|e| format!("input grab: {e}"))?;
    if keyboard {
        eprintln!("TINYLAND_KEYBOARD device={origin} name={:?}", device.name());
    }
    if let Some(pointer) = &pointer {
        let kind = match pointer.axes { Axes::Absolute { .. } => "absolute", Axes::Relative => "relative" };
        eprintln!("TINYLAND_POINTER device={origin} kind={kind} name={:?}", device.name());
    }
    Ok(Some(InputDevice { device, pointer }))
}

pub fn discover_device_paths() -> Result<Vec<std::path::PathBuf>, String> {
    let mut entries = std::fs::read_dir("/dev/input")
        .map_err(|e| format!("read /dev/input: {e}"))?
        .collect::<Result<Vec<_>, _>>().map_err(|e| format!("read input entry: {e}"))?;
    entries.sort_by_key(|e| e.path());
    let mut paths = Vec::new();
    for entry in entries {
        if !entry.file_name().as_encoded_bytes().starts_with(b"event") { continue; }
        paths.push(entry.path());
    }
    Ok(paths)
}

pub fn discover_devices() -> Result<Vec<InputDevice>, String> {
    let mut devices = Vec::new();
    for path in discover_device_paths()? {
        let device = Device::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        if let Some(device) = adopt(device, &path.display().to_string())? {
            devices.push(device);
        }
    }
    require_keyboard(&devices)?;
    Ok(devices)
}

pub fn require_keyboard<'a>(devices: impl IntoIterator<Item = &'a InputDevice>) -> Result<(), String> {
    if devices.into_iter().any(InputDevice::is_keyboard) {
        Ok(())
    } else {
        Err("no keyboard with alphabetic and enter keys".into())
    }
}

/// True when the read error means the device is gone (unplugged).
pub fn is_gone(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::NODEV.raw_os_error())
}

pub fn vt_switch_target(code: u16, depressed: u32) -> Option<i32> {
    if depressed & 0x0c != 0x0c {
        return None;
    }
    if (KeyCode::KEY_F1.code()..=KeyCode::KEY_F10.code()).contains(&code) {
        Some(i32::from(code - KeyCode::KEY_F1.code()) + 1)
    } else if (KeyCode::KEY_F11.code()..=KeyCode::KEY_F12.code()).contains(&code) {
        Some(i32::from(code - KeyCode::KEY_F11.code()) + 11)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_changes_text_and_releases_its_modifier() {
        let mut keyboard = Keyboard::new().unwrap();
        let a = KeyCode::KEY_A.code() as u32 + 8;
        assert_eq!(keyboard.key_utf8(a), "a");
        assert_ne!(keyboard.update(KeyCode::KEY_LEFTSHIFT.code(), true)[0], 0);
        assert_eq!(keyboard.key_utf8(a), "A");
        assert_eq!(keyboard.update(KeyCode::KEY_LEFTSHIFT.code(), false)[0], 0);
        assert_eq!(keyboard.key_utf8(a), "a");
    }

    #[test]
    fn reset_clears_depressed_modifiers_for_resume() {
        let mut keyboard = Keyboard::new().unwrap();
        assert_ne!(keyboard.update(KeyCode::KEY_LEFTSHIFT.code(), true)[0], 0);
        assert_eq!(keyboard.reset_stuck(), [0, 0, 0, 0]);
    }

    #[test]
    fn gone_matches_unplug_errno_only() {
        let gone = std::io::Error::from_raw_os_error(rustix::io::Errno::NODEV.raw_os_error());
        assert!(is_gone(&gone));
        assert!(!is_gone(&std::io::Error::from(std::io::ErrorKind::WouldBlock)));
        assert!(!is_gone(&std::io::Error::from_raw_os_error(
            rustix::io::Errno::INVAL.raw_os_error()
        )));
    }

    #[test]
    fn vt_switch_needs_ctrl_alt_and_function_key() {
        let mut keyboard = Keyboard::new().unwrap();
        assert_eq!(vt_switch_target(KeyCode::KEY_F2.code(), keyboard.modifiers()[0]), None);
        keyboard.update(KeyCode::KEY_LEFTCTRL.code(), true);
        assert_eq!(vt_switch_target(KeyCode::KEY_F2.code(), keyboard.modifiers()[0]), None);
        keyboard.update(KeyCode::KEY_LEFTALT.code(), true);
        assert_eq!(vt_switch_target(KeyCode::KEY_F2.code(), keyboard.modifiers()[0]), Some(2));
        assert_eq!(vt_switch_target(KeyCode::KEY_F10.code(), keyboard.modifiers()[0]), Some(10));
        assert_eq!(vt_switch_target(KeyCode::KEY_F11.code(), keyboard.modifiers()[0]), Some(11));
        assert_eq!(vt_switch_target(KeyCode::KEY_F12.code(), keyboard.modifiers()[0]), Some(12));
        assert_eq!(vt_switch_target(KeyCode::KEY_A.code(), keyboard.modifiers()[0]), None);
        keyboard.update(KeyCode::KEY_LEFTCTRL.code(), false);
        assert_eq!(vt_switch_target(KeyCode::KEY_F2.code(), keyboard.modifiers()[0]), None);
    }

    #[test]
    fn seat_fd_that_is_not_an_input_device_is_rejected() {
        let fd = rustix::fs::memfd_create("tinyland-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        assert!(device_from_fd(fd, "test").is_err());
    }

    fn event(kind: EventType, code: u16, value: i32) -> evdev::InputEvent {
        evdev::InputEvent::new(kind.0, code, value)
    }

    fn syn() -> evdev::InputEvent {
        event(EventType::SYNCHRONIZATION, SynchronizationCode::SYN_REPORT.0, 0)
    }

    fn feed_all(reader: &mut PointerReader, events: &[evdev::InputEvent]) -> Vec<PointerFrame> {
        events.iter().filter_map(|event| reader.feed(event)).collect()
    }

    #[test]
    fn absolute_tablet_reports_normalized_position_once_per_report() {
        let range = AbsRange::new(0, 32767).unwrap();
        let mut reader = PointerReader::absolute(range, range, (0, 0));
        let frames = feed_all(&mut reader, &[
            event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, 32767),
            event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, 16383),
            syn(),
            event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_Y.0, 0),
            syn(),
        ]);
        assert_eq!(frames.len(), 2);
        let Some(Motion::Absolute { x, y }) = frames[0].motion else { panic!("{frames:?}") };
        assert_eq!(x, 1.0);
        assert!((y - 0.5).abs() < 0.001);
        assert_eq!(frames[1].motion, Some(Motion::Absolute { x: 1.0, y: 0.0 }));
    }

    #[test]
    fn absolute_values_outside_the_range_are_clamped() {
        let range = AbsRange::new(100, 200).unwrap();
        let mut reader = PointerReader::absolute(range, range, (150, 150));
        let frames = feed_all(&mut reader, &[event(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, 900), syn()]);
        assert_eq!(frames[0].motion, Some(Motion::Absolute { x: 1.0, y: 0.5 }));
        assert!(AbsRange::new(5, 5).is_none());
    }

    #[test]
    fn relative_mouse_accumulates_deltas_buttons_and_wheel_within_a_report() {
        let mut reader = PointerReader::relative();
        let frames = feed_all(&mut reader, &[
            event(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 3),
            event(EventType::RELATIVE, RelativeAxisCode::REL_X.0, 2),
            event(EventType::RELATIVE, RelativeAxisCode::REL_Y.0, -4),
            event(EventType::KEY, BTN_LEFT, 1),
            event(EventType::RELATIVE, RelativeAxisCode::REL_WHEEL.0, -1),
            syn(),
            syn(),
            event(EventType::KEY, BTN_LEFT, 0),
            syn(),
        ]);
        assert_eq!(frames, vec![
            PointerFrame { motion: Some(Motion::Relative { dx: 5.0, dy: -4.0 }), buttons: vec![(BTN_LEFT, true)], wheel: (-1, 0) },
            PointerFrame { motion: None, buttons: vec![(BTN_LEFT, false)], wheel: (0, 0) },
        ]);
    }

    #[test]
    fn buttons_reach_the_pointer_and_never_the_keyboard() {
        let mut reader = PointerReader::relative();
        assert!(reader.feed(&event(EventType::KEY, 0x14a, 1)).is_none());
        assert!(reader.feed(&syn()).is_none());
        assert!(is_button(BTN_LEFT));
        assert!(is_button(0x14a));
        assert!(!is_button(KeyCode::KEY_A.code()));
    }
}
