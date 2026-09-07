use winit::event::KeyEvent;
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey};

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

/// xterm modifier parameter: `1 + shift:1 + alt:2 + ctrl:4 + super:8`.
fn xterm_mod(modifiers: &ModifiersState) -> u8 {
    1 + u8::from(modifiers.shift_key())
        + 2 * u8::from(modifiers.alt_key())
        + 4 * u8::from(modifiers.control_key())
        + 8 * u8::from(modifiers.super_key())
}

/// CSI for `~`-style keys (`PgUp/PgDn/Ins/Del/F5-F12`).
fn tilde_seq(num: u8, mod_code: u8) -> Vec<u8> {
    if mod_code == 1 {
        format!("\x1b[{num}~").into_bytes()
    } else {
        format!("\x1b[{num};{mod_code}~").into_bytes()
    }
}

/// CSI for letter-suffix keys with `1;mod` (`arrows/Home/End/F1-F4` mods).
fn mod_letter(suffix: u8, mod_code: u8) -> Vec<u8> {
    vec![0x1b, b'[', b'1', b';', b'0' + mod_code, suffix]
}

/// DECKPAM application-keypad byte for a numpad char.
fn keypad_app_byte(ch: char) -> Option<&'static [u8]> {
    match ch {
        '0' => Some(b"\x1bOp"),
        '1' => Some(b"\x1bOq"),
        '2' => Some(b"\x1bOr"),
        '3' => Some(b"\x1bOs"),
        '4' => Some(b"\x1bOt"),
        '5' => Some(b"\x1bOu"),
        '6' => Some(b"\x1bOv"),
        '7' => Some(b"\x1bOw"),
        '8' => Some(b"\x1bOx"),
        '9' => Some(b"\x1bOy"),
        '-' => Some(b"\x1bOm"),
        ',' => Some(b"\x1bOl"),
        '.' => Some(b"\x1bOn"),
        '+' => Some(b"\x1bOk"),
        '*' => Some(b"\x1bOj"),
        '/' => Some(b"\x1bOo"),
        '=' => Some(b"\x1bOX"),
        _ => None,
    }
}

/// Base bytes for `Alt+character`: prefer the shifted logical char so
/// `Alt+Shift+A` stays `ESC A`; fall back to the modifierless key when the
/// logical key is an Option-composed char (macOS `Alt+a` -> `å`).
fn alt_base_bytes(logical_key: &Key, key_without_modifiers: &Key, shift: bool) -> Option<Vec<u8>> {
    let logic_s = match logical_key {
        Key::Character(s) => Some(s.as_str()),
        _ => None,
    };
    let plain_s = match key_without_modifiers {
        Key::Character(s) => Some(s.as_str()),
        _ => None,
    };
    match (logic_s, plain_s) {
        (Some(l), Some(p)) if l == p => Some(l.as_bytes().to_vec()),
        (Some(l), _) if l.is_ascii() && !l.is_empty() => Some(l.as_bytes().to_vec()),
        (_, Some(p)) if !p.is_empty() => {
            if shift && p.len() == 1 && p.as_bytes()[0].is_ascii_lowercase() {
                Some(vec![p.as_bytes()[0].to_ascii_uppercase()])
            } else {
                Some(p.as_bytes().to_vec())
            }
        }
        (Some(l), _) if !l.is_empty() => Some(l.as_bytes().to_vec()),
        _ => None,
    }
}

/// Testable core: map logical key + text to bytes.
#[allow(clippy::too_many_arguments)]
fn map_key(
    logical_key: &Key,
    key_without_modifiers: &Key,
    text: Option<&str>,
    text_with_ctrl: Option<&str>,
    location: KeyLocation,
    pressed: bool,
    modifiers: &ModifiersState,
    cursor_app: bool,
    keypad_app: bool,
    input_config: &crate::config::InputConfig,
) -> Option<Vec<u8>> {
    if !pressed {
        return None;
    }

    // Reserved for future tab switching; never reaches the PTY.
    if is_tab_switch_shortcut(logical_key, modifiers, input_config) {
        return None;
    }
    // Super combos never reach the PTY (OS / app shortcuts),
    // except Cmd+Left/Right for beginning/end of line (all platforms).
    if modifiers.super_key() {
        let is_cmd_edit = matches!(
            logical_key,
            Key::Named(NamedKey::ArrowLeft)
                | Key::Named(NamedKey::ArrowRight)
                | Key::Named(NamedKey::Backspace)
        ) && !modifiers.alt_key()
            && !modifiers.control_key();
        if !is_cmd_edit {
            return None;
        }
    }

    let shift = modifiers.shift_key();
    let alt = modifiers.alt_key();
    let ctrl = modifiers.control_key();
    let mod_code = xterm_mod(modifiers);
    // AltGr (`Ctrl+Alt`) produces text below; it must not take the Alt prefix.
    let alt_prefix = alt && !ctrl;

    if ctrl && !alt {
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
            NamedKey::Enter => {
                if location == KeyLocation::Numpad && keypad_app && !ctrl {
                    let base = b"\x1bOM".to_vec();
                    if alt_prefix {
                        let mut out = vec![0x1b];
                        out.extend_from_slice(&base);
                        return Some(out);
                    }
                    return Some(base);
                }
                if alt_prefix {
                    return Some(vec![0x1b, b'\r']);
                }
                return Some(b"\r".to_vec());
            }
            NamedKey::Backspace => {
                // Cmd+Backspace: delete to beginning of line (`Ctrl+U`).
                if modifiers.super_key() {
                    return Some(vec![0x15]);
                }
                if alt_prefix {
                    return Some(vec![0x1b, 0x7f]);
                }
                return Some(vec![0x7f]);
            }
            NamedKey::Tab => {
                if shift && !alt {
                    return Some(b"\x1b[Z".to_vec());
                }
                if alt_prefix {
                    if shift {
                        return Some(b"\x1b\x1b[Z".to_vec());
                    }
                    return Some(vec![0x1b, b'\t']);
                }
                return Some(b"\t".to_vec());
            }
            NamedKey::Escape => {
                if alt_prefix {
                    return Some(vec![0x1b, 0x1b]);
                }
                return Some(vec![0x1b]);
            }
            NamedKey::Space => {
                if alt_prefix {
                    return Some(vec![0x1b, b' ']);
                }
                return Some(b" ".to_vec());
            }
            NamedKey::ArrowUp => {
                if mod_code == 1 {
                    if cursor_app {
                        return Some(b"\x1bOA".to_vec());
                    }
                    return Some(b"\x1b[A".to_vec());
                }
                return Some(mod_letter(b'A', mod_code));
            }
            NamedKey::ArrowDown => {
                if mod_code == 1 {
                    if cursor_app {
                        return Some(b"\x1bOB".to_vec());
                    }
                    return Some(b"\x1b[B".to_vec());
                }
                return Some(mod_letter(b'B', mod_code));
            }
            NamedKey::ArrowRight => {
                // Cmd+Right: end of line (`Ctrl+E`, like other terminals;
                // `ESC[F` is often unbound in stock zsh).
                if modifiers.super_key() {
                    return Some(vec![0x05]);
                }
                // Option+Right: forward word (`ESC f`).
                if mod_code == 3 {
                    return Some(b"\x1bf".to_vec());
                }
                if mod_code == 1 {
                    if cursor_app {
                        return Some(b"\x1bOC".to_vec());
                    }
                    return Some(b"\x1b[C".to_vec());
                }
                return Some(mod_letter(b'C', mod_code));
            }
            NamedKey::ArrowLeft => {
                // Cmd+Left: beginning of line (`Ctrl+A`, like other terminals;
                // `ESC[H` is often unbound in stock zsh).
                if modifiers.super_key() {
                    return Some(vec![0x01]);
                }
                // Option+Left: backward word (`ESC b`).
                if mod_code == 3 {
                    return Some(b"\x1bb".to_vec());
                }
                if mod_code == 1 {
                    if cursor_app {
                        return Some(b"\x1bOD".to_vec());
                    }
                    return Some(b"\x1b[D".to_vec());
                }
                return Some(mod_letter(b'D', mod_code));
            }
            NamedKey::Home => {
                if mod_code == 1 {
                    return Some(b"\x1b[H".to_vec());
                }
                return Some(mod_letter(b'H', mod_code));
            }
            NamedKey::End => {
                if mod_code == 1 {
                    return Some(b"\x1b[F".to_vec());
                }
                return Some(mod_letter(b'F', mod_code));
            }
            NamedKey::PageUp => return Some(tilde_seq(5, mod_code)),
            NamedKey::PageDown => return Some(tilde_seq(6, mod_code)),
            NamedKey::Insert => return Some(tilde_seq(2, mod_code)),
            NamedKey::Delete => return Some(tilde_seq(3, mod_code)),
            NamedKey::F1 => {
                if mod_code == 1 {
                    return Some(b"\x1bOP".to_vec());
                }
                return Some(mod_letter(b'P', mod_code));
            }
            NamedKey::F2 => {
                if mod_code == 1 {
                    return Some(b"\x1bOQ".to_vec());
                }
                return Some(mod_letter(b'Q', mod_code));
            }
            NamedKey::F3 => {
                if mod_code == 1 {
                    return Some(b"\x1bOR".to_vec());
                }
                return Some(mod_letter(b'R', mod_code));
            }
            NamedKey::F4 => {
                if mod_code == 1 {
                    return Some(b"\x1bOS".to_vec());
                }
                return Some(mod_letter(b'S', mod_code));
            }
            NamedKey::F5 => return Some(tilde_seq(15, mod_code)),
            NamedKey::F6 => return Some(tilde_seq(17, mod_code)),
            NamedKey::F7 => return Some(tilde_seq(18, mod_code)),
            NamedKey::F8 => return Some(tilde_seq(19, mod_code)),
            NamedKey::F9 => return Some(tilde_seq(20, mod_code)),
            NamedKey::F10 => return Some(tilde_seq(21, mod_code)),
            NamedKey::F11 => return Some(tilde_seq(23, mod_code)),
            NamedKey::F12 => return Some(tilde_seq(24, mod_code)),
            _ => {}
        },
        Key::Character(_) => {}
        Key::Unidentified(_) | Key::Dead(_) => {}
    }

    // DECKPAM: numpad digits/ops send SS3 instead of ASCII.
    if location == KeyLocation::Numpad && keypad_app && !ctrl {
        let candidate = match logical_key {
            Key::Character(s) => s.chars().next(),
            _ => None,
        }
        .or_else(|| match key_without_modifiers {
            Key::Character(s) => s.chars().next(),
            _ => None,
        })
        .or_else(|| text.and_then(|t| t.chars().next()));
        if let Some(ch) = candidate
            && let Some(seq) = keypad_app_byte(ch)
        {
            if alt_prefix {
                let mut out = vec![0x1b];
                out.extend_from_slice(seq);
                return Some(out);
            }
            return Some(seq.to_vec());
        }
    }

    // Alt+key sends ESC + base (AltGr excluded above via `!ctrl`).
    if alt_prefix {
        if let Key::Character(_) = logical_key
            && let Some(base) = alt_base_bytes(logical_key, key_without_modifiers, shift)
        {
            let mut out = vec![0x1b];
            out.extend_from_slice(&base);
            return Some(out);
        }
        // Alt + non-character without a CSI encoding has no sequence.
        if let Key::Named(_) = logical_key {
            return None;
        }
    }

    if let Some(t) = text
        && !t.is_empty()
    {
        return Some(t.as_bytes().to_vec());
    }
    None
}

/// Wrap pasted text in bracketed-paste sentinels when the mode is enabled.
/// Future paste paths (OSC 52 / clipboard) should route through here so
/// `CSI ? 2004 h` apps like vim/zsh get `ESC[200~...ESC[201~`.
#[allow(dead_code)]
pub fn wrap_bracketed_paste(text: &str, enabled: bool) -> Vec<u8> {
    if enabled {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

/// Explicit-copy shortcut, gated by [`crate::config::InputConfig`].
pub fn is_copy_shortcut(
    logical_key: &Key,
    modifiers: &ModifiersState,
    config: &crate::config::InputConfig,
) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if s.to_ascii_lowercase() != config.copy_key.to_ascii_lowercase() {
        return false;
    }
    if config.copy_super
        && modifiers.super_key()
        && !modifiers.control_key()
        && !modifiers.alt_key()
    {
        return true;
    }
    config.copy_ctrl_shift
        && modifiers.control_key()
        && modifiers.shift_key()
        && !modifiers.super_key()
        && !modifiers.alt_key()
}

/// Explicit-paste shortcut, gated by [`crate::config::InputConfig`].
pub fn is_paste_shortcut(
    logical_key: &Key,
    modifiers: &ModifiersState,
    config: &crate::config::InputConfig,
) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if s.to_ascii_lowercase() != config.paste_key.to_ascii_lowercase() {
        return false;
    }
    if config.paste_super
        && modifiers.super_key()
        && !modifiers.control_key()
        && !modifiers.alt_key()
    {
        return true;
    }
    config.paste_ctrl_shift
        && modifiers.control_key()
        && modifiers.shift_key()
        && !modifiers.super_key()
        && !modifiers.alt_key()
}

/// New-tab shortcut, gated by [`crate::config::InputConfig`].
/// Shift is excluded so `Ctrl+Shift+T` stays free for a future
/// reopen-closed-tab binding. Consumed locally, never reaches the PTY.
pub fn is_new_tab_shortcut(
    logical_key: &Key,
    modifiers: &ModifiersState,
    config: &crate::config::InputConfig,
) -> bool {
    let Key::Character(s) = logical_key else {
        return false;
    };
    if s.to_ascii_lowercase() != config.new_tab_key.to_ascii_lowercase() {
        return false;
    }
    if modifiers.shift_key() || modifiers.alt_key() {
        return false;
    }
    let ctrl = config.new_tab_ctrl && modifiers.control_key() && !modifiers.super_key();
    let sup = config.new_tab_super && modifiers.super_key() && !modifiers.control_key();
    ctrl != sup
}

/// Reserved tab-switch shortcut, gated by [`crate::config::InputConfig`].
/// Consumed locally so the PTY never sees them; switching itself is a future TODO.
pub fn is_tab_switch_shortcut(
    logical_key: &Key,
    modifiers: &ModifiersState,
    config: &crate::config::InputConfig,
) -> bool {
    if *logical_key != Key::Named(NamedKey::Tab) {
        return false;
    }
    if modifiers.alt_key() {
        return false;
    }
    let ctrl = config.tab_switch_ctrl && modifiers.control_key() && !modifiers.super_key();
    let sup = config.tab_switch_super && modifiers.super_key() && !modifiers.control_key();
    ctrl != sup
}

/// Map a winit KeyEvent to bytes to send to the PTY.
pub fn key_to_bytes(
    event: &KeyEvent,
    modifiers: &ModifiersState,
    cursor_app: bool,
    keypad_app: bool,
    input_config: &crate::config::InputConfig,
) -> Option<Vec<u8>> {
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

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    let without = event.key_without_modifiers();
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    let without = event.logical_key.clone();

    map_key(
        &event.logical_key,
        &without,
        event.text.as_deref(),
        text_with_ctrl,
        event.location,
        event.state.is_pressed(),
        modifiers,
        cursor_app,
        keypad_app,
        input_config,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::InputConfig;
    use winit::event::ElementState;
    use winit::keyboard::ModifiersState;

    fn cfg() -> InputConfig {
        InputConfig::default()
    }

    fn plain(logical: &Key, text: Option<&str>, modifiers: &ModifiersState) -> Option<Vec<u8>> {
        map_key(
            logical,
            logical,
            text,
            text,
            KeyLocation::Standard,
            true,
            modifiers,
            false,
            false,
            &cfg(),
        )
    }

    #[test]
    fn enter_maps_to_cr() {
        let out = plain(
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"\r".to_vec()));
    }

    #[test]
    fn arrows_map() {
        let out = plain(
            &Key::Named(NamedKey::ArrowUp),
            None,
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn text_passthrough() {
        let out = plain(
            &Key::Character("a".into()),
            Some("a"),
            &ModifiersState::empty(),
        );
        assert_eq!(out, Some(b"a".to_vec()));
    }

    #[test]
    fn release_ignored() {
        let out = map_key(
            &Key::Character("a".into()),
            &Key::Character("a".into()),
            Some("a"),
            Some("a"),
            KeyLocation::Standard,
            false,
            &ModifiersState::empty(),
            false,
            false,
            &cfg(),
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

    #[test]
    fn bracketed_wrap_adds_sentinels_when_enabled() {
        assert_eq!(wrap_bracketed_paste("hi", false), b"hi".to_vec());
        assert_eq!(
            wrap_bracketed_paste("hi", true),
            b"\x1b[200~hi\x1b[201~".to_vec()
        );
    }

    #[test]
    fn copy_paste_shortcuts() {
        use winit::keyboard::ModifiersState;
        let cfg = cfg();
        let c: Key = Key::Character("c".into());
        let v: Key = Key::Character("v".into());
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(is_copy_shortcut(&c, &ctrl_shift, &cfg));
        assert!(!is_copy_shortcut(&v, &ctrl_shift, &cfg));
        assert!(is_paste_shortcut(&v, &ctrl_shift, &cfg));
        assert!(!is_paste_shortcut(&c, &ctrl_shift, &cfg));
        assert!(!is_copy_shortcut(&c, &ModifiersState::CONTROL, &cfg));
        let cmd = ModifiersState::SUPER;
        assert!(is_copy_shortcut(&c, &cmd, &cfg));
        assert!(is_paste_shortcut(&v, &cmd, &cfg));
    }

    #[test]
    fn new_tab_shortcut() {
        use winit::keyboard::ModifiersState;
        let cfg = cfg();
        let t: Key = Key::Character("t".into());
        let t_upper: Key = Key::Character("T".into());
        let c: Key = Key::Character("c".into());
        assert!(is_new_tab_shortcut(&t, &ModifiersState::CONTROL, &cfg));
        assert!(is_new_tab_shortcut(&t, &ModifiersState::SUPER, &cfg));
        assert!(is_new_tab_shortcut(
            &t_upper,
            &ModifiersState::CONTROL,
            &cfg
        ));
        assert!(!is_new_tab_shortcut(&c, &ModifiersState::CONTROL, &cfg));
        assert!(!is_new_tab_shortcut(&t, &ModifiersState::empty(), &cfg));
        // Shift / Alt variants stay free for future bindings.
        assert!(!is_new_tab_shortcut(
            &t,
            &(ModifiersState::CONTROL | ModifiersState::SHIFT),
            &cfg
        ));
        assert!(!is_new_tab_shortcut(
            &t,
            &(ModifiersState::CONTROL | ModifiersState::ALT),
            &cfg
        ));
        // Both modifiers at once is not a tab shortcut.
        assert!(!is_new_tab_shortcut(
            &t,
            &(ModifiersState::CONTROL | ModifiersState::SUPER),
            &cfg
        ));
    }

    #[test]
    fn f_keys_plain() {
        assert_eq!(
            plain(&Key::Named(NamedKey::F1), None, &ModifiersState::empty()),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::F5), None, &ModifiersState::empty()),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::F12), None, &ModifiersState::empty()),
            Some(b"\x1b[24~".to_vec())
        );
    }

    #[test]
    fn modified_arrows_and_home() {
        let shift = ModifiersState::SHIFT;
        let ctrl = ModifiersState::CONTROL;
        let alt = ModifiersState::ALT;
        assert_eq!(
            plain(&Key::Named(NamedKey::ArrowUp), None, &shift),
            Some(b"\x1b[1;2A".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::ArrowLeft), None, &ctrl),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::ArrowUp), None, &alt),
            Some(b"\x1b[1;3A".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::Home), None, &shift),
            Some(b"\x1b[1;2H".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::PageUp), None, &shift),
            Some(b"\x1b[5;2~".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::F1), None, &ctrl),
            Some(b"\x1b[1;5P".to_vec())
        );
        assert_eq!(
            plain(&Key::Named(NamedKey::F5), None, &shift),
            Some(b"\x1b[15;2~".to_vec())
        );
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(
            plain(
                &Key::Named(NamedKey::Tab),
                Some("\t"),
                &ModifiersState::SHIFT
            ),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn tab_switch_is_reserved() {
        let cfg = cfg();
        let tab: Key = Key::Named(NamedKey::Tab);
        assert!(is_tab_switch_shortcut(&tab, &ModifiersState::CONTROL, &cfg));
        assert!(is_tab_switch_shortcut(
            &tab,
            &(ModifiersState::CONTROL | ModifiersState::SHIFT),
            &cfg
        ));
        assert!(!is_tab_switch_shortcut(
            &tab,
            &ModifiersState::empty(),
            &cfg
        ));
        assert!(!is_tab_switch_shortcut(&tab, &ModifiersState::SHIFT, &cfg));
        assert_eq!(plain(&tab, Some("\t"), &ModifiersState::CONTROL), None);
    }

    #[test]
    fn alt_prefixes_printable() {
        let a: Key = Key::Character("a".into());
        assert_eq!(
            plain(&a, None, &ModifiersState::ALT),
            Some(vec![0x1b, b'a'])
        );
        let upper: Key = Key::Character("A".into());
        assert_eq!(
            plain(&upper, None, &ModifiersState::ALT),
            Some(vec![0x1b, b'A'])
        );
        // Option+Left/Right move by word (`ESC b/f`).
        assert_eq!(
            plain(&Key::Named(NamedKey::ArrowLeft), None, &ModifiersState::ALT),
            Some(b"\x1bb".to_vec())
        );
        assert_eq!(
            plain(
                &Key::Named(NamedKey::ArrowRight),
                None,
                &ModifiersState::ALT
            ),
            Some(b"\x1bf".to_vec())
        );
        // Alt+Enter is ESC + CR.
        assert_eq!(
            plain(
                &Key::Named(NamedKey::Enter),
                Some("\r"),
                &ModifiersState::ALT
            ),
            Some(vec![0x1b, b'\r'])
        );
    }

    #[test]
    fn alt_uses_modifierless_base_for_option_chars() {
        let composed: Key = Key::Character("å".into());
        let base: Key = Key::Character("a".into());
        let out = map_key(
            &composed,
            &base,
            None,
            None,
            KeyLocation::Standard,
            true,
            &ModifiersState::ALT,
            false,
            false,
            &cfg(),
        );
        assert_eq!(out, Some(vec![0x1b, b'a']));
    }

    #[test]
    fn cursor_app_sends_ss3_for_plain_arrows() {
        let up = Key::Named(NamedKey::ArrowUp);
        let out = map_key(
            &up,
            &up,
            None,
            None,
            KeyLocation::Standard,
            true,
            &ModifiersState::empty(),
            true,
            false,
            &cfg(),
        );
        assert_eq!(out, Some(b"\x1bOA".to_vec()));
        // Modified arrows stay CSI even in app mode.
        let out = map_key(
            &up,
            &up,
            None,
            None,
            KeyLocation::Standard,
            true,
            &ModifiersState::SHIFT,
            true,
            false,
            &cfg(),
        );
        assert_eq!(out, Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn keypad_app_sends_ss3() {
        let one: Key = Key::Character("1".into());
        let out = map_key(
            &one,
            &one,
            Some("1"),
            Some("1"),
            KeyLocation::Numpad,
            true,
            &ModifiersState::empty(),
            false,
            true,
            &cfg(),
        );
        assert_eq!(out, Some(b"\x1bOq".to_vec()));
        // Normal mode passes ASCII through.
        let out = map_key(
            &one,
            &one,
            Some("1"),
            Some("1"),
            KeyLocation::Numpad,
            true,
            &ModifiersState::empty(),
            false,
            false,
            &cfg(),
        );
        assert_eq!(out, Some(b"1".to_vec()));
        // Keypad Enter in app mode is SS3 M.
        let enter = Key::Named(NamedKey::Enter);
        let out = map_key(
            &enter,
            &enter,
            Some("\r"),
            Some("\r"),
            KeyLocation::Numpad,
            true,
            &ModifiersState::empty(),
            false,
            true,
            &cfg(),
        );
        assert_eq!(out, Some(b"\x1bOM".to_vec()));
    }

    #[test]
    fn super_is_ignored() {
        let a: Key = Key::Character("a".into());
        assert_eq!(plain(&a, Some("a"), &ModifiersState::SUPER), None);
    }

    #[test]
    fn cmd_arrows_move_to_line_edges() {
        assert_eq!(
            plain(
                &Key::Named(NamedKey::ArrowLeft),
                None,
                &ModifiersState::SUPER
            ),
            Some(vec![0x01])
        );
        assert_eq!(
            plain(
                &Key::Named(NamedKey::ArrowRight),
                None,
                &ModifiersState::SUPER
            ),
            Some(vec![0x05])
        );
        // Cmd+Up/Down stay ignored.
        assert_eq!(
            plain(&Key::Named(NamedKey::ArrowUp), None, &ModifiersState::SUPER),
            None
        );
    }

    #[test]
    fn cmd_backspace_kills_to_line_start() {
        assert_eq!(
            plain(
                &Key::Named(NamedKey::Backspace),
                Some("\x7f"),
                &ModifiersState::SUPER
            ),
            Some(vec![0x15])
        );
    }
}
