// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Headless regression coverage for the actual shared Field control.
//! Clipboard assertions observe Slint requests to an in-memory platform;
//! they do not exercise the operating system's clipboard or native windows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, Key, Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, Rgb8Pixel, SharedString};

// Only generated component accessors are exempt from the workspace doc lint.
#[allow(missing_docs)]
mod fixture {
    slint::slint! {
        import { Field } from "../ui/primitives/modal.slint";

        export component FieldFixture inherits Window {
            width: 320px;
            height: 128px;
            background: white;
            in property <string> editable-value;
            in property <string> readonly-value;
            out property <int> editable-edits;
            out property <int> readonly-edits;
            out property <int> sentinel-keys;
            out property <string> last-edit;

            Field {
                x: 16px; y: 16px; width: 288px; height: 28px;
                value: root.editable-value;
                edited(text) => {
                    root.editable-edits += 1;
                    root.last-edit = text;
                }
            }
            Field {
                x: 16px; y: 60px; width: 288px; height: 28px;
                value: root.readonly-value;
                read-only: true;
                edited(text) => { root.readonly-edits += 1; }
            }
            FocusScope {
                x: 16px; y: 104px; width: 288px; height: 16px;
                focus-on-click: false;
                key-pressed(event) => {
                    if event.text == Key.Tab { return EventResult.reject; }
                    root.sentinel-keys += 1;
                    return EventResult.accept;
                }
            }
        }
    }
}

struct FieldPlatform {
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
    copies: Rc<RefCell<Vec<String>>>,
}

impl Platform for FieldPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.now.get()
    }

    fn cursor_flash_cycle(&self) -> Duration {
        Duration::ZERO
    }

    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if clipboard == Clipboard::DefaultClipboard {
            self.copies.borrow_mut().push(text.to_owned());
        }
    }

    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        if clipboard == Clipboard::DefaultClipboard {
            self.copies.borrow().last().cloned()
        } else {
            None
        }
    }
}

fn key(window: &MinimalSoftwareWindow, text: impl Into<SharedString>) {
    let text = text.into();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text });
}

fn shortcut(window: &MinimalSoftwareWindow, text: &str) {
    window.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    key(window, text);
    window.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
}

fn click(window: &MinimalSoftwareWindow, y: f32) {
    let position = LogicalPosition::new(30.0, y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: slint::platform::PointerEventButton::Left,
    });
}

/// Advance the platform clock explicitly; no sleeps or wall-clock races.
fn readonly_pixels(window: &MinimalSoftwareWindow, now: &Cell<Duration>) -> Vec<Rgb8Pixel> {
    let mut pixels = vec![Rgb8Pixel::default(); 320 * 128];
    for _ in 0..2 {
        now.set(now.get() + Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 320);
        }));
    }
    pixels
        .chunks_exact(320)
        .skip(60)
        .take(28)
        .flat_map(|row| row[16..304].iter().copied())
        .collect()
}

fn copy_all(window: &MinimalSoftwareWindow, copies: &RefCell<Vec<String>>) {
    copies.borrow_mut().clear();
    shortcut(window, "a");
    shortcut(window, "c");
}

fn exercise_field() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let now = Rc::new(Cell::new(Duration::ZERO));
    let copies = Rc::new(RefCell::new(Vec::new()));
    slint::platform::set_platform(Box::new(FieldPlatform {
        window: window.clone(),
        now: now.clone(),
        copies: copies.clone(),
    }))
    .expect("install an isolated headless Field platform");
    let ui = fixture::FieldFixture::new().expect("create actual Field fixture");
    ui.set_editable_value("seed".into());
    ui.show().expect("show software window");
    window.set_size(PhysicalSize::new(320, 128));
    window.dispatch_event(WindowEvent::WindowActiveChanged(true));
    readonly_pixels(&window, &now);

    click(&window, 74.0);
    let blank = readonly_pixels(&window, &now);
    copy_all(&window, &copies);
    assert!(
        copies.borrow().is_empty(),
        "empty Field has no copy selection"
    );

    // The default editable control retains its typed draft during a publish.
    click(&window, 30.0);
    shortcut(&window, "a");
    for character in "typed".chars() {
        key(&window, character.to_string());
    }
    assert_eq!(ui.get_last_edit(), "typed");
    assert_eq!(ui.get_editable_edits(), 5);
    ui.set_editable_value("published replacement".into());
    readonly_pixels(&window, &now);
    copy_all(&window, &copies);
    assert_eq!(copies.borrow().as_slice(), ["typed"]);

    // One Tab skips the editable Field's invisible read-only input.
    ui.set_readonly_value("secret-A".into());
    readonly_pixels(&window, &now);
    key(&window, Key::Tab);
    copy_all(&window, &copies);
    assert_eq!(copies.borrow().as_slice(), ["secret-A"]);
    let selected = readonly_pixels(&window, &now);
    key(&window, "X");
    key(&window, Key::Backspace);
    let rejected_edit = readonly_pixels(&window, &now);
    assert_eq!(
        selected, rejected_edit,
        "read-only pixels resist real key edits"
    );
    assert_eq!(ui.get_readonly_edits(), 0);
    assert_eq!(ui.get_editable_edits(), 5);

    // Focus remains in the read-only control while the credential rotates.
    ui.set_readonly_value("rotated-secret-B".into());
    let rotated = readonly_pixels(&window, &now);
    assert_ne!(
        rotated, selected,
        "rotation updates the actual rendered text"
    );
    copy_all(&window, &copies);
    assert_eq!(copies.borrow().as_slice(), ["rotated-secret-B"]);
    ui.set_readonly_value(SharedString::new());
    let cleared = readonly_pixels(&window, &now);
    assert_eq!(
        cleared, blank,
        "revocation renders the empty focused control"
    );
    copy_all(&window, &copies);
    assert!(
        copies.borrow().is_empty(),
        "empty selection cannot recopy the old token"
    );
    assert_eq!(ui.get_readonly_edits(), 0);

    // The next Tab skips the read-only Field's invisible editable input;
    // a further Tab wraps to the real editable input, without a hidden stop.
    key(&window, Key::Tab);
    key(&window, "Q");
    assert_eq!(ui.get_sentinel_keys(), 1);
    assert_eq!(ui.get_editable_edits(), 5);
    key(&window, Key::Tab);
    shortcut(&window, "a");
    key(&window, "R");
    assert_eq!(ui.get_sentinel_keys(), 1);
    assert_eq!(ui.get_editable_edits(), 6);
    assert_eq!(ui.get_readonly_edits(), 0);
    assert_eq!(ui.get_last_edit(), "R");
}

#[test]
fn actual_field_preserves_draft_and_revokes_focused_credentials() {
    // Slint's platform context is thread-local. A dedicated thread isolates
    // this fixture from other UI tests and needs only one backend installation.
    std::thread::spawn(exercise_field)
        .join()
        .expect("headless Field regression thread");
}
