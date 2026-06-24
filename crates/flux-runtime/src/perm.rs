//! Coder-style permission rules: `bash` (bare tool, matches all invocations) or `Bash(git:*)`
//! (tool + subject glob). Deny-first; unmatched subjects escalate to an approval prompt.

use flux_policy::wildcard_match;

/// A parsed permission pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    pub tool: String,
    /// `None` means "all invocations of this tool".
    pub subject: Option<String>,
}

impl Pattern {
    /// Parse `"bash"` or `"Bash(git:*)"`. Tool name is lowercased.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if let Some(open) = s.find('(') {
            if s.ends_with(')') {
                let tool = s[..open].trim().to_lowercase();
                let subject = s[open + 1..s.len() - 1].trim().to_string();
                return Pattern {
                    tool,
                    subject: Some(subject),
                };
            }
        }
        Pattern {
            tool: s.to_lowercase(),
            subject: None,
        }
    }

    fn matches(&self, tool: &str, subject: &str) -> bool {
        self.tool == tool
            && match &self.subject {
                None => true,
                Some(p) => wildcard_match(p, subject),
            }
    }

    fn is_bare_for(&self, tool: &str) -> bool {
        self.tool == tool && self.subject.is_none()
    }

    /// Render back to rule-string form.
    pub fn render(&self) -> String {
        match &self.subject {
            None => self.tool.clone(),
            Some(s) => format!("{}({s})", self.tool),
        }
    }
}

/// The outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermDecision {
    Allow,
    Deny,
    Ask,
}

/// Evaluates allow/deny rules for tool invocations and remembers newly-approved patterns.
#[derive(Debug, Default, Clone)]
pub struct PermissionManager {
    allow: Vec<Pattern>,
    deny: Vec<Pattern>,
}

impl PermissionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rules(allow: &[String], deny: &[String]) -> Self {
        Self {
            allow: allow.iter().map(|s| Pattern::parse(s)).collect(),
            deny: deny.iter().map(|s| Pattern::parse(s)).collect(),
        }
    }

    /// Add an allow rule (e.g. after the user picks "always allow").
    pub fn add_allow(&mut self, rule: &str) {
        self.allow.push(Pattern::parse(rule));
    }

    /// The current allow rules in string form (for persistence).
    pub fn allow_rules(&self) -> Vec<String> {
        self.allow.iter().map(Pattern::render).collect()
    }

    /// Decide whether a tool invocation (with its permission subjects) is allowed, denied, or
    /// must be asked. Deny wins; otherwise all subjects must be covered by an allow rule.
    pub fn check(&self, tool: &str, subjects: &[String]) -> PermDecision {
        let tool = tool.to_lowercase();

        for d in &self.deny {
            if d.tool != tool {
                continue;
            }
            match &d.subject {
                None => return PermDecision::Deny,
                Some(_) => {
                    if subjects.iter().any(|s| d.matches(&tool, s)) {
                        return PermDecision::Deny;
                    }
                }
            }
        }

        if self.allow.iter().any(|a| a.is_bare_for(&tool)) {
            return PermDecision::Allow;
        }
        if subjects.is_empty() {
            return PermDecision::Ask;
        }
        let covered = subjects
            .iter()
            .all(|s| self.allow.iter().any(|a| a.matches(&tool, s)));
        if covered {
            PermDecision::Allow
        } else {
            PermDecision::Ask
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_subject_patterns() {
        let p = Pattern::parse("bash");
        assert_eq!(p.tool, "bash");
        assert_eq!(p.subject, None);
        let p = Pattern::parse("Bash(git:*)");
        assert_eq!(p.tool, "bash");
        assert_eq!(p.subject.as_deref(), Some("git:*"));
    }

    #[test]
    fn deny_takes_precedence() {
        let m = PermissionManager::from_rules(&["bash".into()], &["Bash(rm:*)".into()]);
        assert_eq!(m.check("bash", &["rm:-rf /".into()]), PermDecision::Deny);
        // bare allow covers other subjects
        assert_eq!(m.check("bash", &["git:status".into()]), PermDecision::Allow);
    }

    #[test]
    fn subject_allow_requires_all_covered() {
        let m = PermissionManager::from_rules(&["Bash(git:*)".into()], &[]);
        assert_eq!(m.check("bash", &["git:status".into()]), PermDecision::Allow);
        // one uncovered subject → ask
        assert_eq!(
            m.check("bash", &["git:status".into(), "curl:evil.com".into()]),
            PermDecision::Ask
        );
    }

    #[test]
    fn unmatched_is_ask() {
        let m = PermissionManager::new();
        assert_eq!(m.check("write", &["secret.key".into()]), PermDecision::Ask);
    }

    #[test]
    fn add_allow_then_allows_and_renders() {
        let mut m = PermissionManager::new();
        m.add_allow("Read(*)");
        assert_eq!(m.check("read", &["anything".into()]), PermDecision::Allow);
        assert_eq!(m.allow_rules(), vec!["read(*)".to_string()]);
    }
}
