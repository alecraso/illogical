//! tmux command lines: words, quoting, `;` between commands, and each
//! command's flags (tmux's own argument templates).
//!
//! Unlike HTM's parser, a flag's value may start with `-`, so
//! `capture-pane -S -1000` and `-E -1` mean what they say.

use std::collections::BTreeMap;

/// One command: its full name, flags (a flag without a value maps to an
/// empty string) and the arguments after them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub name: &'static str,
    pub flags: BTreeMap<char, String>,
    pub args: Vec<String>,
}

impl Cmd {
    pub fn has(&self, f: char) -> bool {
        self.flags.contains_key(&f)
    }

    pub fn get(&self, f: char) -> Option<&str> {
        self.flags.get(&f).map(String::as_str)
    }
}

/// Commands and their aliases with tmux's flag templates (`x:` takes a
/// value, `x::` an optional one).
const COMMANDS: &[(&str, &str, &str)] = &[
    ("attach-session", "attach", "c:dEf:rt:x"),
    ("break-pane", "breakp", "abdPF:n:s:t:"),
    ("capture-pane", "capturep", "ab:CeE:JMNpPqS:Tt:"),
    ("choose-tree", "", "F:f:GK:NO:rst:wZ"),
    ("clear-history", "clearhist", "Ht:"),
    ("copy-mode", "", "deHMqSs:t:u"),
    ("delete-buffer", "deleteb", "b:"),
    ("detach-client", "detach", "aE:s:t:P"),
    ("display-message", "display", "aCc:d:lINpt:F:v"),
    ("display-panes", "displayp", "bd:Nt:"),
    ("has-session", "has", "t:"),
    ("join-pane", "joinp", "bdfhvp:l:s:t:"),
    ("kill-pane", "killp", "at:"),
    ("kill-server", "", ""),
    ("kill-session", "", "aCt:"),
    ("kill-window", "killw", "at:"),
    ("last-pane", "lastp", "det:Z"),
    ("last-window", "last", "t:"),
    ("link-window", "linkw", "abdks:t:"),
    ("list-buffers", "lsb", "F:f:O:r"),
    ("list-clients", "lsc", "F:f:O:rt:"),
    ("list-commands", "lscm", "F:"),
    ("list-keys", "lsk", "1aNP:T:"),
    ("list-panes", "lsp", "aF:f:O:rst:"),
    ("list-sessions", "ls", "F:f:O:r"),
    ("list-windows", "lsw", "aF:f:O:rt:"),
    ("move-pane", "movep", "bdfhvp:l:s:t:"),
    ("move-window", "movew", "abdkrs:t:"),
    ("new-session", "new", "Ac:dDe:EF:f:n:Ps:t:x:Xy:"),
    ("new-window", "neww", "abc:de:F:kn:PSt:"),
    ("next-window", "next", "at:"),
    ("previous-window", "prev", "at:"),
    ("refresh-client", "refresh", "A:B:cC:Df:r:F:l::LRSt:U"),
    ("rename-session", "rename", "t:"),
    ("rename-window", "renamew", "t:"),
    ("resize-pane", "resizep", "DLMRTt:Ux:y:Z"),
    ("resize-window", "resizew", "aADLRt:Ux:y:"),
    ("run-shell", "run", "bd:Ct:Es:c:"),
    ("select-layout", "selectl", "Enopt:"),
    ("select-pane", "selectp", "DdegLlMmP:RT:t:UZ"),
    ("select-window", "selectw", "lnpTt:"),
    ("send-keys", "send", "c:FHKlMN:Rt:X"),
    ("server-info", "info", ""),
    ("set-buffer", "setb", "ab:t:n:w"),
    ("set-environment", "setenv", "Fhgrt:u"),
    ("set-hook", "", "agpRt:uw"),
    ("set-option", "set", "aFgopqst:uUw"),
    ("set-window-option", "setw", "aFgoqt:u"),
    ("show-buffer", "showb", "b:"),
    ("show-environment", "showenv", "hst:"),
    ("show-messages", "showmsgs", "JTt:"),
    ("show-options", "show", "AgHpqst:vw"),
    ("show-window-options", "showw", "gvt:"),
    ("source-file", "source", "t:Fnqv"),
    ("split-window", "splitw", "bc:de:fF:hIl:p:Pt:vZ"),
    ("swap-pane", "swapp", "dDs:t:UZ"),
    ("swap-window", "swapw", "ds:t:"),
    ("switch-client", "switchc", "c:EFlnO:pt:rT:Z"),
    ("unlink-window", "unlinkw", "kt:"),
    ("wait-for", "wait", "LSU"),
];

/// Every command's name and alias (`list-commands`; WezTerm looks for
/// `resize-window` there before it will resize).
pub fn names() -> impl Iterator<Item = (&'static str, &'static str)> {
    COMMANDS.iter().map(|(n, a, _)| (*n, *a))
}

/// A command name as tmux resolves it: exact name or alias, else a unique
/// prefix of a name.
fn lookup(word: &str) -> Option<(&'static str, &'static str)> {
    if let Some((n, _, t)) = COMMANDS.iter().find(|(n, a, _)| *n == word || (!a.is_empty() && *a == word)) {
        return Some((n, t));
    }
    let mut found = COMMANDS.iter().filter(|(n, _, _)| n.starts_with(word));
    match (found.next(), found.next()) {
        (Some((n, _, t)), None) => Some((n, t)),
        _ => None,
    }
}

/// Split a line into commands' words. `;` (unquoted, alone or ending a
/// word) separates commands; `\;` is a literal semicolon.
pub fn split_line(line: &str) -> Result<Vec<Vec<String>>, String> {
    let mut cmds = vec![];
    let mut words: Vec<String> = vec![];
    let mut word = String::new();
    // Whether the current word exists (an empty quoted string is a word).
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            ';' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if !words.is_empty() {
                    cmds.push(std::mem::take(&mut words));
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(n) => word.push(n),
                    None => word.push('\\'),
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(n) => word.push(n),
                        None => return Err("syntax error: unterminated quote".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => word.push('\n'),
                            Some('r') => word.push('\r'),
                            Some('t') => word.push('\t'),
                            Some('e') => word.push('\x1b'),
                            Some(n) => word.push(n),
                            None => return Err("syntax error: unterminated quote".into()),
                        },
                        Some(n) => word.push(n),
                        None => return Err("syntax error: unterminated quote".into()),
                    }
                }
            }
            // A comment, unless it's a format (`#{...}`) or inside a word.
            '#' if !in_word && chars.peek() != Some(&'{') => break,
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        cmds.push(words);
    }
    Ok(cmds)
}

/// One command's words into a [`Cmd`].
pub fn parse_cmd(words: &[String]) -> Result<Cmd, String> {
    let first = words.first().ok_or("empty command")?;
    let (name, template) = lookup(first).ok_or_else(|| format!("unknown command: {first}"))?;
    let mut flags = BTreeMap::new();
    let mut i = 1;
    while i < words.len() {
        let w = &words[i];
        if w == "--" {
            i += 1;
            break;
        }
        if !w.starts_with('-') || w.len() < 2 {
            break;
        }
        let cluster: Vec<char> = w.chars().skip(1).collect();
        let mut j = 0;
        while j < cluster.len() {
            let f = cluster[j];
            let pos = template.find(f);
            let takes = pos.is_some_and(|p| template[p + 1..].starts_with(':'));
            let optional = pos.is_some_and(|p| template[p + 1..].starts_with("::"));
            if takes {
                let rest: String = cluster[j + 1..].iter().collect();
                let value = if !rest.is_empty() {
                    rest
                } else if optional {
                    String::new()
                } else {
                    i += 1;
                    words.get(i).cloned().ok_or_else(|| format!("-{f} expects an argument"))?
                };
                flags.insert(f, value);
                break;
            }
            // Unknown flags are taken as switches rather than refused: a
            // client that sends one shouldn't be disconnected for it.
            flags.insert(f, String::new());
            j += 1;
        }
        i += 1;
    }
    Ok(Cmd { name, flags, args: words[i.min(words.len())..].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<Vec<String>> {
        split_line(line).unwrap()
    }

    #[test]
    fn splits_lists_and_quotes() {
        assert_eq!(
            words(r#"list-panes -t %0 -F '#{pane_id}' ; split-window -h -t "%0" ; list-panes"#),
            vec![
                vec!["list-panes", "-t", "%0", "-F", "#{pane_id}"],
                vec!["split-window", "-h", "-t", "%0"],
                vec!["list-panes"],
            ]
        );
        assert_eq!(words("show -v -q -t $0 @a; refresh-client -C 120,40").len(), 2);
        assert_eq!(words(r"send -lt %1 a\;b"), vec![vec!["send", "-lt", "%1", "a;b"]]);
        assert_eq!(words("list-sessions -F \"\t\""), vec![vec!["list-sessions", "-F", "\t"]]);
        assert_eq!(words("set @x ''"), vec![vec!["set", "@x", ""]]);
        assert_eq!(words("display -p #{version}"), vec![vec!["display", "-p", "#{version}"]]);
        assert_eq!(words("# a comment"), Vec::<Vec<String>>::new());
    }

    #[test]
    fn flags_take_values_that_start_with_a_dash() {
        let w = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let c = parse_cmd(&w("capture-pane -peqJN -t %0 -S -1000")).unwrap();
        assert_eq!(c.name, "capture-pane");
        assert!(c.has('p') && c.has('e') && c.has('q') && c.has('J') && c.has('N'));
        assert_eq!(c.get('S'), Some("-1000"));
        assert_eq!(c.get('t'), Some("%0"));
        let c = parse_cmd(&w("send -lt %1 echo")).unwrap();
        assert_eq!((c.name, c.get('t'), c.args.clone()), ("send-keys", Some("%1"), vec!["echo".to_owned()]));
        let c = parse_cmd(&w("refresh-client -fpause-after=0,wait-exit")).unwrap();
        assert_eq!(c.get('f'), Some("pause-after=0,wait-exit"));
        let c = parse_cmd(&w("capture-pane -p -S - -E -1 -t %2")).unwrap();
        assert_eq!((c.get('S'), c.get('E')), (Some("-"), Some("-1")));
        // Prefixes and aliases resolve as tmux's do.
        assert_eq!(parse_cmd(&w("show-option -g -v status")).unwrap().name, "show-options");
        assert_eq!(parse_cmd(&w("display -p x")).unwrap().name, "display-message");
        assert!(parse_cmd(&w("phony-command")).is_err());
        assert!(parse_cmd(&w("s")).is_err(), "ambiguous");
    }
}
