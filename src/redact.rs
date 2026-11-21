//! Sensitive command argument redaction for journal persistence and diagnostics.
//!
//! Preserves original in-memory arguments for command execution while ensuring
//! secrets (passwords, tokens, API keys, client secrets, auth headers, and URL credentials)
//! are never persisted to the SQLite journal or exposed in log outputs.

const REDACTED: &str = "[REDACTED]";

/// Redact sensitive options, flags, headers, and URL credentials from argument list.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut iter = argv.iter().peekable();

    while let Some(arg) = iter.next() {
        // 1. Check for inline flag: `--flag=value` or `-flag=value`
        if let Some((flag, val)) = split_flag_value(arg) {
            if is_sensitive_flag_name(flag) {
                out.push(format!("{flag}={REDACTED}"));
                continue;
            }
            if is_header_flag_name(flag) {
                out.push(format!("{flag}={}", redact_header_value(val)));
                continue;
            }
            if is_git_config_flag(flag) {
                out.push(format!("{flag}={}", redact_git_config_value(val)));
                continue;
            }
            // Non-sensitive flag with potential URL in value
            out.push(format!("{flag}={}", redact_embedded_secrets(val)));
            continue;
        }

        // 2. Check for standalone flag expecting value in next argument
        if is_sensitive_flag_name(arg) {
            out.push(arg.clone());
            if let Some(next_val) = iter.next() {
                // If next value is another flag, do not consume it as value
                if next_val.starts_with('-') && !is_likely_value(next_val) {
                    out.push(next_val.clone());
                } else {
                    out.push(REDACTED.to_string());
                }
            }
            continue;
        }

        // 3. Standalone header flag `-H "Header: value"` or `--header "Header: value"`
        if is_header_flag_name(arg) {
            out.push(arg.clone());
            if let Some(next_val) = iter.next() {
                out.push(redact_header_value(next_val));
            }
            continue;
        }

        // 4. Standalone git -c config flag
        if arg == "-c" {
            out.push(arg.clone());
            if let Some(next_val) = iter.next() {
                out.push(redact_git_config_value(next_val));
            }
            continue;
        }

        // 5. Positional arguments / URLs / connection strings
        out.push(redact_embedded_secrets(arg));
    }

    out
}

/// Redact a joined command line string for display.
pub fn redact_cmdline(cmdline: &str) -> String {
    redact_embedded_secrets(cmdline)
}

fn split_flag_value(arg: &str) -> Option<(&str, &str)> {
    if !arg.starts_with('-') {
        return None;
    }
    arg.split_once('=')
}

fn is_sensitive_flag_name(flag: &str) -> bool {
    let name = flag.trim_start_matches('-');
    let lower = name.to_ascii_lowercase();

    matches!(
        lower.as_str(),
        "password"
            | "passwd"
            | "pass"
            | "pwd"
            | "token"
            | "auth-token"
            | "auth_token"
            | "access-token"
            | "access_token"
            | "api-token"
            | "api_token"
            | "bearer-token"
            | "bearer_token"
            | "secret"
            | "secret-key"
            | "secret_key"
            | "client-secret"
            | "client_secret"
            | "api-secret"
            | "api_secret"
            | "api-key"
            | "api_key"
            | "apikey"
            | "auth"
            | "authorization"
            | "authentication"
            | "proxy-password"
            | "proxy_password"
            | "proxy-user-password"
    )
}

fn is_header_flag_name(flag: &str) -> bool {
    matches!(
        flag.to_ascii_lowercase().as_str(),
        "-h" | "--header" | "-header"
    )
}

fn is_git_config_flag(flag: &str) -> bool {
    matches!(flag.to_ascii_lowercase().as_str(), "-c")
}

fn is_likely_value(val: &str) -> bool {
    // If a value starts with '-' but is not purely flags (e.g. negative number or token starting with -)
    val.len() > 1 && val.chars().nth(1).is_some_and(|c| c.is_alphanumeric())
}

fn redact_header_value(header: &str) -> String {
    let lower = header.to_ascii_lowercase();
    if let Some((name, val)) = header.split_once(':') {
        let name_lower = name.trim().to_ascii_lowercase();
        if name_lower == "authorization" || name_lower == "proxy-authorization" {
            let val_trimmed = val.trim();
            if let Some((scheme, _)) = val_trimmed.split_once(' ') {
                return format!("{name}: {scheme} {REDACTED}");
            }
            return format!("{name}: {REDACTED}");
        }
        if name_lower == "x-api-key"
            || name_lower == "x-auth-token"
            || name_lower == "api-key"
            || name_lower == "token"
        {
            return format!("{name}: {REDACTED}");
        }
    } else if lower.contains("bearer ") {
        if let Some((prefix, _)) = header.split_once("bearer ") {
            return format!("{prefix}bearer {REDACTED}");
        }
    }
    redact_embedded_secrets(header)
}

fn redact_git_config_value(val: &str) -> String {
    if let Some((key, value)) = val.split_once('=') {
        let key_lower = key.to_ascii_lowercase();
        if key_lower.contains("extraheader") || key_lower.contains("authorization") {
            return format!("{key}={}", redact_header_value(value));
        }
    }
    redact_embedded_secrets(val)
}

/// Redact credentials embedded in URLs: `scheme://user:pass@host` or `scheme://token@host`.
fn redact_embedded_secrets(s: &str) -> String {
    let mut result = s.to_string();

    // Pattern 1: scheme://user:password@host
    // Find "://" and subsequent "@"
    if let Some(scheme_pos) = result.find("://") {
        let after_scheme = scheme_pos + 3;
        if let Some(at_offset) = result[after_scheme..].find('@') {
            let at_pos = after_scheme + at_offset;
            let cred_part = &result[after_scheme..at_pos];
            if !cred_part.contains('/') && !cred_part.contains(' ') {
                let new_cred = if let Some((user, _pass)) = cred_part.split_once(':') {
                    format!("{user}:{REDACTED}")
                } else {
                    REDACTED.to_string()
                };
                result = format!(
                    "{}{}{}",
                    &result[..after_scheme],
                    new_cred,
                    &result[at_pos..]
                );
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_inline_sensitive_flags() {
        let argv = vec![
            "curl".into(),
            "--password=secret123".into(),
            "--Token=ghp_ABC123".into(),
            "--API-KEY=key_xyz".into(),
            "--client_secret=client_secret_val".into(),
            "-f".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted,
            vec![
                "curl",
                "--password=[REDACTED]",
                "--Token=[REDACTED]",
                "--API-KEY=[REDACTED]",
                "--client_secret=[REDACTED]",
                "-f",
            ]
        );
    }

    #[test]
    fn redacts_separate_sensitive_flags() {
        let argv = vec![
            "command".into(),
            "--password".into(),
            "super_secret_password".into(),
            "--token".into(),
            "secret_token_123".into(),
            "--secret".into(),
            "hidden_secret".into(),
            "file.txt".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted,
            vec![
                "command",
                "--password",
                "[REDACTED]",
                "--token",
                "[REDACTED]",
                "--secret",
                "[REDACTED]",
                "file.txt",
            ]
        );
    }

    #[test]
    fn preserves_non_secret_flags_including_p() {
        let argv = vec![
            "cp".into(),
            "-p".into(),
            "--parents".into(),
            "--path=/tmp/foo".into(),
            "src".into(),
            "dst".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(redacted, argv);
    }

    #[test]
    fn redacts_authorization_headers() {
        let argv = vec![
            "curl".into(),
            "-H".into(),
            "Authorization: Bearer my-secret-jwt-token".into(),
            "--header=X-Api-Key: secret-key-value".into(),
            "https://example.com".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted,
            vec![
                "curl",
                "-H",
                "Authorization: Bearer [REDACTED]",
                "--header=X-Api-Key: [REDACTED]",
                "https://example.com",
            ]
        );
    }

    #[test]
    fn redacts_embedded_url_credentials() {
        let argv = vec![
            "git".into(),
            "clone".into(),
            "https://myuser:mypassword@github.com/org/repo.git".into(),
            "https://token12345@gitlab.com/org/repo.git".into(),
            "postgres://admin:dbsecret@localhost:5432/mydb".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted,
            vec![
                "git",
                "clone",
                "https://myuser:[REDACTED]@github.com/org/repo.git",
                "https://[REDACTED]@gitlab.com/org/repo.git",
                "postgres://admin:[REDACTED]@localhost:5432/mydb",
            ]
        );
    }

    #[test]
    fn redacts_git_extra_headers() {
        let argv = vec![
            "git".into(),
            "-c".into(),
            "http.extraHeader=Authorization: Basic dXNlcjpwYXNz".into(),
            "fetch".into(),
        ];
        let redacted = redact_argv(&argv);
        assert_eq!(
            redacted,
            vec![
                "git",
                "-c",
                "http.extraHeader=Authorization: Basic [REDACTED]",
                "fetch",
            ]
        );
    }
}
