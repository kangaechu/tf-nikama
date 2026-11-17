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
    /// `--no-lock`: plan without taking project locks (plan only).
    pub no_lock: bool,
    /// Everything after `--`, passed to `terraform plan` (plan and apply
    /// only; see `allowed_plan_args`).
    pub extra_args: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommentParseError {
    #[error("unknown command '{0}'")]
    UnknownCommand(String),
    #[error("unknown flag '{0}'")]
    UnknownFlag(String),
    #[error("flag '{0}' requires a value")]
    MissingValue(String),
    #[error("flag '{0}' is only accepted with `plan`")]
    FlagNotAllowed(String),
    #[error("extra arguments after `--` are only accepted with `plan` and `apply`")]
    ExtraArgsNotAllowed,
    #[error("nothing follows `--`")]
    EmptyExtraArgs,
}

/// Returns `None` when the comment is not addressed to this tool, that is
/// when its first non-empty line does not start with one of `wakeup_words`
/// (case-insensitive).
pub fn parse_comment(
    body: &str,
    wakeup_words: &[String],
) -> Option<Result<CommentCommand, CommentParseError>> {
    let first_line = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let lower = first_line.to_ascii_lowercase();
    let rest = wakeup_words
        .iter()
        .map(|w| w.to_ascii_lowercase())
        .find_map(|w| {
            if lower == w {
                Some("")
            } else {
                lower.strip_prefix(&format!("{} ", w))
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
                no_lock: false,
                extra_args: vec![],
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
    let mut no_lock = false;
    let mut extra_args = Vec::new();
    while let Some(tok) = iter.next() {
        if tok == "--" {
            if !matches!(
                verb,
                CommentVerb::Run(Command::Plan) | CommentVerb::Run(Command::Apply)
            ) {
                return Some(Err(CommentParseError::ExtraArgsNotAllowed));
            }
            extra_args.extend(iter.by_ref().filter(|t| !t.trim().is_empty()));
            if extra_args.is_empty() {
                return Some(Err(CommentParseError::EmptyExtraArgs));
            }
            break;
        }
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
            "--no-lock" => {
                if verb != CommentVerb::Run(Command::Plan) {
                    return Some(Err(CommentParseError::FlagNotAllowed(flag)));
                }
                no_lock = true;
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
        no_lock,
        extra_args,
    }))
}

/// The `help` reply, written with the configured wakeup word(s). The first
/// word is used in the examples; any others are listed as alternatives.
pub fn help_text(wakeup_words: &[String]) -> String {
    let w = wakeup_words.first().map(String::as_str).unwrap_or("nikama");
    let mut s = format!(
        "\
**nikama commands**

| Command | Description |
|---|---|
| `{w} plan` | Plan all projects impacted by this PR |
| `{w} plan -p <project>` | Plan selected project(s); `-p` may be repeated, `-d <dir>` selects by directory |
| `{w} plan --no-lock` | Plan without taking project locks (locks this PR already holds for the planned projects are released) |
| `{w} plan -p <project> -- -target=<addr>` | Pass arguments after `--` to `terraform plan` (only those allowed by `allowed_plan_args`) |
| `{w} apply` | Apply all impacted projects (PR must be mergeable) |
| `{w} apply -p <project>` | Apply selected project(s) |
| `{w} apply -p <project> -- -target=<addr>` | Without plan storage only: plan with these arguments immediately before applying |
| `{w} lock` | Take the project locks for this PR without planning (`-p` / `-d` select projects) |
| `{w} unlock` | Release every lock this PR holds; with `-p` / `-d` only the selected projects |
| `{w} help` | Show this message |"
    );
    let others: Vec<String> = wakeup_words
        .iter()
        .skip(1)
        .map(|o| format!("`{}`", o))
        .collect();
    if !others.is_empty() {
        s.push_str(&format!(
            "\n\n{} can be used in place of `{}` in every command.",
            others.join(", "),
            w
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<String> {
        crate::config::DEFAULT_WAKEUP_WORDS
            .iter()
            .map(|w| w.to_string())
            .collect()
    }

    fn parse(body: &str) -> Option<Result<CommentCommand, CommentParseError>> {
        parse_comment(body, &words())
    }

    #[test]
    fn parses_basic() {
        let c = parse("digger plan").unwrap().unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Plan));
        assert!(c.projects.is_empty());
    }

    #[test]
    fn parses_flags() {
        let c = parse("Digger apply -p Vpc --project app -d dev/db")
            .unwrap()
            .unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Apply));
        assert_eq!(c.projects, vec!["Vpc", "app"]);
        assert_eq!(c.dirs, vec!["dev/db"]);
    }

    #[test]
    fn parses_no_lock() {
        let c = parse("digger plan --no-lock -p vpc").unwrap().unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Plan));
        assert!(c.no_lock);
        assert_eq!(c.projects, vec!["vpc"]);
        assert!(!parse("digger plan").unwrap().unwrap().no_lock);
        assert_eq!(
            parse("digger apply --no-lock").unwrap(),
            Err(CommentParseError::FlagNotAllowed("--no-lock".into()))
        );
        assert_eq!(
            parse("digger unlock --no-lock").unwrap(),
            Err(CommentParseError::FlagNotAllowed("--no-lock".into()))
        );
    }

    #[test]
    fn ignores_unrelated() {
        assert!(parse("LGTM, nice work").is_none());
        assert!(parse("").is_none());
        // A wakeup word followed by other characters is not a command.
        assert!(parse("nikama-bot plan").is_none());
        assert!(parse("diggerplan").is_none());
    }

    #[test]
    fn errors() {
        assert_eq!(
            parse("digger noop").unwrap(),
            Err(CommentParseError::UnknownCommand("noop".into()))
        );
        assert_eq!(
            parse("digger plan -p").unwrap(),
            Err(CommentParseError::MissingValue("-p".into()))
        );
        assert_eq!(
            parse("nikama help").unwrap().unwrap().verb,
            CommentVerb::Help
        );
        assert_eq!(parse("nikama").unwrap().unwrap().verb, CommentVerb::Help);
    }

    #[test]
    fn configured_wakeup_words_replace_the_defaults() {
        let tf = vec!["tf".to_string()];
        let c = parse_comment("TF apply -p app", &tf).unwrap().unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Apply));
        assert_eq!(c.projects, vec!["app"]);
        assert_eq!(
            parse_comment("tf", &tf).unwrap().unwrap().verb,
            CommentVerb::Help
        );
        assert!(parse_comment("digger plan", &tf).is_none());
        assert!(parse_comment("nikama plan", &tf).is_none());

        let both = vec!["Terraform".to_string(), "nikama".to_string()];
        assert!(parse_comment("terraform plan", &both).is_some());
        assert!(parse_comment("nikama plan", &both).is_some());
        assert!(parse_comment("digger plan", &both).is_none());
    }

    #[test]
    fn parses_extra_args_after_double_dash() {
        let c = parse("digger plan -p vpc --no-lock -- -target=module.a -var=\"x=a b\"")
            .unwrap()
            .unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Plan));
        assert_eq!(c.projects, vec!["vpc"]);
        assert!(c.no_lock);
        assert_eq!(c.extra_args, vec!["-target=module.a", "-var=x=a b"]);
        // Flags after `--` are terraform's, not ours.
        let c = parse("digger apply -- -p -target=x").unwrap().unwrap();
        assert_eq!(c.verb, CommentVerb::Run(Command::Apply));
        assert!(c.projects.is_empty());
        assert_eq!(c.extra_args, vec!["-p", "-target=x"]);
        assert!(parse("digger plan").unwrap().unwrap().extra_args.is_empty());
        assert_eq!(
            parse("digger plan --").unwrap(),
            Err(CommentParseError::EmptyExtraArgs)
        );
        assert_eq!(
            parse("digger unlock -- -target=x").unwrap(),
            Err(CommentParseError::ExtraArgsNotAllowed)
        );
        assert_eq!(
            parse("digger lock -- x").unwrap(),
            Err(CommentParseError::ExtraArgsNotAllowed)
        );
    }

    #[test]
    fn help_text_uses_the_configured_word() {
        let h = help_text(&words());
        assert!(h.contains("| `nikama plan` |"));
        assert!(h.contains("| `nikama plan -p <project> -- -target=<addr>` |"));
        assert!(h.contains("`digger` can be used in place of `nikama`"));

        let h = help_text(&["tf".to_string()]);
        assert!(h.contains("| `tf apply -p <project>` |"));
        assert!(!h.contains("nikama plan"));
        assert!(!h.contains("in place of"));

        let h = help_text(&["tf".to_string(), "iac".to_string(), "x".to_string()]);
        assert!(h.contains("`iac`, `x` can be used in place of `tf`"));
    }
}
