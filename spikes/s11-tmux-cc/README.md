# S11: what a tmux `-CC` front end has to speak (M5)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s11-tmux-cc/<file>`.

Run 2026-10-01 on geek. There is no Mac here, so this is iTerm2's source plus
a scripted client that replays iTerm2's commands against real tmux 3.6.
**Result: M5 is feasible on illogical's model with no protocol redesign.**

The daemon is missing three things:
- a per-session (and per-pane) string option store;
- an "active pane / active tab" notion;
- a capture that is consistent with the output stream.

Two other points need decisions:
- what to do when a tab is smaller than its tree;
- how size claims work.

Details are under "Mapping" and "Gaps".

## Setup

Sources read:

| Source | Commit | Date |
|---|---|---|
| iTerm2 `master` (`sources/tmux/*`, `PTYSession.m`, `PTYTab.m`) | `91411f53619a` | 2026-09-30 |
| tmux `master` (reports itself as `next-3.9`) | `5a820e63b72f` | 2026-09-30 |
| EternalTerminal, HTM (`src/htm`, tests, docs) | `bcc28abc39d5` | 2026-09-30 |
| WezTerm `main` | `cab25161054c` | 2026-09-29 |
| Ghostty `main` | `0081d4530929` | 2026-10-01 |
| MisterTea/ghostty `tmux-pr9-affinity-restore` | `5eb143208948` | 2026-09-16 |

Other setup:
- The tmux that ran is the installed **3.6**, on its own socket
  (`tmux -L illogical-s11`) with `-f s11.tmux.conf`. That config only sets
  `default-command` to `bash --norc` with `PS1='$ '`.
- WezTerm is not installed. Ghostty was not built. Their command lists come
  from source.
- No illogicald was touched.

Files:
- **`replay.py`**:
  - Starts `tmux -L illogical-s11 -CC new -s s11` on a pty.
  - Plays iTerm2's attach sequence verbatim, then: split, divider drag,
    window resize, typing key by key, manual pause and continue, vi left
    running, new tab and close, detach, `-CC attach -t s11`, quit vi, exit a
    pane, detach.
  - Writes `work/raw.log` (every chunk both ways, escaped, timestamped) and
    `work/transcript.txt` (line view).
- **`flow.py`**: `pause-after=1`, a 2 MB flood while the client stops reading,
  then `%pause` / `%continue`. Writes `work/flow.txt` and `work/flow-raw.log`.
- **`tmux_layout.py`**: an illogical split tree to and from a tmux layout
  string. `--tmux` checks the result against real tmux.
- The cloned sources are in `work/src/`.

```
python3 replay.py && python3 flow.py && python3 tmux_layout.py --tmux
```

## 1. What iTerm2 sends and expects

### Framing rules (TmuxGateway.m)

**Line terminator.** iTerm2 ends commands with **`\r`**, not `\n`, unless the
profile says otherwise. The daemon has to accept CR, LF and CRLF.

**Command lists.** Several commands go on one line, joined by `"; "`.
- The server must answer each one with its own `%begin`/`%end` or `%error`, in
  order.
- iTerm2 matches replies purely by FIFO. It ignores the number in `%begin`
  except to pair it with `%end`.
- An `%error` part-way through a list makes iTerm2 auto-fail the rest of that
  list.

**`%begin T N F` flags.**
- When `F & 1` is clear, the block is server-originated: the unsolicited one
  at attach.
- Real tmux sends `%begin <time> <n> 0` / `%end <time> <n> 0` as its first
  output on every `-CC` attach (transcript below). iTerm2 starts on the first
  `%end`/`%error`.

**Errors are fatal unless tolerated.** Every command carries a "tolerate
errors" flag.
- The flag is set on fire-and-forget sends, and on some commands with a
  callback.
- **An `%error` reply to any other command closes the connection with an
  alert.**
- Commands in that class include:
  - `show-window-options -g aggressive-resize`;
  - `show-option -g -v status`;
  - `list-keys`;
  - `show -v -q -t $S @iterm2_size` and `@iterm2_id`;
  - `list-sessions -F ...` and `list-windows -F ...`;
  - `display-message -p "#{socket_path},#{pid}"`;
  - `show-options -v -g set-titles`;
  - `resize-pane`, `select-layout`, `refresh-client -A '%N:pause'`.
- **M5 must never answer these with `%error`.** Answer empty instead.

**Fatal option values.** If `show-window-options -g aggressive-resize`
answers `aggressive-resize on`, iTerm2 aborts. If `list-sessions -F "\t"`
answers anything containing `_`, iTerm2 aborts as "not UTF-8".

**Notifications before attach completes.**
- Notifications (`%output` included) are **ignored until the last command of
  the first window's initial capture list has answered**.
- `%session-changed` is the exception. It is always handled, and iTerm2
  writes nothing until it has seen it.

**Unknown lines.** An unknown `%` line is logged and ignored. A line that
doesn't start with `%` outside a block disconnects (unless an advanced setting
is on).

**`%exit`.** On tmux ≥ 3.2, iTerm2 answers with an empty line. Under
`wait-exit` tmux waits for it, then writes `ESC \`.

**Inside a reply.** iTerm2 treats a `%exit` inside a reply as data on ≥ 1.9.
Otherwise it ends the reply.

### Attach sequence, in order (tmux 3.6 server)

`PTYSession -startTmuxMode` → `kickOffTmuxForRestoration`. Each `→` line is
one written line. Lines without a gap are pipelined without waiting.

```
(on DCS 1000p) → ^C                        write a bare 0x03 first, so a non-tmux
                                           shell doesn't run what follows
→ phony-command                            arrives as "\x03phony-command"; tmux:
                                           %error "parse error: unknown command: ^Cphony-command"
→ refresh-client -fpause-after=0,wait-exit  "ping", needs 3.2, error tolerated
→ show-window-options -g aggressive-resize  validateOptions (must not be "on")
→ show-option -g -v status
→ list-sessions -F "<TAB>"                 checkForUTF8 (a literal tab)
→ show-options -v -s default-terminal
→ list-keys                                parsed for key-binding overrides
→ copy-mode -q
→ display-message -p "#{version}"          guessVersion; these five are separate lists
→ show-window-options pane-border-format     (fallback probes, only matter if
→ list-windows -F "#{socket_path}"            #{version} is empty or unparseable)
→ list-windows -F "#{pid}"
→ show-options -g message-style
     ... wait for the version ...
→ refresh-client -fpause-after=120         didGuessVersion (age = pref, default 120 s)
→ display-message -p "#{socket_path},#{pid}"    $TMUX matching / locality
→ display-message -p '#{client_name}'
→ show-options -v -g set-titles
→ list-clients -t '$0' -F '#{client_name}<TAB>#{client_control_mode}'   3.6+: OSC 4 query tracker
→ refresh-client -B 'it2_1::#{T:set-clipboard}'                        3.6+: subscription
→ display-message -t '' -p '#{T:set-clipboard}'                        its first poll
→ show -v -q -t $0 @iterm2_size            openWindowsInitial
     ... wait ...
→ show -v -q -t $0 @iterm2_id; refresh-client -C 120,40; show -v -q -t $0 @hidden;
  show -v -q -t $0 @buried_indexes; show -v -q -t $0 @affinities;
  show -v -q -t $0 @per_window_settings; show -v -q -t $0 @per_tab_settings;
  show -v -q -t $0 @origins; show -v -q -t $0 @hotkeys; show -v -q -t $0 @tab_colors;
  list-sessions -F "#{session_id} #{session_name}";
  list-windows -F "#{session_name}\t#{window_id}\t#{window_name}\t#{window_width}\t#{window_height}\t#{window_layout}\t#{window_flags}\t#{?window_active,1,0}\t#{window_visible_layout}\t#{pane-border-status}"
     ... (if @iterm2_id was empty) ...
→ set -t $0 @iterm2_id "<UUID>"            also detects attaching the same session twice
→ per window, per pane depth-first, one list (the last window's list is "initial"):
  capture-pane -peqJN -t "%0" -S -1000;      history + screen, -e SGR, -J joined, -N trailing spaces
  capture-pane -peqJN -a -t "%0" -S -1000;   alternate screen (empty when not on it)
  list-panes -t "%0" -F "<state format>";
  capture-pane -p -P -C -t "%0";             pending, incomplete escape sequence, octal
  refresh-client -A '%0:continue';           only with pause mode
  show-options -v -q -p -t %0 @uservars      3.1+
→ refresh-client -C @0:120x40              each window sized to its tab (3.4+; older: resize-window)
```

The state format (TmuxStateParser) is tab-separated `key=#{key}` pairs for:
- `pane_id`
- `alternate_on`, `alternate_saved_x`, `alternate_saved_y`
- `cursor_x`, `cursor_y`
- `scroll_region_upper`, `scroll_region_lower`
- `pane_tabs`
- `cursor_flag`, `insert_flag`, `keypad_cursor_flag`, `keypad_flag`,
  `wrap_flag`
- `mouse_standard_flag`, `mouse_button_flag`, `mouse_any_flag`,
  `mouse_utf8_flag`, `mouse_sgr_flag`
- `bracket_paste_flag`
- `pane_key_mode`

`list-panes -t %N` returns every pane in the window. iTerm2 picks out its own
line by `pane_id`.

`maxHistory` is `MAX(client rows, scrollback lines)`, so 1000 by default.

### Commands sent later

| Action | Command(s), verbatim | Notes |
|---|---|---|
| Split | `list-panes -t %N -F '#{pane_id}'` ; `split-window -h -t "%N"` (`-v` for horizontal) ; `list-panes ...` again | iTerm2 "vertical" = tmux `-h`. With a custom or recycled directory, `-c "<dir>"` or `-c "#{pane_current_path}"` is appended. The new pane is taken from the `%layout-change` that follows. |
| New tab | `new-window -PF '#{window_id}' -a -t "$S:+"` [`-c ...`] | The reply `@N` is matched against the next `%window-add`. |
| `%window-add @N` (any origin) | `display -p -F <detailed list-windows format> -t @N`, then the per-pane capture list | |
| New panes in a `%layout-change` | the per-pane capture list for each pane not yet seen | |
| Divider drag | `resize-pane -L/-R/-U/-D -t "%N" <cells>` ; `list-windows -F "#{window_id} #{window_layout} #{window_flags} #{window_visible_layout} #{pane-border-status}"` | One list. |
| Window resize | `refresh-client -C @W:WxH` (≥ 3.4) or `resize-window -x W -y H -t @W`; client size `set -t $S @iterm2_size W,H` ; `refresh-client -C W,H` ; `list-windows ...` | |
| Arrange / API layout | `select-layout -t @W even-horizontal\|even-vertical\|tiled\|<layout string>` ; `list-windows ... -t "$S"` | Builds strings with tmux's checksum. |
| Zoom | `resize-pane -Z -t "%N"` | Also `-Z -t @W` to unzoom on open. |
| Typing | `send -lt %N <alnum and + / ) : , _ run>` ; `send -t %N 0xNN 0xNN ...` (other code points, 0 → `C-Space`) ; `send -H -t %N 0d 1b ...` (C0 bytes, ≥ 3.01) | Run-length grouped; one list per keystroke or paste; ≤ 1000 chars per command. Key names (`send -t %N 'C-Up'`) only on 3.2+ for modified keys, and all keys on 3.9+. |
| Focus | `select-pane -t "%N"`, `select-window -t @W` | |
| Close | `kill-pane -t "%N"`, `kill-window -t @W`, `unlink-window -k -t @W` | |
| Move / swap | `swap-pane -s "%a" -t "%b"`, `move-pane -s "%a" -t "%b" -h/-v [-b]`, `break-pane -P -F "#{window_id}" -t "%N"`, `move-window` / `link-window -s "$a:@w" -t "$b:+"` | |
| Rename | `rename-window -t @W "name"`, `rename-session -t "$S" "name"`, `select-pane -t %N -T "title"` | |
| Sessions (dashboard) | `new-session -s "name"`, `attach-session -t "$S"`, `kill-session -t "$S"` | |
| State it stores in tmux | `set -t $S @affinities\|@hidden\|@buried_indexes\|@origins\|@hotkeys\|@tab_colors\|@per_tab_settings\|@per_window_settings "..."`, `set -p -t %N @uservars "..."`, `set -g <it2 client record> "..."` / `set -gu` | Opaque strings. |
| Pause | `refresh-client -A '%N:pause'` (user pauses), `refresh-client -A '%N:continue'` (after re-capturing the pane) | |
| Misc | `clear-history -t "%N"`, `show-buffer -b bufferN`, `refresh-client -r "%N:<report>"` (3.4+: query replies), `display-message -t '<t>' -p '<fmt>'` (option monitors, when `-B` isn't valid) | |
| Detach | `detach` | Then an empty line after `%exit`. |

### Notifications iTerm2 handles

| Notification | iTerm2 does |
|---|---|
| `%output %N <octal-escaped>` | Feeds the pane. Bytes `< 0x20` and `\` are `\ooo`; everything else is raw. |
| `%extended-output %N <age-ms> ... : <data>` | The same, plus a latency monitor. Sent instead of `%output` once `pause-after` is set. |
| `%layout-change @W <layout> <visible-layout> <flags>` | Relayout; `Z` in the flags means zoomed. Unseen panes are captured. |
| `%window-add @W` | Opens a tab. |
| `%window-close @W`, `%unlinked-window-close @W` | Closes the tab. |
| `%window-renamed @W name`, `%unlinked-window-renamed` | Renames the tab. |
| `%unlinked-window-add` | Refreshes the dashboard. |
| `%session-changed $S name` | Handled even before attach completes. It enables writes and starts window opening. |
| `%session-renamed $S name`, `%sessions-changed` | Dashboard and list-sessions. |
| `%session-window-changed $S @W` | Selects the tab. |
| `%window-pane-changed @W %P` | Selects the pane. |
| `%pause %N` | Shows a "paused" banner; the user unpauses. |
| `%continue %N` | Ignored. |
| `%subscription-changed name $s @w i %p : value` | Status bar, clipboard. |
| `%client-session-changed`, `%client-detached` | The OSC query client tracker. |
| `%paste-buffer-changed bufferN` | Clipboard sync. |
| `%exit [reason]` | Disconnects. |
| `%noop`, `%pane-mode-changed` | Ignored. |
| Anything else starting with `%` (e.g. `%config-error`, `%message`, `%paste-buffer-deleted`) | Logged and ignored. |

### Version branches

The `#{version}` reply decides almost everything:
- `openbsd-*`, `next-*` and `-rc` are mapped;
- `3.5a` becomes 3.51;
- below 2.4 or unparseable, it falls back to the probes.

What each version turns on in iTerm2:
- **2.2:** UTF-8 `send` by code point.
- **2.4:** `window_visible_layout` and `#{pane-border-status}` in formats.
- **2.9:** variable window sizes (per-window `refresh-client -C`).
- **3.01:** `send -H` for C0 bytes.
- **3.1:** `capture-pane -N`, `@uservars`.
- **3.2:** pause mode (`pause-after`, `%extended-output`, `refresh-client -A`),
  `-B` subscriptions, the empty line after `%exit`, key names for modified
  keys.
- **3.4:** `refresh-client -C @W:WxH` instead of `resize-window`;
  `refresh-client -r` query replies.
- **3.5:** `pane_key_mode`.
- **3.6:** `list-clients` tracker, OSC 4/52 query handling, `set-clipboard`
  monitor.
- **3.7:** `get-clipboard` monitor.
- **3.9:** every key sent by name (code comment: "re-verify against the
  actual release").

HTM reports `3.5a`. That is a sensible target for M5 (see "Minimal set").

## 2. Real tmux transcript (tmux 3.6)

The full one is `work/transcript.txt`, 1,443 lines; `work/raw.log` has the
raw bytes. Below it is cleaned up:
- `list-keys` output and blank capture lines are dropped;
- each `%begin t n 1 … %end t n 1` is folded into `=> body` under its
  command.

```
# attach 1: tmux -CC new -s s11
< \033P1000p                                   DCS, glued to the first line, no newline
< %begin 1790860477 272 0 / %end ... 0         server-originated empty block
< %window-add @0
< %sessions-changed
< %session-changed $0 s11
< %window-renamed @0 tmux
< %output %0 \033[?2004h$                       plain %output until pause-after is set
> ^C
> phony-command                     => %error  "parse error: unknown command: \x03phony-command"
> refresh-client -fpause-after=0,wait-exit  => ok
> show-window-options -g aggressive-resize  => "aggressive-resize off"
> show-option -g -v status          => "on"
> list-sessions -F "<TAB>"          => "<TAB>"
> show-options -v -s default-terminal => "tmux-256color"
> list-keys                         => 534 bind-key lines
> copy-mode -q                      => ok (not an error)
> display-message -p "#{version}"   => "3.6"
> show-window-options pane-border-format => ""      (no -g: empty, not an error)
> list-windows -F "#{socket_path}"  => "/tmp/tmux-1000/illogical-s11"
> list-windows -F "#{pid}"          => "1380127"
> show-options -g message-style     => "message-style bg=yellow,fg=black"
> refresh-client -fpause-after=120  => ok
> display-message -p "#{socket_path},#{pid}" => "/tmp/tmux-1000/illogical-s11,1380127"
> display-message -p '#{client_name}' => "/dev/pts/62"
> show-options -v -g set-titles     => "off"
> list-clients -t '$0' -F '#{client_name}<TAB>#{client_control_mode}' => "/dev/pts/62<TAB>1"
> refresh-client -B 'it2_1::#{T:set-clipboard}' => ok
> display-message -t '' -p '#{T:set-clipboard}' => "external"
> show -v -q -t $0 @iterm2_size     => ""
> show -v -q -t $0 @iterm2_id; refresh-client -C 120,40; show ... @hidden; ... @tab_colors;
  list-sessions -F "#{session_id} #{session_name}"; list-windows -F "<detailed>"
                                    => "" x10, "$0 s11",
                                       "s11\t@0\tbash\t120\t40\taafd,120x40,0,0,0\t*\t1\taafd,120x40,0,0,0\toff"
< %layout-change @0 aafd,120x40,0,0,0 aafd,120x40,0,0,0 *     (from refresh-client -C)
< %extended-output %0 0 : \015\033[K\015$
< %subscription-changed it2_1 $0 - - - : external
> set -t $0 @iterm2_id "E552D69E-..."
> capture-pane -peqJN -t "%0" -S -1000; ...-a...; list-panes -t "%0" -F "<state>";
  capture-pane -p -P -C -t "%0"; refresh-client -A '%0:continue'; show-options -v -q -p -t %0 @uservars
                                    => "$ " + 39 blank lines; ""; state line; ""; ""; ""
     state: pane_id=%0  alternate_on=0  alternate_saved_x=4294967295  alternate_saved_y=4294967295
            cursor_x=2  cursor_y=0  scroll_region_upper=0  scroll_region_lower=39
            pane_tabs=8,16,...,112  cursor_flag=1  insert_flag=0  keypad_cursor_flag=0  keypad_flag=0
            wrap_flag=1  mouse_*_flag=0  bracket_paste_flag=   pane_key_mode=VT10x
> refresh-client -C @0:120x40       => ok;  < %layout-change @0 aafd,... *

# split: iTerm2 "split vertically"
> list-panes -t %0 -F '#{pane_id}' ; split-window -h -t "%0" ; list-panes ...
                                    => "%0"; ok; "%0\n%1"
< %window-pane-changed @0 %1
< %layout-change @0 f91d,120x40,0,0{60x40,0,0,0,59x40,61,0,1} f91d,...{...} *
< %window-renamed @0 tmux / %window-renamed @0 bash            (automatic-rename churn)
> (capture list for %1)

# drag divider 5 cells right
> resize-pane -R -t "%0" 5; list-windows -F "#{window_id} #{window_layout} ..."
                                    => ok; "@0 2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1} * 2a7e,... off"
< %layout-change @0 2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1} ... *

# window shrinks
> refresh-client -C @0:100x30       => ok
< %layout-change @0 9ceb,100x30,0,0{55x30,0,0,0,44x30,56,0,1} ... *    tmux took 10 from each side

# typing "echo hi there⏎" key by key
> send -lt %1 e   < %extended-output %1 0 : e        (one command and one echo per key)
> send -t %1 0x20 ...  > send -H -t %1 0d
< %extended-output %1 0 : \015\012\033[?2004l\015 / hi there\015\012 / \033[?2004h / $

# pause by hand
> refresh-client -A '%1:pause'      => %begin / %pause %1 / %end     (notification INSIDE the reply)
> send -lt %1 seq; send -t %1 0x20; ... send -H -t %1 0d      no output while paused
> (capture list for %1)             => history now shows "$ seq 1 3 / 1 / 2 / 3";
                                       refresh-client -A '%1:continue' => %begin / %continue %1 / %end

# vi (vim-tiny), typed into, left open
< %extended-output %0 0 : \033[?1049h\033[?1h\033=... \033[6n ... \033[>c    queries answered by tmux

# new tab, close it
> new-window -PF '#{window_id}' -a -t "$0:+"   => "@1"
< %session-window-changed $0 @1 / %window-add @1 / %window-renamed @1 ...
> display -p -F "<detailed>" -t @1  => "s11\t@1\tbash\t120\t40\taaff,120x40,0,0,2\t*\t1\t..."
> (capture list for %2) ; refresh-client -C @1:120x40
< %layout-change @0 ... -   (old window loses '*')  / %layout-change @1 aaff,... *
> kill-window -t @1
< %session-window-changed $0 @0 / %unlinked-window-close @1      (not %window-close)

# detach
> detach  => ok
< %exit
> (empty line)                       wait-exit
< \033\                              ST, no newline

# attach 2: tmux -CC attach -t s11
< \033P1000p / %begin ... 0 / %end ... 0 / %session-changed $0 s11
   (no %window-add, no %layout-change; the client lists everything itself)
> ... same sequence; @iterm2_id now answers the UUID set above ...
> capture-pane -peqJN -t "%0" ...   => shell history ("$ vi -u NONE ...")
> capture-pane -peqJN -a -t "%0"    => vi's alternate screen with SGR: "hello from s11", "\033[94m~ ...", status line
  state %0: alternate_on=1 alternate_saved_x=0 alternate_saved_y=1 cursor_x=13 cursor_y=0
> send -H -t %0 1b; send -lt %0 :q; send -t %0 0x21; send -H -t %0 0d
< %extended-output %0 0 : ... \033[?1049l ...     vi survived the detach and exits cleanly
> send -lt %0 exit; send -H -t %0 0d
< %layout-change @0 aafe,120x40,0,0,1 ... *         pane closed, %1 takes the window
```

Observations that matter for M5:
- **Reattach is lean.** tmux sends only the empty block and
  `%session-changed`. There is no replay of output from while detached; the
  client re-captures. HTM matches this.
- **`%output` vs `%extended-output`.** `%output` switches to
  `%extended-output %N <age> : ...` as soon as `pause-after` is set.
- **Notifications inside a reply.** tmux put `%pause`/`%continue` inside the
  reply to `refresh-client -A`, and iTerm2 swallows them as body text. HTM
  instead queues notifications until after `%end`. M5 should do what HTM does.
- **Window closes.** Killing the current window gave `%unlinked-window-close`,
  not `%window-close`. iTerm2, WezTerm and MT Ghostty accept both.
- **tmux answers terminal queries itself** (`\033[6n`, `\033[>c` from vi).
  illogical's daemon does too (`take_replies`), so iTerm2's
  `refresh-client -r` reports can be acknowledged and dropped.
- **A real `%pause` is rare with a single control client** (`flow.py`, 1 of 3
  runs).
  - When every attached client is a control client that is behind, tmux
    **stops reading the pane's PTY**: `server_client_check_pane_buffer` turns
    off `EV_READ`. So nothing ages past `pause-after`.
  - `%pause` only comes when output that is already buffered goes stale.
  - illogical never pauses the PTY (its log absorbs output), so it will reach
    `%pause` more often than tmux does. That is fine for iTerm2, which re-captures and
    sends `continue`.

## 3. Other clients and HTM

### HTM (MisterTea's tmux-compatible daemon in EternalTerminal)

This is the closest prior art: a non-tmux server that iTerm2, Ghostty, WezTerm,
Hyper and Windows Terminal attach to.

**Commands.**
- The command set is about the list under "Minimal set" below, plus layout
  moves, buffers and sessions.
- `-F` formats go through a **generic expander**: `#{name}`, `#{q:}`,
  `#{?c,a,b}` and `#{@opt}`. It does not pattern-match iTerm2's strings.
- Unknown variables expand to `""`.

**Shortcuts and canned replies.**

| What | HTM does |
|---|---|
| `#{version}` | Fixed at `3.5a`. |
| `#{socket_path}` | `htm` |
| `#{pid}` | `0` |
| `#{pane_index}` | Always 0. |
| Built-in options (`show -gv default-terminal`, `aggressive-resize`, …) | Ignored on set; an **empty `%end`** on show. |
| `@user` options | Stored per scope (global, session, window, pane). |
| `list-keys`, `copy-mode`, `list-clients`, `clear-history`, `phony-command` | Empty success. |
| `list-commands` | A canned reply aimed at WezTerm. |
| `capture-pane -P -C` | Empty. |
| Most of the state flags | Canned `0`. |
| `pane_tabs` | Empty. |
| `refresh-client -B` subscriptions | Not implemented. |
| `pause-after=N>0` | Never pauses; `%extended-output` age is always 0. |
| Sessions | Start at `$1`. A second attach evicts the first. |
| Notifications | Queued until after `%end`. |
| `%window-close` | Never sent; always `%unlinked-window-close`. |

**Attach behaviour.**
- On first attach HTM sends `%window-add` per window, `%sessions-changed` and
  `%session-changed`.
- On reattach it sends only `%session-changed`, the same as tmux.

**Bug to avoid.** Its argument parser drops a flag value that starts with `-`
and is longer than one character. `capture-pane -S -1000` therefore ignores
`-S`, and only the visible screen comes back. M5's parser must handle `-S -N`,
`-E -1` and `-S -`.

**Tests.**
- Unit tests check notification order after each mutation:
  - `new-window`: `%end`, `%session-window-changed`, `%window-add`;
  - `split-window`: `%end`, `%window-pane-changed`, `%layout-change`;
  - `kill-pane`: `%end`, `%layout-change`, `%window-pane-changed`.
- The GUI end-to-end tests drive the real apps through accessibility APIs,
  and compare pane dumps with real `tmux -CC`.
- There are no recorded client transcripts. The only fixture is the expected
  iTerm2 window grouping (`iterm2_tmux_cc_affinities.json`).

### WezTerm

Not installed; this is from source, `main` plus MisterTea's open PRs
#8168–#8179.

**Attach.** One command at a time:
1. `list-commands`.
2. `list-windows -F '#{session_id} #{window_id} #{window_width} #{window_height} #{window_active} #{window_name} #{window_layout} #{history_limit}' -t $S`
   - 8 fields, space-separated, **so a window name with a space breaks it**.
3. Per pane, `capture-pane -p -t %P -e -C -S -<history_limit>`.
4. `list-panes -F '<11 fields>' -t @W`.
5. `list-session`, as an end marker.

**Later commands.**
- Splits are `split-window -h/-v -t %P`, and **the split waits for
  `%window-pane-changed`**.
- Resize uses `resize-window` if `list-commands` listed it, otherwise
  `refresh-client -C`; then `resize-pane -x -y`.
- Keys go as `send-keys -H -t %P 0x1B 0x5B ...`.

**Strictness.**
- Its grammar is strict: **any unknown or blank line outside a block ends
  control mode**.
- It can't parse `%extended-output`, so never enable `pause-after` unless the
  client asks.
- There is no version probe.
- `main` can panic (`todo!()` in `try_wait`) and can't close panes.

### Ghostty

**`main`.** It parses the protocol, but window handling is a `// TODO`, so
nothing shows.

**MisterTea's `tmux-pr9-affinity-restore`** works, and is what the brief
should build. Per new pane, in order:
1. `display-message -p '#{version}'`. It must be one token.
2. `list-windows -F` with literal-tab fields including `A#{@affinities}`.
3. Four captures: `capture-pane -p -e -q [-a] [-S - -E -1] -t %N`.
4. One `list-panes` with 26 `;`-separated fields.

**Strictness and behaviour.**
- **An `%error` to the version, list-windows or list-panes query ends the
  session.**
- It verifies the layout checksum. It ignores the sizes in the layout and
  resizes with `resize-pane -x -y` itself.
- Its `send-keys -H` values are **Unicode code points**, while WezTerm's and
  tmux's are bytes.
- A line or block over 1 MiB ends the session.

## 4. Mapping onto illogical

illogical already has the tmux ID model:
- `$session`, `@tab`, `%pane`;
- one ID space for all block types;
- IDs that are never reused and survive daemon restarts (better than tmux,
  whose IDs reset with the server).

It also has whole `State` after every change, `Intent`s, and cell layouts
with one-cell dividers.

The front end (`illogical tmux -CC`) is a daemon client:
- it diffs successive `State`s into `%` notifications;
- it turns commands into `Intent` / `View` / `Pane` / input frames.

### Layout strings are derivable (`tmux_layout.py`)

`to_tmux(tree, cols, rows)`:
- ports `layout.rs::distribute()` (floor, largest remainder, one-cell
  dividers);
- writes `WxH,X,Y,ID` leaves with `{}` for `row` and `[]` for `column`;
- adds the `layout-custom.c` checksum (rotate right 1, add byte, `%04x`).

`from_tmux()` parses a string back, with weights = cell extents ÷ total.

**Results.**
- **The transcript's own strings** (`aafd,120x40,0,0,0`,
  `f91d,…{60x40…,59x40…}`, `2a7e,…{65x40…,54x40…}`, `9ceb,100x30…`) parse and
  re-derive byte for byte.
- **For two children,** illogical's split arithmetic gives the same cells as
  tmux's (120 → 60|59).
- **3,000 random trees** (2–3 children, 3 levels, 20–400 × 10–150 cells): the
  chain derive → parse → derive is identical. A tmux-side change such as a
  divider drag therefore turns into weights that reproduce exactly the cells
  tmux reported.
- **Against tmux 3.6:** 64 of 66 strings were accepted by `select-layout` and
  read back from `#{window_layout}` byte for byte. Those 64 were 6 hand-made
  cases plus the random ones that fit.
- **The 2 refusals** (`size mismatch after applying layout`) are tabs too
  small for their tree:
  - illogical's `distribute()` gives each child at least one cell and lets
    the extra overflow (layout.rs: "the last children overflow and get clipped
    by the client");
  - tmux never lets a layout exceed its window.
  - **Gap 1 below.**

Example: the `layout.rs` test tree at 81×25 is
`1280,81x25,0,0{40x25,0,0,1,40x25,41,0[40x12,41,0,2,40x12,41,13,3]}`.

### Commands

Status key:
- **direct**: maps onto an existing message;
- **front end**: answered by the front end alone;
- **GAP**: needs a daemon change.

| tmux command (as sent) | illogical | Status |
|---|---|---|
| `phony-command`, any unknown command | `%error parse error: unknown command: X` | front end |
| `refresh-client -f pause-after[=N],wait-exit,no-output,!flag` | Per-connection flags in the front end | front end |
| `refresh-client -A '%N:pause\|continue\|on\|off'` | `pause` → `Detach{panes:[N]}` + `%pause`. `continue` → the next capture re-attaches. | front end over `Attach`/`Detach` |
| `refresh-client -C W,H` | Default size for this connection and new tabs | front end |
| `refresh-client -C @W:WxH`, `resize-window -x -y -t @W` | `View{tab: W, cols, rows, zoom, claim: true}` | direct, with a sizing caveat (gap 4) |
| `refresh-client -B name:target:fmt` | Reply `%end`, then send `%subscription-changed` when the expanded value changes. Can start as "never changes". | front end |
| `refresh-client -r "%N:..."` | Drop (the daemon answers queries itself) | front end |
| `display-message -p [-t T] [-F] '<fmt>'`, `display -p -F ... -t @W` | Format expander over `State` | front end |
| `list-sessions -F`, `list-windows [-t $S] -F`, `list-panes [-t %N\|@W] [-s] -F`, `list-clients -F` | The expander over `State.sessions` / `tabs` / `panes` | front end |
| `show[-options] [-g\|-s\|-w\|-p] [-v] [-q] [-t ...] <name>` | `@user`: per-session / pane / global string store. Built-ins: canned values (`status off`, `aggressive-resize off`, `set-titles off`, `default-terminal <TERM the daemon sets>`, `pane-border-status off`). | **GAP 2** (the store) |
| `set[-option] [-g\|-p\|-u] -t ... @name "value"` | Write the store; ignore built-ins | **GAP 2** |
| `list-keys`, `copy-mode -q`, `clear-history` | Empty `%end`. `clear-history` → `PaneOp::Purge`. | front end, or direct |
| `capture-pane -p [-e] [-q] [-J] [-N] [-a] [-C] [-S n] [-E n] -t %N`, `capture-pane -p -P -C -t %N` | From a terminal at a known stream offset (gap 3). `-e` is SGR per line (`vt_text`). `-a` is the alt screen, empty if off. `-C` octal. `-P` is empty. | **GAP 3** |
| `list-panes -F "<state format>"` | Cursor, alt-screen saved cursor, scroll region, tab stops, DEC modes, `pane_key_mode` from the same terminal | GAP 3 (accessors) |
| `split-window -h\|-v [-b] [-c dir] -t %N [-P -F fmt]` | `Intent::Split{pane, edge: Right\|Bottom\|Left\|Top}`. `-c "#{pane_current_path}"` is the default (`from_pane`). | direct. `-c <arbitrary dir>` is a **gap**: Split/NewTab take no cwd |
| `new-window [-a] [-t $S:+] [-c dir] -PF '#{window_id}'` | `Intent::NewTab{session, from_pane}`, reply `@id` | direct (same `-c` gap) |
| `kill-pane -t %N` | `ClosePane` | direct |
| `kill-window -t @W`, `unlink-window -k -t @W` | `CloseTab` | direct |
| `resize-pane -L/-R/-U/-D -t %N n` | Find the nearest split along that axis, move the boundary `n` cells, `ResizeSplit{split, weights = extents/sum}` (exact, per the round trip) | direct, via conversion |
| `resize-pane -x/-y -t %N` (Ghostty, WezTerm) | The same, with absolute cells | direct, via conversion |
| `resize-pane -Z -t %N` | `View{zoom: Some(N)}`. tmux zoom is per window and shared; illogical's is the size owner's. | partial |
| `select-layout -t @W <string>` | Parse → if the tree shape matches, one `ResizeSplit` per split; if the shape differs, nothing can express it | partial, **gap** (no SetLayout intent) |
| `select-layout even-horizontal\|even-vertical\|tiled` | `even-*` = equal weights when the tree is flat; `tiled` restructures | partial, gap |
| `swap-pane -s a -t b` | `MovePane{edge: Center}` | direct |
| `move-pane`/`join-pane -s a -t b -h\|-v [-b]` | `MovePane{edge}` | direct |
| `break-pane [-P -F] -t %N` | `BreakPane` | direct |
| `move-window -s $a:@w -t $b:+` | `MoveTab` | direct |
| `link-window` | A tab in two sessions | **not expressible**; `%error` (iTerm2 tolerates it) |
| `rename-window`, `rename-session`, `new-session -s`, `kill-session` | `RenameTab`, `RenameSession`, `NewSession`, `CloseSession` | direct |
| `attach-session -t $S`, `switch-client` | Front end switches session; `%session-changed` | front end |
| `select-pane -t %N [-T title]`, `select-window -t @W` | Front-end "active" state, plus `Focus{pane}` | **GAP 5** (no shared active pane/tab) |
| `send -lt %N text`, `send -t %N 0xNN ... \| C-Space \| KeyName`, `send -H -t %N NN ...` | Input frames. `0xNN` is a code point → UTF-8 (≥ 0x80) or the key; `-H` is a raw byte; key names need tmux's key table (only 3.2+ modified keys, or 3.9+). | direct + an encoder |
| `show-buffer`, `set-buffer` | Not needed for attach | later |
| `detach`, `detach-client`, an empty line | `%exit`, wait for an empty line (`wait-exit`), `ESC \`, close | front end |

### Notifications

| `%` notification | From illogical | Status |
|---|---|---|
| `%begin/%end/%error T N F` | Front end; `F=0` for the unsolicited first block, `F=1` for replies | front end |
| `%session-changed $S name` | On attach and session switch | front end |
| `%sessions-changed`, `%session-renamed` | `State.sessions` diff | direct |
| `%window-add`, `%window-close`, `%window-renamed` | `State.tabs` diff (tab name `None` → the foreground command, as automatic-rename does) | direct |
| `%unlinked-window-*` | Tabs in other sessions; can be skipped | direct |
| `%layout-change @W L VL F` | Each `TabView` whose derived string changed. VL differs from L only while zoomed; F is `*`, `Z`, `-` | direct (gap 1 for overflow) |
| `%window-pane-changed`, `%session-window-changed` | Front-end active state (gap 5) | gap 5 |
| `%output` / `%extended-output %N age : data` | `Output` frames, octal-escaped. Age from the frame's time, or 0 like HTM. | direct |
| `%pause` / `%continue` | `Resync` (the client fell past the 512 KB window) or `-A pause` → `%pause`. `-A continue` → re-attach → `%continue`. | direct, semantics differ (gap 6) |
| `%subscription-changed` | Front end re-expands formats on `State` changes | front end |
| `%exit [reason]` | Detach, or the daemon going away | front end |
| `%pane-mode-changed`, `%paste-buffer-*`, `%client-*`, `%config-error`, `%message` | Never needed | skip |

The other way round, every illogical event has a `%` home (the M5 rule):

| illogical event | `%` notification |
|---|---|
| `Opened` / `Closed` | `%layout-change` (and `%window-add/close`) |
| `Layout{rev}` | `%layout-change` |
| `Exit` | `%layout-change`, or a dead pane |
| `Cwd`, `Prompt`, `CommandStart/End`, `Attention`, `Notify`, `Bell`, `Machine` | No `%` equivalent. Expose them as pane formats (e.g. `#{pane_current_path}`) or `%subscription-changed`. Nothing requires them. |

Non-terminal blocks are `%N` panes with no PTY. Answer `capture-pane` from
`capture --text` and drop `send-keys`; this is M5's existing plan.

### Gaps and suggested resolutions

1. **A tab smaller than its tree.**
   - tmux never emits a layout bigger than its window. iTerm2 trusts the
     sizes and Ghostty verifies them. illogical's `distribute()` overflows.
   - Fix in the core, not the front end. Either:
     - give each tab a minimum size (the sum of minimums: 1 cell per pane
       plus dividers) and clamp the tab's `cols`/`rows` to it. A too-small
       client then sees a larger window, which is tmux's behaviour; or
     - have the front end report the clamped window size in `%layout-change`.
   - The first keeps every client consistent.

2. **No option store.**
   - iTerm2 stores its tab grouping (`@affinities`), hidden tabs, tab colours
     and double-attach guard (`@iterm2_id`) as tmux user options.
     MT Ghostty and WezTerm's PRs use `@affinities` too.
   - Without a store every attach looks new: windows regroup, hidden tabs
     reappear, and two Macs can attach the same session unknowingly.
   - Add `options: BTreeMap<String, String>` to `Session` (and to panes and a
     global map), persisted with the layout. Values are opaque.

3. **Capture vs stream consistency.**
   - tmux answers `capture-pane` and then streams `%output` from exactly that
     point, because it is one thread.
   - The front end needs the same guarantee. Suggested: the front end holds a
     mirror `VtEngine` per pane that it has captured.
     - Attach with `offset: None`, feed the `Snapshot` into the mirror, then
       answer `capture-pane` (`vt_text` per line, `-J`/`-N` unwrapping, `-a`
       for the alt screen) and the state format from it.
     - Stream `Output` after the snapshot's offset.
     - Until iTerm2 has captured a pane, hold that pane's output. iTerm2
       ignores `%output` before then anyway.
   - Accessors still needed from libghostty-vt:
     - cursor x/y;
     - the alt screen's saved cursor (tmux uses `4294967295` for "none");
     - the scroll region;
     - tab stops;
     - the DEC modes (`dec_mode` exists).
   - The alternative is a daemon `capture` that returns its offset. The mirror
     keeps the daemon unchanged.

4. **Sizing, "last claim wins" vs tmux windows.**
   - tmux 3.6 with `window-size latest` gave each window the size of the
     client that last sent `refresh-client -C @W`. That is close to
     illogical's tab `owner`. (PLAN says per pane; the code sizes per tab,
     which is the right shape here.)
   - Treat `refresh-client -C @W:...` and any `send` to a pane in W as
     `View{claim: true}`. Treat the initial `refresh-client -C W,H` as
     `claim: false`.
   - **Open:** what iTerm2 does when another client owns the size and
     `%layout-change` reports a size different from its tab.
     - Smaller should draw inside the tab.
     - Larger makes iTerm2 resize its window (`adjustWindowSizeIfNeededForTabs`),
       which may fight the web client.
   - tmux's own resize shrank both panes equally (65|54 → 55|44).
     illogical's is proportional (→ 54|45). That is harmless, because
     illogical is the authority and sends its own string.

5. **No shared active pane/tab.**
   - tmux has an active pane per window and a current window per session,
     shared by all clients:
     - `window_active`, the `*` flag;
     - `%window-pane-changed`, `%session-window-changed`;
     - WezTerm waits on `%window-pane-changed` after a split.
   - Keep it per front-end connection: set it from `select-pane` /
     `select-window` and from new splits and tabs, and emit the
     notifications from it.
   - That is enough for one iTerm2. A shared notion can come later if
     web↔iTerm2 focus following is wanted.

6. **Flow control semantics differ.**
   - tmux: age-based (`pause-after` seconds). It discards a paused pane's
     output, and backpressures the PTY when only control clients are
     attached.
   - illogical: a byte window (512 KB) and never pauses the PTY.
   - Map as follows:
     - the front end ACKs `Output` once stdout (ssh) accepts the write;
     - `Resync` from the daemon becomes `%pause %N` when the client set
       `pause-after`. iTerm2 then re-captures and sends `continue`, which
       re-attaches.
     - For clients without `pause-after` (WezTerm, Ghostty), the front end has
       to resync silently: emit `ESC c` plus the snapshot as `%output`.
       Whether that is acceptable in each client is open.
   - `%extended-output` age can be 0, as HTM does.
   - Never enable `pause-after` unless the client asks: WezTerm can't parse
     `%extended-output`.

7. **`-c <dir>` on split and new-window.** iTerm2's "custom directory" profiles
   send an explicit path. `Split`/`NewTab` only take `from_pane`. Add
   `cwd: Option<String>` to both.

8. **`select-layout` with a different shape**, and `tiled`. There is no
   intent for it.
   - iTerm2 sends `select-layout` with flags 0, so **an `%error` would
     disconnect it**.
   - Reply `%end`, then emit `%layout-change` with the unchanged layout, and
     iTerm2 snaps back.
   - The API path (`setTmuxSizesFromSplitTreeNode`) would also need it. Add
     `SetTree` later if wanted.

9. **`send-keys -H` meaning.** Bytes for tmux and WezTerm, code points for MT
   Ghostty. Treat values ≤ 0xff as bytes, and UTF-8-encode values above
   0xff. The 0x80–0xff range stays ambiguous for Ghostty.

10. **Octal escaping and lines.**
    - Escape bytes `< 0x20` and `\`. Escaping 0x7f too is harmless.
    - Pass other bytes raw, as tmux does, even invalid UTF-8.
    - Keep lines under 1 MiB (Ghostty), so split large `Output` frames.
    - Never write a blank line or a non-`%` line outside a block (WezTerm).
    - Never write a raw ESC.

## Minimal set for M5

Goal: iTerm2 attached, showing tabs and splits, typing works, splits, drags
and closes round-trip.

**Front end, framing:**
- DCS `\033P1000p`;
- an empty `%begin T N 0`/`%end`;
- `%session-changed`;
- one reply block per `;`-separated command, notifications queued until
  after `%end`;
- CR/LF/CRLF input; a leading `^C` tolerated;
- `%exit` → wait for an empty line → `ESC \`.

**Report `#{version}` as `3.5a`.** That:
- keeps pause mode, per-window `refresh-client -C @W` and `-N` captures;
- skips the 3.6 OSC-query tracker and `list-clients`;
- matches what MT Ghostty, WezTerm and HTM were tested against.

**A real `-F` format expander:**
- `#{v}`, `#{?c,a,b}`, `#{@opt}`, `#{q:}`;
- unknown → `""`;
- the variables used above: session, window and pane ids, names, sizes,
  layout, visible layout, flags, active; `socket_path`, `pid`,
  `client_name`, `history_limit`, `pane_index`, `pane_left/top`,
  `pane_width/height`; the state-format keys.

**Answer with canned success** (never `%error`):
- `show(-options|-window-options) ...` for built-ins: `aggressive-resize off`,
  `status off`, `set-titles off`, `default-terminal`, `pane-border-status off`,
  and `message-style`;
- `list-keys`, `copy-mode -q`;
- `list-sessions -F "\t"` → a tab;
- `refresh-client -f ...`, `-B` (no updates), `-r`;
- `list-clients` (just itself);
- `list-commands` (the WezTerm list).

**Real commands:**
- `list-sessions/-windows/-panes -F`, `display(-message) -p [-t] [-F]`;
- `show/set` for `@` options (gap 2);
- `capture-pane` with `-p -e -q -J -N -a -C -P -S -E -t` (gap 3), and
  `list-panes` with the state format;
- `refresh-client -C W,H` and `-C @W:WxH`, `resize-window`;
- `refresh-client -A '%N:continue|pause'`;
- `split-window -h/-v -t %N`, `new-window -PF -a -t`, `kill-pane`,
  `kill-window`, `unlink-window -k`;
- `resize-pane -L/R/U/D n` and `-x/-y`;
- `send -l/-H/0xNN/C-Space`;
- `select-pane`, `select-window`;
- `rename-window`;
- `detach`.

**Notifications:** `%output`/`%extended-output`, `%layout-change`,
`%window-add`, `%window-close`, `%window-renamed`, `%window-pane-changed`,
`%session-window-changed`, `%sessions-changed`, `%pause`, `%continue`,
`%exit`.

**Second wave:**
- `resize-pane -Z`, `swap-pane`/`move-pane`/`break-pane`, `select-layout`;
- the dashboard's session commands;
- `%subscription-changed` updates, `clear-history`;
- `@uservars`.

**Daemon changes:**
- gap 2: the option store;
- gap 1: the minimum tab size;
- `cwd` on Split/NewTab;
- libghostty-vt accessors for gap 3.

Everything else is in the front end.

## Still open (needs a Mac with iTerm2)

- **The real attach sequence.** It comes from source plus replay, not
  capture. Capture it once with a logging proxy, e.g.
  `ssh host 'tee in.log | tmux -CC attach | tee out.log'`, or iTerm2's
  gateway "L" logging. Things to compare:
  - pipelining (iTerm2 writes the kickoff commands before any reply);
  - what it sends after windows open (font and `@per_window_settings`
    writes, extra `refresh-client -C` on window placement);
  - anything in `PTYTab`/`PseudoTerminal` that wasn't read.
- **Size conflicts (gap 4):** whether a `%layout-change` with a size different
  from the tab makes iTerm2 resize its window, letterbox, or loop against
  `refresh-client -C`.
- **Whether `3.5a` is the best version to claim**, vs `3.4` (fewer
  features) or `3.6` (OSC 4/52 query handling, which illogical answers in the
  daemon anyway).
- **Silent resync:** whether `ESC c` + snapshot as `%output` is acceptable in
  iTerm2 (scrollback, alt screen) for the no-`pause-after` path.
- **iTerm2 auto-pause:** whether its buffer monitor pauses panes by itself
  when `%extended-output` age stays 0.
- **Affinities and window restore across reattach** with the option store,
  checked against HTM's `iterm2_tmux_cc_affinities.json` expectations.
- **Ghostty (MT branch) and WezTerm (PR stack) end to end.** Ghostty can run
  on geek per `~/ghostty-tmux-control-mode-brief.md`. Use it against real
  tmux first, then against M5.

## Cleanup

- Every tmux server here used `-L illogical-s11`. Each script kills it at
  start and end (`tmux -L illogical-s11 kill-server`).
- `tmux -L illogical-s11 ls` now reports "no server running".
- `/tmp/s11-vim.txt` was never written (vi quit with `:q!`).
- No illogicald was contacted.
- Nothing outside `spikes/s11-tmux-cc/` was changed. Two clones initially
  landed in the repo root because of a backgrounded `cd`; they were moved
  into `work/src/` at once. `git status` shows only untracked spike
  directories.
- `work/` is git-ignored:
  - the cloned sources: about 62 MB;
  - the transcripts: about 250 KB;
  - the subagent's scratch: `htmparse/`, `prs/`.
