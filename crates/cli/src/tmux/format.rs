//! tmux's `-F` formats: `#{name}`, `#{?cond,a,b}`, comparisons, the common
//! modifiers (`q:`, `l:`, `E:`, `T:`, `=N:`, `b:`, `d:`, `n:`, `s/a/b/:`),
//! `##` and the one-letter aliases (`#S`, `#D`, ...). Unknown variables
//! expand to nothing, as in tmux.

/// Where variables come from (a session, window and pane in context).
pub trait Vars {
    fn var(&self, name: &str) -> Option<String>;
}

pub fn expand(fmt: &str, vars: &dyn Vars) -> String {
    let mut out = String::new();
    let b = fmt.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'#' || i + 1 >= b.len() {
            let c = fmt[i..].chars().next().expect("a char");
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        match b[i + 1] {
            b'#' => {
                out.push('#');
                i += 2;
            }
            b',' => {
                out.push(',');
                i += 2;
            }
            b'}' => {
                out.push('}');
                i += 2;
            }
            b'{' => match matching_brace(fmt, i + 1) {
                Some(end) => {
                    out.push_str(&expr(&fmt[i + 2..end], vars));
                    i = end + 1;
                }
                None => {
                    out.push_str(&fmt[i..]);
                    break;
                }
            },
            c => {
                let alias = match c {
                    b'D' => Some("pane_id"),
                    b'F' => Some("window_flags"),
                    b'H' => Some("host"),
                    b'h' => Some("host_short"),
                    b'I' => Some("window_index"),
                    b'P' => Some("pane_index"),
                    b'S' => Some("session_name"),
                    b'T' => Some("pane_title"),
                    b'W' => Some("window_name"),
                    _ => None,
                };
                match alias {
                    Some(name) => out.push_str(&vars.var(name).unwrap_or_default()),
                    None => {
                        out.push('#');
                        out.push(c as char);
                    }
                }
                i += 2;
            }
        }
    }
    out
}

/// Index of the `}` closing the `{` at `open`, counting nested `#{`.
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split at top-level commas (not inside `#{...}`).
fn split_commas(s: &str) -> Vec<&str> {
    let b = s.as_bytes();
    let mut parts = vec![];
    let mut depth = 0usize;
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'#' if b.get(i + 1) == Some(&b'{') => {
                depth += 1;
                i += 2;
                continue;
            }
            // `#,` is an escaped comma.
            b'#' if b.get(i + 1) == Some(&b',') => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

fn truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

/// A condition or comparison operand: a format if it has `#`, otherwise a
/// variable name (for `?`) or a literal (for comparisons).
fn operand(s: &str, vars: &dyn Vars, name: bool) -> String {
    if s.contains('#') {
        expand(s, vars)
    } else if name {
        lookup(s, vars)
    } else {
        s.to_owned()
    }
}

fn lookup(name: &str, vars: &dyn Vars) -> String {
    vars.var(name).unwrap_or_default()
}

/// What's inside `#{...}`.
fn expr(e: &str, vars: &dyn Vars) -> String {
    if let Some(rest) = e.strip_prefix('?') {
        // `?c1,a,c2,b,...,else`.
        let parts = split_commas(rest);
        let mut k = 0;
        while k + 1 < parts.len() {
            if truthy(&operand(parts[k], vars, true)) {
                return expand(parts[k + 1], vars);
            }
            k += 2;
        }
        return if k < parts.len() { expand(parts[k], vars) } else { String::new() };
    }
    let Some((mods, rest)) = modifiers(e) else {
        return lookup(e, vars);
    };
    match mods {
        "l" => rest.to_owned(),
        "q" => quote(&value(rest, vars)),
        "E" => expand(&value(rest, vars), vars),
        // Time formats: nothing to strftime in the values we have.
        "T" => expand(&value(rest, vars), vars),
        "b" => value(rest, vars).rsplit('/').next().unwrap_or("").to_owned(),
        "d" => {
            let v = value(rest, vars);
            match v.rfind('/') {
                Some(0) => "/".into(),
                Some(i) => v[..i].to_owned(),
                None => ".".into(),
            }
        }
        "n" => value(rest, vars).chars().count().to_string(),
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "||" | "&&" | "m" => {
            let args = split_commas(rest);
            let a = args.first().map(|a| operand(a, vars, false)).unwrap_or_default();
            let b = args.get(1).map(|b| operand(b, vars, false)).unwrap_or_default();
            let r = match mods {
                "==" => a == b,
                "!=" => a != b,
                "<" => a < b,
                ">" => a > b,
                "<=" => a <= b,
                ">=" => a >= b,
                "||" => {
                    truthy(&operand(args.first().copied().unwrap_or(""), vars, true))
                        || truthy(&operand(args.get(1).copied().unwrap_or(""), vars, true))
                }
                "&&" => {
                    truthy(&operand(args.first().copied().unwrap_or(""), vars, true))
                        && truthy(&operand(args.get(1).copied().unwrap_or(""), vars, true))
                }
                // Glob matching is beyond what clients use; a substring is close.
                _ => b.contains(a.trim_matches('*')),
            };
            (r as u8).to_string()
        }
        m if m.starts_with('=') => {
            let v = value(rest, vars);
            let n: i64 = m[1..].parse().unwrap_or(0);
            let chars: Vec<char> = v.chars().collect();
            let k = n.unsigned_abs() as usize;
            if k >= chars.len() {
                v
            } else if n >= 0 {
                chars[..k].iter().collect()
            } else {
                chars[chars.len() - k..].iter().collect()
            }
        }
        m if m.starts_with("s/") => {
            let v = value(rest, vars);
            let body = &m[2..];
            let mut it = body.splitn(3, '/');
            let (from, to) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if from.is_empty() { v } else { v.replace(from, to) }
        }
        _ => value(rest, vars),
    }
}

/// The operand of a modifier: a format when it has `#`, else a variable.
fn value(s: &str, vars: &dyn Vars) -> String {
    operand(s, vars, true)
}

/// `mods:rest`, if `e` starts with modifiers. A variable name never has a
/// colon, so the first top-level `:` ends them.
fn modifiers(e: &str) -> Option<(&str, &str)> {
    if e.starts_with('@') {
        return None;
    }
    let i = e.find(':')?;
    let m = &e[..i];
    if m.contains('{') {
        return None;
    }
    // `s/a/b/` may contain ':' only after its own slashes; keep it simple.
    Some((m.trim_end_matches(';'), &e[i + 1..]))
}

/// `#{q:}`: backslash-escape what a shell would treat specially.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "|&;<>()$`\\\"'*?[# =%".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct M(HashMap<&'static str, &'static str>);
    impl Vars for M {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|v| (*v).to_owned())
        }
    }

    fn vars() -> M {
        M(HashMap::from([
            ("session_name", "s11"),
            ("window_id", "@0"),
            ("window_active", "1"),
            ("pane_id", "%3"),
            ("pane_current_path", "/home/me/src"),
            ("@affinities", "a b"),
            ("zero", "0"),
        ]))
    }

    #[test]
    fn expands_what_clients_send() {
        let v = vars();
        assert_eq!(expand("#{session_name}\t#{window_id}\t#{?window_active,1,0}", &v), "s11\t@0\t1");
        assert_eq!(expand("pane_id=#{pane_id}\tx=#{unknown}", &v), "pane_id=%3\tx=");
        assert_eq!(expand("A#{@affinities}", &v), "Aa b");
        assert_eq!(expand("#{?zero,yes,no} #{?missing,yes,no}", &v), "no no");
        assert_eq!(expand("#{?#{==:#{session_name},s11},same,diff}", &v), "same");
        assert_eq!(
            expand("#{q:pane_current_path} #{b:pane_current_path} #{d:pane_current_path}", &v),
            "/home/me/src src /home/me"
        );
        assert_eq!(expand("#{q:@affinities}", &v), "a\\ b");
        assert_eq!(expand("#{l:#{pane_id}} ## #S #D", &v), "#{pane_id} # s11 %3");
        assert_eq!(expand("#{=2:session_name} #{=-2:session_name} #{n:session_name}", &v), "s1 11 3");
        assert_eq!(expand("#{s/home/HOME/:pane_current_path}", &v), "/HOME/me/src");
        assert_eq!(expand("#{T:set-clipboard}", &v), "");
        assert_eq!(expand("#{?window_active,#{pane_id},-}", &v), "%3");
        assert_eq!(expand("#{?zero,a,missing,b,c}", &v), "c");
        assert_eq!(expand("#{&&:window_active,zero} #{||:window_active,zero}", &v), "0 1");
    }
}
