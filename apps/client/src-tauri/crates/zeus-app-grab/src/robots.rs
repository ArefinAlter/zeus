//! robots.txt (RFC 9309): groups by user agent, `Allow`/`Disallow` with `*` and `$`, longest match wins, `Allow` wins a
//! tie, `Crawl-delay` as a pacing hint. The site grabber identifies as `Zeus`; a group for `zeus` wins over `*`.

use std::time::Duration;

/// What a host's robots.txt lets the site grabber do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    rules: Vec<Rule>,
    /// Seconds between requests the site asks for, if it says.
    pub crawl_delay: Option<Duration>,
    /// True when the file said nothing was allowed (an unreachable robots.txt is treated this way).
    pub deny_all: bool,
    /// Number of rules that apply to Zeus.
    pub rule_count: usize,
    /// The file had content.
    pub present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    allow: bool,
    pattern: String,
}

/// The user agent token the site grabber answers to in robots.txt.
pub const AGENT_TOKEN: &str = "zeus";

impl Robots {
    /// Everything allowed (no robots.txt).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Nothing allowed (robots.txt could not be read because of a server error).
    pub fn deny_everything() -> Self {
        Self {
            deny_all: true,
            ..Self::default()
        }
    }

    /// Parse a robots.txt body (at most 512 KiB is read by the caller).
    pub fn parse(text: &str) -> Self {
        struct Group {
            agents: Vec<String>,
            rules: Vec<Rule>,
            delay: Option<Duration>,
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut collecting_agents = false;
        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !collecting_agents || groups.is_empty() {
                        groups.push(Group {
                            agents: Vec::new(),
                            rules: Vec::new(),
                            delay: None,
                        });
                    }
                    collecting_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    collecting_agents = false;
                    if let Some(g) = groups.last_mut() {
                        // An empty Disallow allows everything: it is no rule at all.
                        if !value.is_empty() {
                            g.rules.push(Rule {
                                allow: key == "allow",
                                pattern: value.to_owned(),
                            });
                        }
                    }
                }
                "crawl-delay" => {
                    collecting_agents = false;
                    if let Some(g) = groups.last_mut()
                        && let Ok(secs) = value.parse::<f64>()
                        && (0.0..=600.0).contains(&secs)
                    {
                        g.delay = Some(Duration::from_millis((secs * 1000.0) as u64));
                    }
                }
                _ => collecting_agents = false,
            }
        }
        // The most specific group: one naming us, else the `*` group. Groups of the same name add up.
        let named: Vec<&Group> = groups
            .iter()
            .filter(|g| {
                g.agents
                    .iter()
                    .any(|a| !a.is_empty() && a != "*" && AGENT_TOKEN.contains(a.as_str()))
            })
            .collect();
        let chosen: Vec<&Group> = if named.is_empty() {
            groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == "*"))
                .collect()
        } else {
            named
        };
        let rules: Vec<Rule> = chosen
            .iter()
            .flat_map(|g| g.rules.iter().cloned())
            .collect();
        let crawl_delay = chosen.iter().filter_map(|g| g.delay).max();
        Self {
            rule_count: rules.len(),
            present: !text.trim().is_empty(),
            rules,
            crawl_delay,
            deny_all: false,
        }
    }

    /// Whether `path_and_query` (starting with `/`) may be fetched.
    pub fn allowed(&self, path_and_query: &str) -> bool {
        if self.deny_all {
            return false;
        }
        let mut best: Option<(usize, bool)> = None;
        for rule in &self.rules {
            if matches_pattern(&rule.pattern, path_and_query) {
                let len = rule.pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, rule.allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// A robots pattern: a prefix match where `*` matches any run and a final `$` anchors the end.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let (core, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let mut p: Vec<char> = core.chars().collect();
    if !anchored {
        p.push('*');
    }
    let s: Vec<char> = path.chars().collect();
    wild(&p, &s, false)
}

/// Wildcard matching with backtracking over the last `*`; with `single`, `?` matches exactly one character.
fn wild(p: &[char], s: &[char], single: bool) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while si < s.len() {
        if pi < p.len() && ((single && p[pi] == '?') || (p[pi] != '*' && p[pi] == s[si])) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// A glob over a path: `*` any run (including `/`), `?` one character, anchored, case-insensitive. A pattern without
/// `*` or `?` is a substring test (the rule the client's include and exclude boxes have always had).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let (p, s) = (pattern.to_lowercase(), path.to_lowercase());
    if !p.contains(['*', '?']) {
        return s.contains(&p);
    }
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    wild(&p, &s, true)
}

/// True when any of `patterns` matches `path`.
pub fn any_match(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# comment\nUser-agent: *\nDisallow: /private/\nAllow: /private/public/\nCrawl-delay: 2\n\nUser-agent: badbot\nDisallow: /\n";

    #[test]
    fn star_group_applies_with_longest_match() {
        let r = Robots::parse(FILE);
        assert!(r.allowed("/site/"));
        assert!(!r.allowed("/private/secret.pdf"));
        assert!(r.allowed("/private/public/x.pdf"));
        assert_eq!(r.crawl_delay, Some(Duration::from_secs(2)));
    }

    #[test]
    fn a_group_naming_zeus_wins_over_star() {
        let r = Robots::parse("User-agent: *\nDisallow: /\n\nUser-agent: Zeus\nDisallow: /nope/\n");
        assert!(r.allowed("/a"));
        assert!(!r.allowed("/nope/x"));
        let r = Robots::parse("User-agent: googlebot\nDisallow: /\n");
        assert!(r.allowed("/a"), "a group for another agent is not ours");
    }

    #[test]
    fn wildcards_anchors_and_empty_disallow() {
        let r =
            Robots::parse("User-agent: *\nDisallow: /*.zip$\nDisallow: /tmp*/cache\nDisallow:\n");
        assert!(!r.allowed("/a/b.zip"));
        assert!(r.allowed("/a/b.zip?x=1"));
        assert!(!r.allowed("/tmp1/cache"));
        assert!(r.allowed("/other"));
        assert!(Robots::parse("User-agent: *\nDisallow:\n").allowed("/anything"));
    }

    #[test]
    fn allow_wins_a_tie_and_unreadable_denies() {
        let r = Robots::parse("User-agent: *\nDisallow: /x\nAllow: /x\n");
        assert!(r.allowed("/x"));
        assert!(!Robots::deny_everything().allowed("/"));
        assert!(Robots::allow_all().allowed("/"));
    }

    #[test]
    fn globs_follow_the_clients_rules() {
        assert!(glob_match("*/blog/*", "/site/blog/post/"));
        assert!(!glob_match("*/blog/*", "/site/guide/"));
        assert!(glob_match("*draft*", "/a/Draft-1.pdf"));
        assert!(glob_match("draft", "/a/draft-1.pdf"));
        assert!(glob_match("/s?te/*.pdf", "/site/x.pdf"));
        assert!(!glob_match("/s?te/*.pdf", "/site/x.zip"));
        assert!(!glob_match("", "/anything"));
    }
}
