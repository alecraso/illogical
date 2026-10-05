//! S29 (#216): can a detached per-pane ConPTY host keep a shell running
//! while the process that talks to it (the daemon, later) goes away and a
//! new one reconnects? Windows has no fd passing, and a pseudoconsole can
//! only be resized or closed by the process that made it, so the pane's
//! host has to own it and serve it over a named pipe.
//!
//!   conpty-host host --pipe NAME [--grace S] -- CMDLINE   serve a pane
//!   conpty-host spawn --pipe NAME [--grace S] -- CMDLINE  start a host detached, then exit
//!   conpty-host attach --pipe NAME                        type into it (Ctrl-] leaves)
//!   conpty-host bench echo|bulk|resize|close [--pipe NAME | --direct]
//!   conpty-host bench put --pipe NAME --text T            send T and leave (a "daemon" that goes away)
//!   conpty-host bench expect --pipe NAME --send S --want W
//!   conpty-host echo                                      raw stdin -> stdout (the latency probe)
//!   conpty-host http-serve --pipe NAME --tcp ADDR          axum on a named pipe and on TCP
//!   conpty-host http-bench --pipe NAME --tcp ADDR          requests over each, and a duplex check
//!
//! Frames both ways: kind (u8), length (u32 LE), payload.
//! 0 data, 1 resize (cols u16, rows u16), 2 close, 3 exit (code i32).

#[cfg(windows)]
mod http;
#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    win::main();
    #[cfg(not(windows))]
    eprintln!("conpty-host is Windows only");
}
