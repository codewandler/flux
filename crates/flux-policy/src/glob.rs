//! Minimal wildcard matcher: `*` matches any run of characters (including `/`); every other
//! character is literal. `**` collapses to the same behavior as `*`. This is deliberately
//! permissive at the policy layer — the finer coder-style permission *rules* (hierarchical
//! `cmd:subcmd:*` patterns) live in the tool layer, not here.

pub fn wildcard_match(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while vi < v.len() {
        if pi < p.len() && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if let Some(s) = star {
            // Backtrack: let the last `*` consume one more character.
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::wildcard_match;

    #[test]
    fn star_matches_anything() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything/at/all"));
    }

    #[test]
    fn path_globs() {
        assert!(wildcard_match("src/**", "src/main.rs"));
        assert!(wildcard_match("src/**", "src/a/b/c.rs"));
        assert!(!wildcard_match("src/**", "etc/passwd"));
        assert!(wildcard_match("*.md", "README.md"));
        assert!(!wildcard_match("*.md", "main.rs"));
    }

    #[test]
    fn dotted_and_colon_patterns() {
        assert!(wildcard_match("workspace.*", "workspace.read"));
        assert!(!wildcard_match("workspace.*", "secret.read"));
        assert!(wildcard_match("git:*", "git:status"));
        assert!(wildcard_match("git:status", "git:status"));
        assert!(!wildcard_match("git:status", "git:push"));
    }

    #[test]
    fn literal_mismatch() {
        assert!(!wildcard_match("abc", "abd"));
        assert!(wildcard_match("a*c", "abbbc"));
        assert!(!wildcard_match("a*c", "abbb"));
    }
}
