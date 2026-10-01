//! Named keys and mouse events as the bytes a terminal would send.

use crate::api::{MouseAction, MouseButton};

/// The terminal modes that change what keys and the mouse send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM: arrows send `ESC O x` instead of `ESC [ x`.
    pub app_cursor: bool,
    /// Any of 1000/1002/1003: the program wants mouse events.
    pub mouse: bool,
    /// 1006: SGR mouse encoding.
    pub sgr_mouse: bool,
}

/// One tmux-style key name (`C-c`, `M-x`, `Up`, `Enter`, `F5`, `Space`, a
/// single character) as bytes. Anything else is sent as literal text.
pub fn key(name: &str, modes: Modes) -> Vec<u8> {
    // Modifier prefixes, any order: C- (control), M- (meta/alt), S- (shift).
    let mut rest = name;
    let (mut ctrl, mut meta, mut shift) = (false, false, false);
    while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
        match rest.as_bytes()[0] {
            b'C' => ctrl = true,
            b'M' => meta = true,
            b'S' => shift = true,
            _ => break,
        }
        rest = &rest[2..];
    }
    // Cursor and function keys carry their modifiers in the sequence, as
    // xterm sends them (`C-Up` is `CSI 1;5 A`).
    if (ctrl || meta || shift)
        && let Some(seq) = modified(rest, 1 + shift as u8 + 2 * meta as u8 + 4 * ctrl as u8)
    {
        return seq;
    }
    let mut out = match named(rest, modes, shift) {
        Some(b) => b,
        None if rest.chars().count() == 1 => {
            let c = rest.chars().next().unwrap();
            if ctrl {
                match c {
                    ' ' | '@' | '2' => vec![0],
                    'a'..='z' => vec![c as u8 - b'a' + 1],
                    'A'..='Z' => vec![c as u8 - b'A' + 1],
                    '[' | '3' => vec![0x1b],
                    '\\' | '4' => vec![0x1c],
                    ']' | '5' => vec![0x1d],
                    '^' | '6' => vec![0x1e],
                    '_' | '7' | '/' => vec![0x1f],
                    '?' | '8' => vec![0x7f],
                    _ => c.to_string().into_bytes(),
                }
            } else if shift {
                c.to_uppercase().to_string().into_bytes()
            } else {
                c.to_string().into_bytes()
            }
        }
        // Not a key name: literal text (with any prefixes as typed).
        None => return name.as_bytes().to_vec(),
    };
    if meta {
        out.insert(0, 0x1b);
    }
    out
}

/// A cursor or function key with xterm's modifier parameter.
fn modified(name: &str, m: u8) -> Option<Vec<u8>> {
    let letter = |c: char| Some(format!("\x1b[1;{m}{c}").into_bytes());
    let tilde = |n: u8| Some(format!("\x1b[{n};{m}~").into_bytes());
    match name {
        "Up" => letter('A'),
        "Down" => letter('B'),
        "Right" => letter('C'),
        "Left" => letter('D'),
        "Home" => letter('H'),
        "End" => letter('F'),
        "F1" => letter('P'),
        "F2" => letter('Q'),
        "F3" => letter('R'),
        "F4" => letter('S'),
        "Insert" | "IC" => tilde(2),
        "Delete" | "DC" => tilde(3),
        "PageUp" | "PgUp" | "PPage" => tilde(5),
        "PageDown" | "PgDn" | "NPage" => tilde(6),
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => None,
    }
}

fn named(name: &str, m: Modes, shift: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| if m.app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let s = |x: &str| x.as_bytes().to_vec();
    Some(match name {
        "Enter" | "Return" | "CR" => s("\r"),
        "Tab" if shift => s("\x1b[Z"),
        "Tab" => s("\t"),
        "BTab" => s("\x1b[Z"),
        "Escape" | "Esc" => s("\x1b"),
        "Space" => s(" "),
        "BSpace" | "Backspace" => s("\x7f"),
        "Up" => arrow(b'A'),
        "Down" => arrow(b'B'),
        "Right" => arrow(b'C'),
        "Left" => arrow(b'D'),
        "Home" => arrow(b'H'),
        "End" => arrow(b'F'),
        "PageUp" | "PgUp" | "PPage" => s("\x1b[5~"),
        "PageDown" | "PgDn" | "NPage" => s("\x1b[6~"),
        "Insert" | "IC" => s("\x1b[2~"),
        "Delete" | "DC" => s("\x1b[3~"),
        "F1" => s("\x1bOP"),
        "F2" => s("\x1bOQ"),
        "F3" => s("\x1bOR"),
        "F4" => s("\x1bOS"),
        "F5" => s("\x1b[15~"),
        "F6" => s("\x1b[17~"),
        "F7" => s("\x1b[18~"),
        "F8" => s("\x1b[19~"),
        "F9" => s("\x1b[20~"),
        "F10" => s("\x1b[21~"),
        "F11" => s("\x1b[23~"),
        "F12" => s("\x1b[24~"),
        _ => return None,
    })
}

/// A mouse event at cell (`x`, `y`), counted from 1, in the encoding the
/// program asked for. `None` if it isn't listening to the mouse.
pub fn mouse(x: u16, y: u16, button: MouseButton, action: MouseAction, m: Modes) -> Option<Vec<u8>> {
    if !m.mouse {
        return None;
    }
    let code: u16 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    let one = |press: bool, motion: bool| -> Vec<u8> {
        let b = code + if motion { 32 } else { 0 };
        if m.sgr_mouse {
            format!("\x1b[<{b};{x};{y}{}", if press { 'M' } else { 'm' }).into_bytes()
        } else {
            // X10: release is button 3; coordinates offset by 32 (and capped).
            let b = if press { b } else { 3 };
            let enc = |v: u16| (v.min(223) + 32) as u8;
            vec![0x1b, b'[', b'M', (b + 32).min(255) as u8, enc(x), enc(y)]
        }
    };
    Some(match action {
        MouseAction::Click if wheel => one(true, false),
        MouseAction::Click => [one(true, false), one(false, false)].concat(),
        MouseAction::Press => one(true, false),
        MouseAction::Release => one(false, false),
        MouseAction::Drag => one(true, true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let m = Modes::default();
        assert_eq!(key("C-c", m), b"\x03");
        assert_eq!(key("M-x", m), b"\x1bx");
        assert_eq!(key("C-M-a", m), b"\x1b\x01");
        assert_eq!(key("Up", m), b"\x1b[A");
        assert_eq!(key("Up", Modes { app_cursor: true, ..m }), b"\x1bOA");
        assert_eq!(key("Enter", m), b"\r");
        assert_eq!(key("F5", m), b"\x1b[15~");
        assert_eq!(key("S-Tab", m), b"\x1b[Z");
        assert_eq!(key("C-Up", m), b"\x1b[1;5A");
        assert_eq!(key("M-Left", m), b"\x1b[1;3D");
        assert_eq!(key("C-S-End", m), b"\x1b[1;6F");
        assert_eq!(key("S-F5", m), b"\x1b[15;2~");
        assert_eq!(key("M-PPage", m), b"\x1b[5;3~");
        assert_eq!(key("hello", m), b"hello", "not a name: literal");
        assert_eq!(key("x", m), b"x");
    }

    #[test]
    fn mouse_encodings() {
        let off = Modes::default();
        assert_eq!(mouse(1, 1, MouseButton::Left, MouseAction::Click, off), None);
        let sgr = Modes { mouse: true, sgr_mouse: true, ..off };
        assert_eq!(mouse(10, 5, MouseButton::Left, MouseAction::Click, sgr).unwrap(), b"\x1b[<0;10;5M\x1b[<0;10;5m");
        assert_eq!(mouse(3, 4, MouseButton::WheelUp, MouseAction::Click, sgr).unwrap(), b"\x1b[<64;3;4M");
        let x10 = Modes { mouse: true, ..off };
        assert_eq!(
            mouse(1, 1, MouseButton::Right, MouseAction::Press, x10).unwrap(),
            vec![0x1b, b'[', b'M', 34, 33, 33]
        );
    }
}
