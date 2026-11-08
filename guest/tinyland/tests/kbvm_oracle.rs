#![cfg(feature = "oracle")]
use evdev::KeyCode;
fn new_kbvm() -> (kbvm::state_machine::StateMachine, kbvm::state_machine::State, kbvm::Components) {
    let builder = kbvm::xkb::Context::builder();
    let context = builder.build();
    let groups = [kbvm::xkb::rmlvo::Group { layout: "us", variant: "" }];
    let keymap = context.keymap_from_names(
        kbvm::xkb::diagnostic::WriteToLog,
        Some("evdev"),
        Some("pc105"),
        Some(&groups),
        Some(&[]),
    );
    assert!(keymap.keys().count() > 0);
    let agent = keymap.to_builder();
    let machine = agent.build_state_machine();
    let state = machine.create_state();
    (machine, state, kbvm::Components::default())
}
fn kbvm_update(
    machine: &kbvm::state_machine::StateMachine,
    state: &mut kbvm::state_machine::State,
    components: &mut kbvm::Components,
    code: u16,
    down: bool,
) -> [u32; 4] {
    let key = kbvm::Keycode::from_evdev(u32::from(code));
    let direction = if down {
        kbvm::state_machine::Direction::Down
    } else {
        kbvm::state_machine::Direction::Up
    };
    let mut events = Vec::new();
    machine.handle_key(state, &mut events, key, direction);
    for event in events {
        components.apply_event(event);
    }
    [components.mods_pressed.0, components.mods_latched.0, components.mods_locked.0, components.group.0]
}
#[test]
fn kbvm_matches_libxkbcommon_oracle() {
    let (machine, mut state, mut components) = new_kbvm();
    let mut context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_DEFAULT_INCLUDES);
    let _ = context.include_path_append_default();
    let keymap = xkbcommon::xkb::Keymap::new_from_names(
        &context, "evdev", "pc105", "us", "", Some(String::new()), xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
    ).expect("oracle keymap");
    let mut oracle = xkbcommon::xkb::State::new(&keymap);
    let cases = [
        (KeyCode::KEY_A.code(), false),
        (KeyCode::KEY_A.code(), true),
        (KeyCode::KEY_LEFTSHIFT.code(), true),
        (KeyCode::KEY_A.code(), true),
        (KeyCode::KEY_A.code(), false),
        (KeyCode::KEY_LEFTSHIFT.code(), false),
        (KeyCode::KEY_LEFTCTRL.code(), true),
        (KeyCode::KEY_LEFTALT.code(), true),
        (KeyCode::KEY_CAPSLOCK.code(), true),
        (KeyCode::KEY_CAPSLOCK.code(), false),
    ];
    for (code, down) in cases {
        let direction = if down { xkbcommon::xkb::KeyDirection::Down } else { xkbcommon::xkb::KeyDirection::Up };
        oracle.update_key((u32::from(code) + 8).into(), direction);
        let expected = [
            oracle.serialize_mods(xkbcommon::xkb::STATE_MODS_DEPRESSED),
            oracle.serialize_mods(xkbcommon::xkb::STATE_MODS_LATCHED),
            oracle.serialize_mods(xkbcommon::xkb::STATE_MODS_LOCKED),
            oracle.serialize_layout(xkbcommon::xkb::STATE_LAYOUT_EFFECTIVE),
        ];
        let actual = kbvm_update(&machine, &mut state, &mut components, code, down);
        assert_eq!(actual, expected, "code={code} down={down}");
    }
}
