//! Keep the daemon's answers within what clients can draw.
//!
//! Programs ask the terminal what it supports and then use it. The daemon's
//! libghostty answers those questions, but the bytes are drawn by the
//! client's renderer, which may support less. Neovim, told by libghostty
//! that left/right margins (DECLRMM, mode 69) work, scrolls vertical splits
//! with them, and xterm.js, which lacks them, draws garbage. So replies are
//! rewritten to the client's capabilities: DECRQM answers for unsupported
//! modes become "not recognized", and the kitty keyboard reply is dropped
//! (programs read its absence, before the DA1 reply that follows, as "no").
//! Kitty graphics are turned off in the engine itself when the client can't
//! draw them ([`Capabilities::kitty_graphics`]), so a graphics query gets no
//! answer at all.

/// What the client renderer supports.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub dec_modes: &'static [u16],
    pub ansi_modes: &'static [u16],
    pub kitty_keyboard: bool,
    /// The Kitty graphics protocol. Without it the engine neither answers
    /// graphics queries nor keeps images (libghostty doesn't parse sixel,
    /// and its DA1 doesn't advertise it, either way).
    pub kitty_graphics: bool,
}

impl Capabilities {
    /// @xterm/xterm 6.0, measured by asking it DECRQM for modes 1..3000 and
    /// keeping those it reports as set, reset or permanently set.
    pub const XTERM_JS: Self = Self {
        dec_modes: &[
            1, 6, 7, 8, 9, 12, 25, 45, 47, 66, 1000, 1002, 1003, 1004, 1006, 1016, 1047, 1048, 1049, 2004, 2026,
        ],
        ansi_modes: &[4, 12, 20],
        kitty_keyboard: false,
        kitty_graphics: false,
    };

    /// Everything libghostty answers passes through unchanged.
    pub const ALL: Self = Self { dec_modes: &[], ansi_modes: &[], kitty_keyboard: true, kitty_graphics: true };

    fn unrestricted(&self) -> bool {
        self.dec_modes.is_empty() && self.ansi_modes.is_empty() && self.kitty_keyboard
    }

    /// Rewrite a batch of replies the terminal wants sent to the program.
    pub fn filter_replies(&self, replies: &[u8]) -> Vec<u8> {
        if self.unrestricted() {
            return replies.to_vec();
        }
        let mut out = Vec::with_capacity(replies.len());
        let mut i = 0;
        while i < replies.len() {
            match parse_csi(&replies[i..]) {
                Some(csi) => {
                    out.extend(self.rewrite(&csi).unwrap_or_else(|| csi.raw.to_vec()));
                    i += csi.raw.len();
                }
                None => {
                    out.push(replies[i]);
                    i += 1;
                }
            }
        }
        out
    }

    fn rewrite(&self, csi: &Csi) -> Option<Vec<u8>> {
        match (csi.private, csi.intermediate, csi.final_byte) {
            // DECRPM: CSI ? Ps ; Pm $ y (DEC) or CSI Ps ; Pm $ y (ANSI).
            (private, Some(b'$'), b'y') => {
                let mode: u16 = csi.params.first()?.parse().ok()?;
                let supported = if private { self.dec_modes } else { self.ansi_modes };
                if supported.contains(&mode) {
                    return None;
                }
                let prefix = if private { "?" } else { "" };
                Some(format!("\x1b[{prefix}{mode};0$y").into_bytes())
            }
            // Kitty keyboard flags report: CSI ? flags u.
            (true, None, b'u') if !self.kitty_keyboard => Some(Vec::new()),
            _ => None,
        }
    }
}

struct Csi<'a> {
    raw: &'a [u8],
    private: bool,
    params: Vec<&'a str>,
    intermediate: Option<u8>,
    final_byte: u8,
}

/// A CSI sequence at the start of `b`, if there is a complete one.
fn parse_csi(b: &[u8]) -> Option<Csi<'_>> {
    if !b.starts_with(b"\x1b[") {
        return None;
    }
    let mut i = 2;
    let private = b.get(i) == Some(&b'?');
    if private {
        i += 1;
    }
    let params_start = i;
    while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b';') {
        i += 1;
    }
    let params = std::str::from_utf8(&b[params_start..i]).ok()?.split(';').collect();
    let intermediate = b.get(i).copied().filter(|c| (0x20..=0x2f).contains(c));
    if intermediate.is_some() {
        i += 1;
    }
    let final_byte = *b.get(i).filter(|c| (0x40..=0x7e).contains(*c))?;
    Some(Csi { raw: &b[..=i], private, params, intermediate, final_byte })
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: Capabilities = Capabilities::XTERM_JS;

    #[test]
    fn unsupported_dec_mode_becomes_not_recognized() {
        assert_eq!(X.filter_replies(b"\x1b[?69;2$y"), b"\x1b[?69;0$y");
        assert_eq!(X.filter_replies(b"\x1b[?2027;1$y"), b"\x1b[?2027;0$y");
    }

    #[test]
    fn supported_modes_pass_through() {
        assert_eq!(X.filter_replies(b"\x1b[?2026;2$y"), b"\x1b[?2026;2$y");
        assert_eq!(X.filter_replies(b"\x1b[4;2$y"), b"\x1b[4;2$y");
        assert_eq!(X.filter_replies(b"\x1b[2;2$y"), b"\x1b[2;0$y");
    }

    #[test]
    fn kitty_keyboard_reply_dropped_others_kept() {
        let batch = b"\x1b[?69;2$y\x1b[?0u\x1b[?62;22c\x1b[0n";
        assert_eq!(X.filter_replies(batch), b"\x1b[?69;0$y\x1b[?62;22c\x1b[0n");
    }

    #[test]
    fn non_csi_replies_untouched() {
        let osc = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1bP>|arugula\x1b\\";
        assert_eq!(X.filter_replies(osc), osc);
    }

    #[test]
    fn all_is_identity() {
        let b = b"\x1b[?69;2$y\x1b[?0u";
        assert_eq!(Capabilities::ALL.filter_replies(b), b);
    }
}
