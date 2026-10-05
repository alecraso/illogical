//! What an agent in a terminal pane is doing, read off its screen (#145):
//! working, blocked on you, or idle at its prompt.
//!
//! Each agent has a small table of rules. A rule looks at one region of the
//! live screen (the window title, the last few non-empty lines, what's below
//! the last horizontal rule, or the prompt box between the last two), and
//! matches when all its conditions do. The highest-priority match wins; no
//! match means "can't tell", and the caller keeps what it knew.
//!
//! The regions are always cut from the bottom of the active screen, never
//! from where a viewer has scrolled to, so scrolling back can't change the
//! answer.
//!
//! The shape of the rules (regions, priorities, contains/any/all/not, line
//! regexes) follows herdr's agent manifests (Apache-2.0,
//! <https://github.com/herdrdev/herdr>, `src/detect/manifests/`); the
//! strings were checked against Claude Code 2.1 and Codex 0.155 screens.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Thinking, running a tool, writing.
    Working,
    /// Waiting on you: a permission prompt, a question, a trust dialog.
    Blocked,
    /// At its prompt, with nothing going on.
    Idle,
}

/// Where on the screen a rule looks.
#[derive(Debug, Clone, Copy)]
pub enum Region {
    /// The window title (OSC 0/2).
    Title,
    /// The last `n` non-empty lines of the screen.
    Bottom(usize),
    /// The lines below the last full horizontal rule (`────`).
    AfterLastRule,
    /// The lines between the last two horizontal rules: Claude Code's
    /// prompt box.
    PromptBox,
}

/// A condition on a region's text.
#[derive(Debug, Clone, Copy)]
pub enum Cond {
    /// The text contains this, ignoring case.
    Contains(&'static str),
    /// Some line of it matches this regex.
    Line(&'static str),
    Any(&'static [Cond]),
    All(&'static [Cond]),
    Not(&'static Cond),
}

/// What a blocked rule says on the card, with `{}` filled from the screen
/// when [`Detail::capture`] finds something.
#[derive(Debug, Clone, Copy)]
pub struct Detail {
    /// Start looking on the line after the first one matching this.
    pub after: Option<&'static str>,
    /// The first line from there matching this gives its first group.
    pub capture: &'static str,
    /// The headline with the capture (`{}`).
    pub with: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub id: &'static str,
    pub state: AgentState,
    pub priority: u16,
    pub region: Region,
    /// Every one of these holds.
    pub when: &'static [Cond],
    /// The headline when it's blocked.
    pub says: Option<&'static str>,
    pub detail: Option<Detail>,
}

#[derive(Debug)]
pub struct Agent {
    pub id: &'static str,
    /// What people call it ("Claude Code").
    pub name: &'static str,
    /// Program names that run it.
    pub programs: &'static [&'static str],
    pub rules: &'static [Rule],
}

/// What the screen shows, and the rule that said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub state: AgentState,
    pub rule: &'static str,
    /// For a blocked agent: what it's asking.
    pub headline: Option<String>,
}

const fn rule(id: &'static str, state: AgentState, priority: u16, region: Region, when: &'static [Cond]) -> Rule {
    Rule { id, state, priority, region, when, says: None, detail: None }
}

use AgentState::{Blocked, Idle, Working};
use Cond::{All, Any, Contains, Line, Not};
use Region::{AfterLastRule, Bottom, PromptBox, Title};

const CLAUDE: Agent = Agent {
    id: "claude",
    name: "Claude Code",
    programs: &["claude", "claude-code"],
    rules: &[
        // A dialog below the last rule that can be cancelled: a permission
        // prompt ("Do you want to proceed?" over numbered Yes/No), the
        // folder trust check, a question with options.
        Rule {
            says: Some("Claude Code is asking for permission"),
            detail: Some(Detail {
                after: Some(r"^\s*╌{8,}\s*$"),
                capture: r"^\s*(\S.*?)\s*$",
                with: "Claude Code asks to run `{}`",
            }),
            ..rule(
                "permission_prompt",
                Blocked,
                1000,
                AfterLastRule,
                &[
                    Contains("esc to cancel"),
                    Contains("do you want to proceed?"),
                    Line(r"(?i)^\s*(❯\s*)?[1-9]\.\s+(yes|no)\b"),
                ],
            )
        },
        Rule {
            says: Some("Claude Code asks whether to trust this folder"),
            ..rule(
                "trust_folder",
                Blocked,
                990,
                AfterLastRule,
                &[Contains("esc to cancel"), Contains("enter to confirm"), Contains("trust this folder")],
            )
        },
        Rule {
            says: Some("Claude Code is waiting for you to choose"),
            ..rule(
                "choice",
                Blocked,
                980,
                AfterLastRule,
                &[
                    Contains("esc to cancel"),
                    Any(&[
                        Contains("enter to confirm"),
                        Contains("enter to select"),
                        Contains("do you want to"),
                        Contains("would you like to"),
                    ]),
                ],
            )
        },
        // The busy title: a braille spinner (older releases) or a
        // half-circle.
        rule("title_spinner", Working, 975, Title, &[Line(r"^[\x{2800}-\x{28FF}\x{25D0}-\x{25D3}] ")]),
        // The footer while a turn runs, or the spinner line above the box
        // ("✽ Thinking… (12s · ↓ 300 tokens)").
        rule(
            "turn_running",
            Working,
            970,
            Bottom(12),
            &[Any(&[Line(r"(?i)esc to interrupt"), Line(r"^\s*[*·✢✳✶✻✽]\s+\S.*…\s*(\(\d+[smh]\b.*)?$")])],
        ),
        // The prompt box, with nothing in a dialog.
        rule(
            "prompt_box",
            Idle,
            950,
            PromptBox,
            &[Line(r"^\s*❯"), Not(&Contains("esc to cancel")), Not(&Contains("enter to select"))],
        ),
        rule("title_idle", Idle, 250, Title, &[Line(r"^✳ ")]),
    ],
};

/// Codex's working spinner, in its title.
const CODEX_SPINNER: &str = r"(?:^| )[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏](?: |$)";

const CODEX: Agent = Agent {
    id: "codex",
    name: "Codex",
    programs: &["codex"],
    rules: &[
        Rule {
            says: Some("Codex needs your approval"),
            detail: Some(Detail { after: None, capture: r"^\s*\$ (.+?)\s*$", with: "Codex asks to run `{}`" }),
            ..rule(
                "approval",
                Blocked,
                1100,
                Bottom(20),
                &[Any(&[
                    Contains("press enter to confirm or esc to cancel"),
                    Contains("allow command?"),
                    Contains("enter to submit answer"),
                    Contains("enter to submit all"),
                ])],
            )
        },
        Rule {
            says: Some("Codex asks whether to trust this folder"),
            ..rule(
                "trust_folder",
                Blocked,
                1090,
                Bottom(20),
                &[Contains("do you trust the contents of this directory?"), Contains("press enter to continue")],
            )
        },
        Rule {
            says: Some("Codex needs your approval"),
            detail: Some(Detail { after: None, capture: r"^\s*\$ (.+?)\s*$", with: "Codex asks to run `{}`" }),
            ..rule("title_action_required", Blocked, 1080, Title, &[Contains("action required")])
        },
        rule("title_spinner", Working, 1050, Title, &[Line(CODEX_SPINNER)]),
        // "• Working (12s • esc to interrupt)"
        rule("turn_running", Working, 900, Bottom(8), &[Line(r"\((\d+[hm] )*\d+s( • .*to interrupt)?\)")]),
        rule("prompt", Idle, 100, Bottom(4), &[Line(r"^›")]),
    ],
};

/// Agents with rules, by id.
pub static AGENTS: &[Agent] = &[CLAUDE, CODEX];

/// The agent a program name runs (`claude`, `/usr/bin/codex`), if it has
/// rules.
pub fn agent(program: &str) -> Option<&'static Agent> {
    let name = program.rsplit('/').next().unwrap_or(program);
    AGENTS.iter().find(|a| a.id == name || a.programs.contains(&name))
}

fn regex(pattern: &'static str) -> Regex {
    static CACHE: LazyLock<Mutex<HashMap<&'static str, Regex>>> = LazyLock::new(Default::default);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.entry(pattern).or_insert_with(|| Regex::new(pattern).expect("a valid rule regex")).clone()
}

/// A line that's a horizontal rule across the screen (`────────`).
fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.chars().count() >= 8 && t.chars().all(|c| c == '─')
}

fn region<'a>(region: Region, title: &'a str, lines: &'a [String]) -> Vec<&'a str> {
    match region {
        Title => vec![title],
        Bottom(n) => {
            let mut out: Vec<&str> =
                lines.iter().rev().filter(|l| !l.trim().is_empty()).take(n).map(|l| l.as_str()).collect();
            out.reverse();
            out
        }
        AfterLastRule => match lines.iter().rposition(|l| is_rule(l)) {
            Some(i) => lines[i + 1..].iter().map(|l| l.as_str()).collect(),
            None => vec![],
        },
        PromptBox => {
            let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
            match rules[..] {
                [.., a, b] => lines[a + 1..b].iter().map(|l| l.as_str()).collect(),
                _ => vec![],
            }
        }
    }
}

fn holds(cond: &Cond, lines: &[&str], lower: &str) -> bool {
    match cond {
        Contains(s) => lower.contains(&s.to_lowercase()),
        Line(p) => {
            let re = regex(p);
            lines.iter().any(|l| re.is_match(l))
        }
        Any(cs) => cs.iter().any(|c| holds(c, lines, lower)),
        All(cs) => cs.iter().all(|c| holds(c, lines, lower)),
        Not(c) => !holds(c, lines, lower),
    }
}

fn detail(d: &Detail, lines: &[String]) -> Option<String> {
    let from = match d.after {
        Some(p) => {
            let re = regex(p);
            lines.iter().position(|l| re.is_match(l))? + 1
        }
        None => 0,
    };
    let re = regex(d.capture);
    let got = lines[from..].iter().find_map(|l| re.captures(l)?.get(1).map(|m| m.as_str().to_owned()))?;
    let got: String = got.chars().take(120).collect();
    Some(d.with.replace("{}", &got))
}

impl Agent {
    /// What `lines` (the active screen, top to bottom) and `title` say this
    /// agent is doing; `None` when no rule matches.
    pub fn detect(&self, title: &str, lines: &[String]) -> Option<Detection> {
        let mut rules: Vec<&Rule> = self.rules.iter().collect();
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        rules.into_iter().find_map(|r| {
            let text = region(r.region, title, lines);
            let lower = text.join("\n").to_lowercase();
            if !r.when.iter().all(|c| holds(c, &text, &lower)) {
                return None;
            }
            let headline = (r.state == Blocked)
                .then(|| r.detail.and_then(|d| detail(&d, lines)).or_else(|| r.says.map(str::to_owned)))
                .flatten();
            Some(Detection { state: r.state, rule: r.id, headline })
        })
    }
}

/// How long after an agent starts the screen isn't believed: it draws its
/// banner and dialogs in pieces.
pub const GRACE: Duration = Duration::from_millis(1000);
/// Seeing it idle this many times...
const CONFIRMS: u8 = 3;
/// ...this far apart...
const CONFIRM_GAP: Duration = Duration::from_millis(100);
/// ...or for this long, settles it: a gap between spinner frames isn't the
/// end of a turn.
const CONFIRM_CAP: Duration = Duration::from_millis(700);

/// Turns what the screen shows, read over and over, into changes worth
/// reporting: working and blocked at once, idle once it's held.
#[derive(Debug)]
pub struct Debounce {
    started: Instant,
    shown: Option<AgentState>,
    /// Idle, seen this many times, first and last when.
    idle: Option<(u8, Instant, Instant)>,
}

impl Debounce {
    pub fn new(now: Instant) -> Self {
        Self { started: now, shown: None, idle: None }
    }

    /// The state last reported.
    pub fn shown(&self) -> Option<AgentState> {
        self.shown
    }

    /// Whether it wants another look soon (still in its grace period, or
    /// waiting to confirm idle).
    pub fn pending(&self, now: Instant) -> bool {
        now.duration_since(self.started) < GRACE || self.idle.is_some()
    }

    /// What the screen shows at `now` (`None`: can't tell). Returns the new
    /// state when it should be reported.
    pub fn see(&mut self, now: Instant, seen: Option<AgentState>) -> Option<AgentState> {
        if now.duration_since(self.started) < GRACE {
            return None;
        }
        let Some(seen) = seen else {
            self.idle = None;
            return None;
        };
        if Some(seen) == self.shown {
            self.idle = None;
            return None;
        }
        if seen == Idle && self.shown.is_some() {
            let (n, first, last) = match self.idle {
                None => (1, now, now),
                Some((n, first, last)) if now.duration_since(last) >= CONFIRM_GAP => (n + 1, first, now),
                Some(x) => x,
            };
            if n < CONFIRMS && now.duration_since(first) < CONFIRM_CAP {
                self.idle = Some((n, first, last));
                return None;
            }
        }
        self.idle = None;
        self.shown = Some(seen);
        Some(seen)
    }
}

#[cfg(test)]
mod tests;
