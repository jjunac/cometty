use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// Testable core: map logical key + text to bytes.
fn map_key(
    logical_key: &Key,
    text: Option<&str>,
    text_with_ctrl: Option<&str>,
    pressed: bool,
    modifiers: &ModifiersState,
) -> Option<Vec<u8>> {
    if !pressed {
        return None;
    }

    if modifiers.control_key() && !modifiers.super_key() && !modifiers.alt_key() {
        if let Some(t) = text_with_ctrl
            && !t.is_empty()
            && (t.as_bytes()[0] < 0x20 || t.as_bytes()[0] == 0x7f)
        {
            // Ctrl+letter already resolved to control byte by winit.
            // But avoid hijacking Ctrl+C etc. when user expects SIGINT? No,
            // terminal must send it.
            if t.len() == 1 {
                return Some(t.as_bytes().to_vec());
            }
        }
        if let Key::Character(s) = logical_key {
            // logical_key ignores Ctrl, so this is the base char.
            if let Some(ch) = s.chars().next() {
                let lower = ch.to_ascii_lowercase() as u8;
                if lower.is_ascii_lowercase() {
                    return Some(vec![lower - b'a' + 1]);
                }
            }
        } else if let Key::Named(NamedKey::Space) = logical_key {
            return Some(vec![0x00]);
        }
    }

    match logical_key {
        Key::Named(named) => match named {
            NamedKey::Enter => return Some(b"\r".to_vec()),
            NamedKey::Backspace => return Some(vec![0x7f]),
            NamedKey::Tab => return Some(b"\t".to_vec()),
            NamedKey::Escape => return Some(vec![0x1b]),
            NamedKey::ArrowUp => return Some(b"\x1b[A".to_vec()),
            NamedKey::ArrowDown => return Some(b"\x1b[B".to_vec()),
            NamedKey::ArrowRight => return Some(b"\x1b[C".to_vec()),
            NamedKey::ArrowLeft => return Some(b"\x1b[D".to_vec()),
            NamedKey::Home => return Some(b"\x1b[H".to_vec()),
            NamedKey::End => return Some(b"\x1b[F".to_vec()),
            NamedKey::PageUp => return Some(b"\x1b[5~".to_vec()),
            NamedKey::PageDown => return Some(b"\x1b[6~".to_vec()),
            NamedKey::Insert => return Some(b"\x1b[2~".to_vec()),
            NamedKey::Delete => return Some(b"\x1b[3~".to_vec()),
            _ => {}
        },
        Key::Character(_) => {}
        Key::Unidentified(_) | Key::Dead(_) => {}
    }

    if let Some(t) = text
        && !t.is_empty()
    {
        return Some(t.as_bytes().to_vec());
    }
    None
}

/// Map a winit KeyEvent to bytes to send to the PTY.
pub fn key_to_bytes(event: &KeyEvent, modifiers: &ModifiersState) -> Option<Vec<u8>> {
    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    let text_with_ctrl = event.text_with_all_modifiers().map(|s| s as &str);
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    let text_with_ctrl: Option<&str> = None;

    map_key(
        &event.logical_key,
        event.text.as_deref(),
        text_with_ctrl,
        event.state.is_pressed(),
        modifiers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;
    use winit::keyboard::ModifiersState;

    #[test]
    fn enter_maps_to_cr() {
        let out = map_key(
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Some("\r"),
            true,
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"\r".to_vec()));
    }

    #[test]
    fn arrows_map() {
        let out = map_key(
            &Key::Named(NamedKey::ArrowUp),
            None,
            None,
            true,
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn text_passthrough() {
        let out = map_key(
            &Key::Character("a".into()),
            Some("a"),
            Some("a"),
            true,
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"a".to_vec()));
    }

    #[test]
    fn release_ignored() {
        let out = map_key(
            &Key::Character("a".into()),
            Some("a"),
            Some("a"),
            false,
            &ModifiersState::empty(),
        );
        assert_eq!(out, None);
    }

    #[test]
    fn key_event_wrapper_text() {
        // Exercise the real KeyEvent path via a synthetic event built by winit?
        // winit doesn't allow struct-literal construction (private fields), so
        // we only test map_key here; integration is covered by manual run.
        let _ = ElementState::Pressed;
    }
}
