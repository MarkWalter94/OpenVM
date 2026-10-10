//! Console (F2) in the whole app, headless: synthetic frame instead of the
//! display socket, stand-in supervisor recording the input batches. Covers
//! coordinates after window resize, keyboard and mouse as `Input` batches
//! tied to run and geometry, release of the keys on focus loss, the
//! full screen and leave chords, screenshots, empty states and the ticket
//! request.

mod common;

use common::{FakeSupervisor, connected, deterministic_frame, vm};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use lightvm_desktop::console::{ConsoleTarget, LiveConsole};
use lightvm_desktop::service::{ConsoleRequest, Failure, RequestStatus, ServiceCommand};
use lightvm_desktop::{LightVmApp, Section};
use lightvm_proto::{InputBatch, InputEvent, KeyAction, PointerMove, QCode, VmState};

const GEOM: (usize, usize) = (1280, 800);

fn console_app() -> (Harness<'static, LightVmApp>, common::Handles) {
    let (fake, hs) = FakeSupervisor::new(connected(vec![vm(1, "ubuntu-dev", VmState::Running)]));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: common::vm_id(1),
            vm_name: "ubuntu-dev".into(),
            run_id: common::run_id(1),
        },
        deterministic_frame(GEOM.0, GEOM.1),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_section(Section::Console);
    let h = Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    (h, hs)
}

fn batches(hs: &common::Handles) -> Vec<InputBatch> {
    hs.sent()
        .into_iter()
        .filter_map(|c| match c {
            ServiceCommand::Input(b) => Some(b),
            _ => None,
        })
        .collect()
}

fn events(hs: &common::Handles) -> Vec<InputEvent> {
    batches(hs).into_iter().flat_map(|b| b.events).collect()
}

fn keys(hs: &common::Handles) -> Vec<(String, bool)> {
    events(hs)
        .into_iter()
        .filter_map(|e| match e {
            InputEvent::Key(KeyAction { qcode, down }) => Some((qcode.as_str().to_owned(), down)),
            _ => None,
        })
        .collect()
}

/// Key press and release with the physical key, as egui-winit produces them.
fn press(h: &Harness<'_, LightVmApp>, key: egui::Key, modifiers: egui::Modifiers) {
    for pressed in [true, false] {
        h.event(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        });
    }
}

fn image(h: &Harness<'_, LightVmApp>) -> egui::Rect {
    h.state().live_console().unwrap().image_rect().unwrap()
}

/// Pointer moves produced by hovering a position.
fn move_to(h: &mut Harness<'_, LightVmApp>, hs: &common::Handles, p: egui::Pos2) -> PointerMove {
    hs.clear();
    h.hover_at(p);
    h.run_steps(4);
    events(hs)
        .into_iter()
        .rev()
        .find_map(|e| match e {
            InputEvent::PointerMove(m) => Some(m),
            _ => None,
        })
        .expect("pointer move sent")
}

/// Expected absolute coordinate for a guest pixel (QEMU formula).
fn abs(px: u32, size: u32) -> u16 {
    (u64::from(px) * 32767 / u64::from(size - 1)) as u16
}

#[test]
fn coordinates_are_correct_after_resize() {
    let (mut h, hs) = console_app();
    h.run_steps(4);
    for size in [
        egui::vec2(1100.0, 760.0),
        egui::vec2(900.0, 600.0),
        egui::vec2(1600.0, 700.0),
        egui::vec2(950.0, 1000.0),
    ] {
        h.set_size(size);
        h.run_steps(4);
        let r = image(&h);
        // Aspect ratio of the guest kept.
        assert!(
            (r.width() / r.height() - 1.6).abs() < 0.01,
            "aspect at {size:?}: {r:?}"
        );
        let (w, hgt) = (GEOM.0 as u32, GEOM.1 as u32);
        let at = |fx: f32, fy: f32| egui::pos2(r.min.x + fx * r.width(), r.min.y + fy * r.height());
        let m = move_to(&mut h, &hs, at(0.0, 0.0) + egui::vec2(0.2, 0.2));
        assert_eq!((m.x, m.y), (0, 0), "top-left at {size:?}");
        let m = move_to(&mut h, &hs, r.max - egui::vec2(0.2, 0.2));
        assert_eq!((m.x, m.y), (32767, 32767), "bottom-right at {size:?}");
        let m = move_to(&mut h, &hs, at(0.5, 0.25) + egui::vec2(0.01, 0.01));
        let px = |f: f32, n: u32| ((f * n as f32).floor() as u32).min(n - 1);
        let (ex, ey) = (abs(px(0.5, w), w), abs(px(0.25, hgt), hgt));
        assert!(
            m.x.abs_diff(ex) <= abs(2, w) && m.y.abs_diff(ey) <= abs(2, hgt),
            "centre at {size:?}: {m:?} vs ({ex}, {ey})"
        );
        // Every batch carries run and current geometry.
        for b in batches(&hs) {
            assert_eq!(b.run_id, common::run_id(1));
            assert_eq!(b.vm_id, common::vm_id(1));
            assert_eq!((b.geometry.width, b.geometry.height), (w, hgt));
        }
        // Outside the image (letterbox): no movement forwarded.
        hs.clear();
        h.hover_at(egui::pos2(r.min.x - 3.0, r.center().y));
        h.run_steps(4);
        assert!(
            events(&hs).is_empty(),
            "movement outside the image at {size:?}"
        );
    }
}

#[test]
fn keyboard_goes_as_batches_only_with_focus_and_is_released_on_focus_loss() {
    let (mut h, hs) = console_app();
    h.run_steps(4);
    press(&h, egui::Key::A, egui::Modifiers::NONE);
    h.run_steps(4);
    assert!(keys(&hs).is_empty(), "keys without focus");
    h.get_by_label("Console of ubuntu-dev").click();
    h.run_steps(4);
    h.event(egui::Event::Key {
        key: egui::Key::ShiftLeft,
        physical_key: Some(egui::Key::ShiftLeft),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    press(&h, egui::Key::Tab, egui::Modifiers::SHIFT);
    h.run_steps(4);
    assert_eq!(
        keys(&hs),
        [
            ("shift".into(), true),
            ("tab".into(), true),
            ("tab".into(), false)
        ],
        "Tab goes to the guest, not to egui navigation"
    );
    // Focus loss (click on Dashboard): Shift released.
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.run_steps(4);
    assert_eq!(keys(&hs).last(), Some(&("shift".to_owned(), false)));
    assert!(h.state().live_console().is_some_and(|c| !c.has_focus()));
}

#[test]
fn chords_toggle_full_screen_and_leave_without_reaching_the_guest() {
    let (mut h, hs) = console_app();
    h.run_steps(4);
    h.get_by_label("Console of ubuntu-dev").click();
    h.run_steps(4);
    let ca = egui::Modifiers {
        ctrl: true,
        alt: true,
        command: true,
        ..Default::default()
    };
    press(&h, egui::Key::Enter, ca);
    h.run_steps(4);
    assert!(h.state().fullscreen());
    // Side and status panels hidden; exit button available.
    assert!(
        h.query_by_role_and_label(Role::Button, "Settings")
            .is_none()
    );
    h.get_by_role_and_label(Role::Button, "Exit full screen");
    press(&h, egui::Key::Escape, ca);
    h.run_steps(4);
    assert!(!h.state().fullscreen());
    assert!(
        !h.state().live_console().unwrap().has_focus(),
        "focus released"
    );
    h.get_by_role_and_label(Role::Button, "Settings");
    assert!(
        !keys(&hs).iter().any(|(k, _)| k == "ret" || k == "esc"),
        "chord keys reached the guest: {:?}",
        keys(&hs)
    );
    // Button: on and off.
    h.get_by_role_and_label(Role::Button, "Full screen").click();
    h.run_steps(4);
    assert!(h.state().fullscreen());
    h.get_by_role_and_label(Role::Button, "Exit full screen")
        .click();
    h.run_steps(4);
    assert!(!h.state().fullscreen());
}

#[test]
fn screenshot_is_saved_as_png_through_the_save_dialog() {
    let (fake, _hs) = FakeSupervisor::new(connected(vec![vm(1, "ubuntu-dev", VmState::Running)]));
    let console = LiveConsole::synthetic(
        ConsoleTarget {
            vm_id: common::vm_id(1),
            vm_name: "ubuntu-dev".into(),
            run_id: common::run_id(1),
        },
        deterministic_frame(320, 200),
    );
    let mut app = LightVmApp::with_live_console(console);
    app.set_supervisor(fake);
    app.set_section(Section::Console);
    let dir = std::env::temp_dir().join(format!("lightvm-shot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("shot.png");
    let names = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (o2, n2) = (out.clone(), names.clone());
    app.set_save_picker(Some(Box::new(move |name: &str| {
        n2.borrow_mut().push(name.to_owned());
        Some(o2.clone())
    })));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 760.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "Screenshot").click();
    h.run_steps(4);
    let bytes = std::fs::read(&out).expect("PNG written");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert!(names.borrow()[0].starts_with("lightvm-ubuntu-dev-"));
    assert!(names.borrow()[0].ends_with(".png"));
    h.get_by_label_contains("Screenshot saved:");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_states_and_ticket_request() {
    // No running VM.
    let (fake, hs) = FakeSupervisor::new(connected(vec![vm(1, "off", VmState::Stopped)]));
    let mut app = LightVmApp::new(false);
    app.set_supervisor(fake);
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 600.0))
        .build_ui_state(|ui, app: &mut LightVmApp| app.ui(ui), app);
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "Console").click();
    h.run_steps(4);
    h.get_by_label("No console connected.");
    h.get_by_label("No VM is running. Start one from the Dashboard, then choose Console.");
    assert_eq!(h.query_all_by_role(Role::Image).count(), 0);

    // The VM starts: Dashboard "Console" requests the ticket.
    hs.update(|s| {
        s.vms[0].state = VmState::Running;
        s.vms[0].run_id = Some(common::run_id(1));
    });
    h.get_by_role_and_label(Role::Button, "Dashboard").click();
    h.run_steps(4);
    h.get_by_role_and_label(Role::Button, "Open the console of off")
        .click();
    h.run_steps(4);
    assert_eq!(h.state().section(), Section::Console);
    assert!(
        hs.sent()
            .contains(&ServiceCommand::OpenConsole(common::vm_id(1)))
    );
    h.get_by_label("Opening the console of off…");

    // Supervisor without display tickets: stated, no retry loop.
    hs.update(|s| {
        s.console = Some(ConsoleRequest {
            vm_id: common::vm_id(1),
            status: RequestStatus::Failed(Failure {
                unsupported: true,
                message: "console: unsupported".into(),
            }),
        });
    });
    hs.clear();
    h.run_steps(4);
    h.get_by_label("The console is not available.");
    h.get_by_label("This supervisor does not provide display tickets yet.");
    std::thread::sleep(std::time::Duration::from_millis(3100));
    h.run_steps(4);
    assert!(hs.sent().is_empty(), "no retry after unsupported");

    // The VM stops: stated, the console waits for the next run.
    hs.update(|s| {
        s.vms[0].state = VmState::Stopped;
        s.console = None;
    });
    h.run_steps(4);
    h.get_by_label("off is not running (stopped).");
    assert!(h.query_by_role(Role::TextInput).is_none());
}

/// A new run (restart) replaces the console: releases sent for the old run,
/// new ticket requested.
#[test]
fn new_run_reconnects() {
    let (mut h, hs) = console_app();
    h.run_steps(4);
    h.get_by_label("Console of ubuntu-dev").click();
    h.run_steps(4);
    h.event(egui::Event::Key {
        key: egui::Key::ControlLeft,
        physical_key: Some(egui::Key::ControlLeft),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::CTRL,
    });
    h.run_steps(4);
    hs.clear();
    hs.update(|s| s.vms[0].run_id = Some(common::run_id(9)));
    h.run_steps(4);
    let b = batches(&hs);
    assert_eq!(b.len(), 1, "{b:?}");
    assert_eq!(b[0].run_id, common::run_id(1));
    assert_eq!(
        b[0].events,
        [InputEvent::Key(KeyAction {
            qcode: QCode::parse("ctrl").unwrap(),
            down: false
        })]
    );
    assert!(
        hs.sent()
            .contains(&ServiceCommand::OpenConsole(common::vm_id(1)))
    );
    assert!(h.state().live_console().is_none());
}
