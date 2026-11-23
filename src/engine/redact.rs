//! Redaction of secrets in Terraform / OpenTofu output before it is logged
//! or posted to a pull request.
//!
//! Terraform masks only attributes the provider schema marks as sensitive.
//! Secrets that live in ordinary attributes (container environment
//! variables, user data, connection strings in a settings map) are printed in
//! clear. The built-in patterns below catch the common shapes; a workflow's
//! `filter_regex` adds repository-specific ones.

use regex::Regex;
use std::sync::LazyLock;

pub const REDACTED: &str = "<REDACTED>";

/// Built-in patterns replaced wholesale with `<REDACTED>`.
static TOKENS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // AWS access key ids.
        r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
        // GitHub tokens (classic and fine-grained).
        r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
        // Slack tokens.
        r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
        // Google API keys.
        r"\bAIza[0-9A-Za-z_-]{35}\b",
        // JSON Web Tokens.
        r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+\b",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid built-in pattern"))
    .collect()
});

/// `scheme://user:password@host`: the password part.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)([a-z][a-z0-9+.-]*://[^\s/:@"']+:)[^\s@"']+@"#).expect("valid pattern")
});

/// `password = "value"`, `"DB_SECRET": "value"`, `API_TOKEN=value`, with
/// an optional `-> "new value"` after it as Terraform prints for updates.
/// Group 1 is everything up to the value, group 2 the key itself.
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(["']?([\w.-]*(?:password|passwd|pwd|passphrase|secret|token|api[_-]?key|private[_-]?key|credentials?)[\w.-]*)["']?\s*[=:]\s*)("[^"]*"|'[^']*'|<<-?EOT\b|[^\s,;"'()]+)(\s*->\s*("[^"]*"|'[^']*'|[^\s,;"'()]+))?"#,
    )
    .expect("valid pattern")
});

/// Key suffixes that name something about a secret rather than the secret
/// itself (`secret_arn`, `token_type`, `password_length`, ...).
const HARMLESS_SUFFIXES: &[&str] = &[
    "_arn",
    "_id",
    "_ids",
    "_name",
    "_names",
    "_version",
    "_type",
    "_length",
    "_count",
    "_policy",
    "_ttl",
    "_enabled",
    "_file",
    "_path",
    "_validity",
    "_rotation",
    "_store",
    "_format",
    "_date",
    "_at",
];

static PEM_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----").expect("valid pattern"));
static PEM_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z0-9 ]*PRIVATE KEY-----").expect("valid pattern"));
static HEREDOC_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[-+~]\s*)?EOT,?\s*$").expect("valid pattern"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    /// Inside a PEM private key: the body is collapsed to one line.
    Pem {
        body_seen: bool,
    },
    /// Inside a `<<-EOT` heredoc assigned to a secret-looking key: dropped
    /// up to and including the `EOT` line.
    Heredoc,
}

/// Line-by-line redactor. Multi-line secrets (PEM blocks, heredocs) carry
/// state across lines, so use one instance per output stream.
#[derive(Debug, Clone)]
pub struct Redactor {
    /// Repository-specific pattern (`filter_regex`), applied first.
    custom: Option<Regex>,
    builtin: bool,
    block: Block,
}

impl Redactor {
    pub fn new(custom: Option<Regex>, builtin: bool) -> Self {
        Redactor {
            custom,
            builtin,
            block: Block::None,
        }
    }

    /// Redacts nothing.
    pub fn none() -> Self {
        Self::new(None, false)
    }

    pub fn is_noop(&self) -> bool {
        self.custom.is_none() && !self.builtin
    }

    /// Redact one line. `None` drops the line (the inside of a multi-line
    /// secret).
    pub fn line(&mut self, line: &str) -> Option<String> {
        if self.is_noop() {
            return Some(line.to_string());
        }
        let mut out = match &self.custom {
            Some(re) => re.replace_all(line, REDACTED).into_owned(),
            None => line.to_string(),
        };
        if !self.builtin {
            return Some(out);
        }
        match self.block {
            Block::Pem { body_seen } => {
                if PEM_END.is_match(&out) {
                    self.block = Block::None;
                    return Some(out);
                }
                if body_seen {
                    return None;
                }
                self.block = Block::Pem { body_seen: true };
                return Some(indent_of(&out) + REDACTED);
            }
            Block::Heredoc => {
                if HEREDOC_END.is_match(&out) {
                    self.block = Block::None;
                }
                return None;
            }
            Block::None => {}
        }
        if PEM_BEGIN.is_match(&out) {
            self.block = Block::Pem { body_seen: false };
            return Some(out);
        }
        let mut heredoc = false;
        out = KEY_VALUE
            .replace_all(&out, |caps: &regex::Captures| {
                let key = caps[2].to_ascii_lowercase();
                if HARMLESS_SUFFIXES.iter().any(|s| key.ends_with(s)) {
                    return caps[0].to_string();
                }
                if caps[3].starts_with("<<") {
                    heredoc = true;
                }
                format!("{}{}", &caps[1], REDACTED)
            })
            .into_owned();
        if heredoc {
            self.block = Block::Heredoc;
        }
        out = URL_CREDENTIALS
            .replace_all(&out, format!("${{1}}{}@", REDACTED).as_str())
            .into_owned();
        for re in TOKENS.iter() {
            out = re.replace_all(&out, REDACTED).into_owned();
        }
        Some(out)
    }

    /// Redact a whole text; a fresh block state is used.
    pub fn text(&self, text: &str) -> String {
        if self.is_noop() {
            return text.to_string();
        }
        let mut r = self.clone();
        r.block = Block::None;
        let mut out: Vec<String> = text.lines().filter_map(|l| r.line(l)).collect();
        if text.ends_with('\n') {
            out.push(String::new());
        }
        out.join("\n")
    }
}

fn indent_of(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(text: &str) -> String {
        Redactor::new(None, true).text(text)
    }

    /// Drop the newline that follows `r#"` so the literal can start on its
    /// own line without losing indentation.
    fn t(s: &str) -> &str {
        s.strip_prefix('\n').unwrap_or(s)
    }

    #[test]
    fn noop_leaves_text_alone() {
        let r = Redactor::none();
        assert!(r.is_noop());
        assert_eq!(r.text("password = \"x\"\n"), "password = \"x\"\n");
    }

    #[test]
    fn custom_pattern_runs_before_builtins() {
        let r = Redactor::new(Some(Regex::new("hello-\\w+").unwrap()), true);
        assert_eq!(
            r.text("input = \"hello-world\"\ntoken = \"abc\""),
            "input = \"<REDACTED>\"\ntoken = <REDACTED>"
        );
        let r = Redactor::new(Some(Regex::new("hello-\\w+").unwrap()), false);
        assert_eq!(r.text("token = \"hello-world\""), "token = \"<REDACTED>\"");
    }

    #[test]
    fn key_value_shapes() {
        let cases = [
            (
                "      + password = \"hunter2\"",
                "      + password = <REDACTED>",
            ),
            (
                "      ~ db_password = \"old\" -> \"new\"",
                "      ~ db_password = <REDACTED>",
            ),
            (
                "          + \"DB_PASSWORD\" = \"hunter2\"",
                "          + \"DB_PASSWORD\" = <REDACTED>",
            ),
            (
                "          + \"DATABASE_PASSWORD=hunter2\",",
                "          + \"DATABASE_PASSWORD=<REDACTED>\",",
            ),
            (
                "    {\"api_key\": \"abc\", \"region\": \"us\"}",
                "    {\"api_key\": <REDACTED>, \"region\": \"us\"}",
            ),
            (
                "      + client_secret = 'quoted single'",
                "      + client_secret = <REDACTED>",
            ),
            (
                "export API_TOKEN=abc && run",
                "export API_TOKEN=<REDACTED> && run",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "input: {input}");
        }
    }

    #[test]
    fn terraform_placeholders_and_harmless_keys_are_kept() {
        let kept = [
            "      + password = (sensitive value)",
            "      + password = (known after apply)",
            "      + secret_arn = \"arn:aws:secretsmanager:...\"",
            "      + secret_id = \"my-secret\"",
            "      + token_type = \"Bearer\"",
            "      + password_length = 16",
            "      + name = \"db-password\"",
            "      + tags = {}",
        ];
        for input in kept {
            assert_eq!(redact(input), input, "input: {input}");
        }
    }

    #[test]
    fn well_known_tokens() {
        assert_eq!(
            redact("  + access_key_id = \"AKIAIOSFODNN7EXAMPLE\""),
            "  + access_key_id = \"<REDACTED>\""
        );
        assert_eq!(
            redact("run: curl -H 'Authorization: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789'"),
            "run: curl -H 'Authorization: Bearer <REDACTED>'"
        );
        assert_eq!(
            redact("x github_pat_11ABCDEFG0123456789abcdefghijk x"),
            "x <REDACTED> x"
        );
        assert_eq!(redact("xoxb-1234567890-abcdef"), "<REDACTED>");
        assert_eq!(
            redact("AIzaSyA-1234567890abcdefghijklmnopqrstu"),
            "<REDACTED>"
        );
        assert_eq!(
            redact("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.sig-nature_1"),
            "<REDACTED>"
        );
    }

    #[test]
    fn url_credentials() {
        assert_eq!(
            redact("  + url = \"postgres://app:hunter2@db.internal:5432/app\""),
            "  + url = \"postgres://app:<REDACTED>@db.internal:5432/app\""
        );
        assert_eq!(
            redact("  + endpoint = \"https://db.internal:5432/app\""),
            "  + endpoint = \"https://db.internal:5432/app\""
        );
    }

    #[test]
    fn pem_private_key_body_is_collapsed() {
        let input = t(r#"
      + private_key = <<-EOT
            -----BEGIN RSA PRIVATE KEY-----
            MIIEowIBAAKCAQEA
            abcdef
            -----END RSA PRIVATE KEY-----
        EOT
      + cert = <<-EOT
            -----BEGIN CERTIFICATE-----
            MIIC
            -----END CERTIFICATE-----
        EOT
"#);
        // `private_key` is a secret-looking key, so its heredoc is dropped
        // entirely; the certificate is not a private key and stays.
        assert_eq!(
            redact(input),
            t(r#"
      + private_key = <REDACTED>
      + cert = <<-EOT
            -----BEGIN CERTIFICATE-----
            MIIC
            -----END CERTIFICATE-----
        EOT
"#)
        );
        // A PEM block under an innocuous key keeps its markers and loses
        // its body.
        let input = t(r#"
      + user_data = <<-EOT
            echo install
            -----BEGIN OPENSSH PRIVATE KEY-----
            b3BlbnNzaC1rZXktdjEAAAAA
            AAAAAAAAAAAAAAAAAAAAAAAA
            -----END OPENSSH PRIVATE KEY-----
            echo done
        EOT
"#);
        assert_eq!(
            redact(input),
            t(r#"
      + user_data = <<-EOT
            echo install
            -----BEGIN OPENSSH PRIVATE KEY-----
            <REDACTED>
            -----END OPENSSH PRIVATE KEY-----
            echo done
        EOT
"#)
        );
    }

    #[test]
    fn heredoc_diff_lines_are_dropped_until_terminator() {
        let input = t(r#"
      ~ secret_env = <<-EOT
          - A=1
          + A=2
        EOT
      + image = "nginx"
"#);
        assert_eq!(
            redact(input),
            "      ~ secret_env = <REDACTED>\n      + image = \"nginx\"\n"
        );
    }

    #[test]
    fn line_state_is_per_instance() {
        let mut r = Redactor::new(None, true);
        assert_eq!(
            r.line("  -----BEGIN EC PRIVATE KEY-----").as_deref(),
            Some("  -----BEGIN EC PRIVATE KEY-----")
        );
        assert_eq!(r.line("  body").as_deref(), Some("  <REDACTED>"));
        assert_eq!(r.line("  more"), None);
        assert_eq!(
            r.line("  -----END EC PRIVATE KEY-----").as_deref(),
            Some("  -----END EC PRIVATE KEY-----")
        );
        assert_eq!(r.line("  after").as_deref(), Some("  after"));
        // `text` starts from a clean state even on an instance mid-block.
        let mut mid = Redactor::new(None, true);
        mid.line("-----BEGIN PRIVATE KEY-----");
        assert_eq!(mid.text("plain"), "plain");
    }
}
