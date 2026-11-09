//! Parsing of PR comment commands such as `digger plan -p vpc -p app`.

use crate::engine::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentVerb {
    Run(Command),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentCommand {
    pub verb: CommentVerb,
    pub projects: Vec<String>,
    pub dirs: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommentParseError {
    #[error("unknown command '{0}'")]
    UnknownCommand(String),
    #[error("unknown flag '{0}'")]
    UnknownFlag(String),
    #[error("flag '{0}' requires a value")]
    MissingValue(String),
}

/// Returns `None` when the comment is not addressed to this tool.
pub fn parse_comment(body: &str) -> Option<Result<CommentCommand, CommentParseError>> {
    let first_line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let lower = first_line.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("digger ")
        .or_else(|| lower.strip_prefix("nikama "))
        .or_else(|| {
            if lower == "digger" || lower == "nikama" {
                Some("")
            } else {
                None
            }
        })?;
    // Re-split the original (case preserved) text after the prefix for values.
    let original_rest = &first_line[first_line.len() - rest.len()..];
    let tokens = match shell_words::split(original_rest) {
        Ok(t) => t,
        Err(_) => original_rest
            .split_whitespace()
            .map(|s| s.to_string())
            .collect(),
    };
    let mut iter = tokens.into_iter();
    let verb_token = match iter.next() {
        Some(v) => v,
        None => {
            return Some(Ok(CommentCommand {
                verb: CommentVerb::Help,
                projects: vec![],
                dirs: vec![],
            }))
        }
    };
    let verb = match verb_token.to_ascii_lowercase().as_str() {
        "help" => CommentVerb::Help,
        other => match Command::parse(other) {
            Some(c) => CommentVerb::Run(c),
            None => return Some(Err(CommentParseError::UnknownCommand(verb_token))),
        },
    };
    let mut projects = Vec::new();
    let mut dirs = Vec::new();
    while let Some(tok) = iter.next() {
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with('-') => (f.to_string(), Some(v.to_string())),
            _ => (tok.clone(), None),
        };
        match flag.as_str() {
            "-p" | "--project" => {
                let v = match inline.or_else(|| iter.next()) {
                    Some(v) => v,
                    None => return Some(Err(CommentParseError::MissingValue(flag))),
                };
                projects.push(v);
            }
            "-d" | "--directory" => {
                let v = match inline.or_else(|| iter.next()) {
                    Some(v) => v,
                    None => return Some(Err(CommentParseError::MissingValue(flag))),
                };
                dirs.push(v);
            }
            other if other.starts_with('-') => {
                return Some(Err(CommentParseError::UnknownFlag(other.to_string())))
            }
            // Trailing free text is ignored (e.g. "digger plan please").
            _ => {}
        }
    }
    Some(Ok(CommentCommand {
        verb,
        projects,
        dirs,
    }))
}

pub const HELP_TEXT: &str = "\
**nikama commands**

| Command | Description |
|---|---|
| `digger plan` | Plan all projects impacted by this PR |
| `digger plan -p <project>` | Plan selected project(s); `-p` may be repeated, `-d <dir>` selects by directory |
| `digger apply` | Apply all impacted projects (PR must be mergeable) |
| `digger apply -p <project>` | Apply selected project(s) |
| `digger lock` / `digger unlock` | Acquire / release the PR-level lock for impacted projects |
| `digger help` | Show this message |

`nikama` can be used in place of `digger` in every command.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic() {
        let c = parse_comment("digger plan").unwrap().unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Plan));
        assert!(c.projects.is_empty());
    }

    #[test]
    fn parses_flags() {
        let c = parse_comment("Digger apply -p Vpc --project app -d dev/db")
            .unwrap()
            .unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Apply));
        assert_eq!(c.projects, vec!["Vpc", "app"]);
        assert_eq!(c.dirs, vec!["dev/db"]);
    }

    #[test]
    fn ignores_unrelated() {
        assert!(parse_comment("LGTM, nice work").is_none());
        assert!(parse_comment("").is_none());
    }

    #[test]
    fn errors() {
        assert_eq!(
            parse_comment("digger noop").unwrap(),
            Err(CommentParseError::UnknownCommand("noop".into()))
        );
        assert_eq!(
            parse_comment("digger plan -p").unwrap(),
            Err(CommentParseError::MissingValue("-p".into()))
        );
        assert_eq!(
            parse_comment("nikama help").unwrap().unwrap().verb,
            CommentVerb::Help
        );
    }
}
