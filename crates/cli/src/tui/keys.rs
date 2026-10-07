//! Keys as the outer terminal reports them (crossterm, with the kitty
//! keyboard protocol when it has one) turned into libghostty key events, so
//! each pane's engine can encode them for the modes its program set.

use arugula_vt::key;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The menu key, Ctrl-], as `arugula attach` uses it. A terminal without
/// the kitty protocol sends it as 0x1d, which crossterm reads as Ctrl-5.
pub fn is_menu_key(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL)
        && !k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        && matches!(k.code, KeyCode::Char(']') | KeyCode::Char('5'))
}

/// A libghostty event for a key, or `None` for keys we don't send (media
/// keys, lone modifiers).
pub fn event(k: &KeyEvent) -> Option<key::Event<'static>> {
    let mut mods = key::Mods::empty();
    for (m, g) in [
        (KeyModifiers::SHIFT, key::Mods::SHIFT),
        (KeyModifiers::CONTROL, key::Mods::CTRL),
        (KeyModifiers::ALT, key::Mods::ALT),
        (KeyModifiers::SUPER, key::Mods::SUPER),
        (KeyModifiers::META, key::Mods::ALT),
    ] {
        if k.modifiers.contains(m) {
            mods |= g;
        }
    }
    let (code, text, unshifted): (key::Key, Option<String>, Option<char>) = match k.code {
        KeyCode::Char(c) => {
            let (code, base) = physical(c);
            // Text goes in as typed, before Ctrl turns it into a control
            // character; the encoder does that from the key and mods.
            (code, (!c.is_control()).then(|| c.to_string()), Some(base))
        }
        KeyCode::Enter => (key::Key::Enter, None, None),
        KeyCode::Tab => (key::Key::Tab, None, None),
        KeyCode::BackTab => {
            mods |= key::Mods::SHIFT;
            (key::Key::Tab, None, None)
        }
        KeyCode::Backspace => (key::Key::Backspace, None, None),
        KeyCode::Esc => (key::Key::Escape, None, None),
        KeyCode::Left => (key::Key::ArrowLeft, None, None),
        KeyCode::Right => (key::Key::ArrowRight, None, None),
        KeyCode::Up => (key::Key::ArrowUp, None, None),
        KeyCode::Down => (key::Key::ArrowDown, None, None),
        KeyCode::Home => (key::Key::Home, None, None),
        KeyCode::End => (key::Key::End, None, None),
        KeyCode::PageUp => (key::Key::PageUp, None, None),
        KeyCode::PageDown => (key::Key::PageDown, None, None),
        KeyCode::Insert => (key::Key::Insert, None, None),
        KeyCode::Delete => (key::Key::Delete, None, None),
        KeyCode::Menu => (key::Key::ContextMenu, None, None),
        KeyCode::KeypadBegin => (key::Key::NumpadBegin, None, None),
        KeyCode::F(n) => (function_key(n)?, None, None),
        _ => return None,
    };
    let mut e = key::Event::new().ok()?;
    e.set_action(match k.kind {
        KeyEventKind::Press => key::Action::Press,
        KeyEventKind::Repeat => key::Action::Repeat,
        KeyEventKind::Release => key::Action::Release,
    })
    .set_key(code)
    .set_mods(mods);
    if let Some(u) = unshifted {
        e.set_unshifted_codepoint(u);
        // Shift made the text (A, !): it's spent on that.
        if text.as_deref().and_then(|t| t.chars().next()).is_some_and(|c| c != u) {
            e.set_consumed_mods(key::Mods::SHIFT);
        }
    }
    e.set_utf8(text);
    Some(e)
}

fn function_key(n: u8) -> Option<key::Key> {
    use key::Key::*;
    [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24]
        .get(usize::from(n).checked_sub(1)?)
        .copied()
}

/// The key a character is on (a US layout's, which is what the kitty
/// protocol's key numbers mean), and the character without Shift.
fn physical(c: char) -> (key::Key, char) {
    use key::Key::*;
    let lower = c.to_ascii_lowercase();
    let letters = [A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z];
    if lower.is_ascii_lowercase() {
        return (letters[(lower as u8 - b'a') as usize], lower);
    }
    let digits = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    if let Some(d) = c.to_digit(10) {
        return (digits[d as usize], c);
    }
    match c {
        ' ' => (Space, ' '),
        ')' => (Digit0, '0'),
        '!' => (Digit1, '1'),
        '@' => (Digit2, '2'),
        '#' => (Digit3, '3'),
        '$' => (Digit4, '4'),
        '%' => (Digit5, '5'),
        '^' => (Digit6, '6'),
        '&' => (Digit7, '7'),
        '*' => (Digit8, '8'),
        '(' => (Digit9, '9'),
        '`' | '~' => (Backquote, '`'),
        '-' | '_' => (Minus, '-'),
        '=' | '+' => (Equal, '='),
        '[' | '{' => (BracketLeft, '['),
        ']' | '}' => (BracketRight, ']'),
        '\\' | '|' => (Backslash, '\\'),
        ';' | ':' => (Semicolon, ';'),
        '\'' | '"' => (Quote, '\''),
        ',' | '<' => (Comma, ','),
        '.' | '>' => (Period, '.'),
        '/' | '?' => (Slash, '/'),
        _ => (Unidentified, c),
    }
}

#[cfg(test)]
mod tests {
    use arugula_vt::{GhosttyEngine, VtEngine};
    use ratatui::crossterm::event::KeyEventState;

    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn sent(e: &GhosttyEngine, k: KeyEvent) -> Vec<u8> {
        e.encode_key(&event(&k).unwrap())
    }

    #[test]
    fn keys_reach_a_shell_as_they_would_from_ghostty() {
        let e = GhosttyEngine::mirror(80, 24, 10);
        assert_eq!(sent(&e, press(KeyCode::Char('a'), KeyModifiers::NONE)), b"a");
        assert_eq!(sent(&e, press(KeyCode::Char('A'), KeyModifiers::SHIFT)), b"A");
        assert_eq!(sent(&e, press(KeyCode::Char('c'), KeyModifiers::CONTROL)), b"\x03");
        assert_eq!(sent(&e, press(KeyCode::Char('b'), KeyModifiers::ALT)), b"\x1bb");
        assert_eq!(sent(&e, press(KeyCode::Char('!'), KeyModifiers::SHIFT)), b"!");
        assert_eq!(sent(&e, press(KeyCode::Char('é'), KeyModifiers::NONE)), "é".as_bytes());
        assert_eq!(sent(&e, press(KeyCode::Enter, KeyModifiers::NONE)), b"\r");
        assert_eq!(sent(&e, press(KeyCode::Backspace, KeyModifiers::NONE)), b"\x7f");
        assert_eq!(sent(&e, press(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A");
        assert_eq!(sent(&e, press(KeyCode::BackTab, KeyModifiers::SHIFT)), b"\x1b[Z");
        assert_eq!(sent(&e, press(KeyCode::F(5), KeyModifiers::NONE)), b"\x1b[15~");
    }

    #[test]
    fn kitty_programs_get_kitty_keys() {
        let mut e = GhosttyEngine::mirror(80, 24, 10);
        e.feed(b"\x1b[>1u");
        // Claude Code's newline.
        assert_eq!(sent(&e, press(KeyCode::Enter, KeyModifiers::SHIFT)), b"\x1b[13;2u");
        assert_eq!(sent(&e, press(KeyCode::Char('i'), KeyModifiers::CONTROL)), b"\x1b[105;5u");
        assert_eq!(sent(&e, press(KeyCode::Esc, KeyModifiers::NONE)), b"\x1b[27u");
    }

    #[test]
    fn the_menu_key() {
        assert!(is_menu_key(&press(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(is_menu_key(&press(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!is_menu_key(&press(KeyCode::Char(']'), KeyModifiers::NONE)));
    }
}
