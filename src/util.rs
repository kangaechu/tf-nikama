//! Small shared helpers.

use std::collections::BTreeMap;

/// Read an environment variable, treating empty strings as unset.
pub fn env_opt(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Read the first environment variable in `names` that is set and non-empty.
pub fn env_first(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| env_opt(n))
}

/// `true` when the variable is set to `true` (case-insensitive).
pub fn env_is_true(name: &str) -> bool {
    env_opt(name)
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Expand `$VAR` and `${VAR}` references using the process environment.
/// Unknown variables expand to the empty string, matching Go's `os.ExpandEnv`.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next();
                let mut name = String::new();
                for n in chars.by_ref() {
                    if n == '}' {
                        break;
                    }
                    name.push(n);
                }
                out.push_str(&std::env::var(&name).unwrap_or_default());
            }
            Some(n) if n.is_ascii_alphanumeric() || *n == '_' => {
                let mut name = String::new();
                while let Some(n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || *n == '_' {
                        name.push(*n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(&std::env::var(&name).unwrap_or_default());
            }
            _ => out.push('$'),
        }
    }
    out
}

/// Split `owner/repo` into its two halves.
pub fn split_repo(full_name: &str) -> Option<(&str, &str)> {
    let (owner, repo) = full_name.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

/// Merge `b` into `a`, with `b` winning on duplicate keys.
pub fn merge_maps(
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = a.clone();
    for (k, v) in b {
        out.insert(k.clone(), v.clone());
    }
    out
}

/// Truncate a string to at most `max_chars` characters, appending `suffix` when truncated.
pub fn truncate_chars(s: &str, max_chars: usize, suffix: &str) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(suffix.chars().count());
    let mut out: String = s.chars().take(keep).collect();
    out.push_str(suffix);
    out
}

/// Remove ANSI escape sequences.
pub fn strip_ansi(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());
    re.replace_all(s, "").into_owned()
}

/// Serialises tests that mutate process environment variables.
#[cfg(test)]
pub(crate) fn test_env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_env() {
        std::env::set_var("NIKAMA_TEST_X", "hello");
        assert_eq!(expand_env("a $NIKAMA_TEST_X b"), "a hello b");
        assert_eq!(expand_env("${NIKAMA_TEST_X}!"), "hello!");
        assert_eq!(expand_env("$NIKAMA_TEST_UNSET-"), "-");
        assert_eq!(expand_env("$$"), "$$");
    }

    #[test]
    fn splits_repo() {
        assert_eq!(split_repo("a/b"), Some(("a", "b")));
        assert_eq!(split_repo("a"), None);
    }

    #[test]
    fn truncates() {
        assert_eq!(truncate_chars("abcdef", 4, ".."), "ab..");
        assert_eq!(truncate_chars("abc", 4, ".."), "abc");
    }
}
