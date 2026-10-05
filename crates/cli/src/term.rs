//! The terminal the CLI runs in: its size, raw input for `attach`, and no
//! echo for a secret. termios on Unix; the console's modes on Windows,
//! where raw means VT input (keys arrive as the escape sequences a pane
//! expects) and VT output (what the pane prints is drawn, not shown).

/// Columns and rows; 80x24 if it can't be told.
pub fn size() -> (u16, u16) {
    match ratatui::crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => (c, r),
        _ => (80, 24),
    }
}

/// Raw input (and on Windows, VT output) until dropped.
pub struct Raw {
    #[cfg(unix)]
    saved: Option<nix::sys::termios::Termios>,
    #[cfg(windows)]
    saved: Option<(u32, u32)>,
}

impl Raw {
    pub fn enter() -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use nix::sys::termios::{self, SetArg};
            let stdin = std::io::stdin();
            let saved = termios::tcgetattr(&stdin).ok();
            if let Some(t) = &saved {
                let mut raw = t.clone();
                termios::cfmakeraw(&mut raw);
                termios::tcsetattr(&stdin, SetArg::TCSANOW, &raw)?;
            }
            Ok(Self { saved })
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console::{
                DISABLE_NEWLINE_AUTO_RETURN, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
                ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            };
            let (input, output) = (console_mode(Std::In), console_mode(Std::Out));
            let saved = input.zip(output);
            if saved.is_some() {
                set_console_mode(Std::In, ENABLE_VIRTUAL_TERMINAL_INPUT);
                set_console_mode(
                    Std::Out,
                    ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN,
                );
            }
            Ok(Self { saved })
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(t) = &self.saved {
            let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t);
        }
        #[cfg(windows)]
        if let Some((input, output)) = self.saved {
            set_console_mode(Std::In, input);
            set_console_mode(Std::Out, output);
        }
    }
}

/// Read a line from the terminal without echoing it.
pub fn read_hidden(line: &mut String) -> std::io::Result<usize> {
    let stdin = std::io::stdin();
    #[cfg(unix)]
    {
        use nix::sys::termios::{self, LocalFlags, SetArg};
        let saved = termios::tcgetattr(&stdin).ok();
        if let Some(t) = &saved {
            let mut quiet = t.clone();
            quiet.local_flags.remove(LocalFlags::ECHO);
            let _ = termios::tcsetattr(&stdin, SetArg::TCSANOW, &quiet);
        }
        let r = stdin.read_line(line);
        if let Some(t) = saved {
            let _ = termios::tcsetattr(&stdin, SetArg::TCSANOW, &t);
        }
        r
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::ENABLE_ECHO_INPUT;
        let saved = console_mode(Std::In);
        if let Some(m) = saved {
            set_console_mode(Std::In, m & !ENABLE_ECHO_INPUT);
        }
        let r = stdin.read_line(line);
        if let Some(m) = saved {
            set_console_mode(Std::In, m);
        }
        r
    }
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum Std {
    In,
    Out,
}

#[cfg(windows)]
fn handle(s: Std) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    // SAFETY: a plain lookup.
    unsafe { GetStdHandle(if matches!(s, Std::In) { STD_INPUT_HANDLE } else { STD_OUTPUT_HANDLE }) }
}

/// The console's mode; `None` when it isn't a console (redirected).
#[cfg(windows)]
fn console_mode(s: Std) -> Option<u32> {
    let mut m = 0u32;
    // SAFETY: our own std handle.
    (unsafe { windows_sys::Win32::System::Console::GetConsoleMode(handle(s), &mut m) } != 0).then_some(m)
}

#[cfg(windows)]
fn set_console_mode(s: Std, mode: u32) {
    // SAFETY: our own std handle.
    unsafe { windows_sys::Win32::System::Console::SetConsoleMode(handle(s), mode) };
}
