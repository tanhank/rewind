//! Shared coreutils argument tokenizer and parsing utilities.
//!
//! Provides reusable primitives for parsing GNU and BSD command-line arguments
//! across wrapped commands (`rm`, `mv`, `cp`, `chmod`, `chown`, `git`),
//! including:
//! - Clustered short options (e.g. `-rf`, `-fdx`)
//! - Short options with inline values (e.g. `-tDIR`, `-ePAT`) or separate values (`-t DIR`)
//! - Long options with inline values (e.g. `--target-directory=DIR`) or separate values
//! - `--` end-of-options delimiter
//! - Standalone `-` treated as an operand
//! - Preserving exact user arguments for native command passthrough

/// A scanned token from the command-line argument list.
#[derive(Debug, PartialEq, Eq)]
pub enum ArgToken<'a> {
    /// Long option: `--name` or `--name=value`
    Long {
        name: &'a str,
        inline_value: Option<&'a str>,
    },
    /// Short option cluster: `-abc`
    Short(ShortCluster<'a>),
    /// Non-option operand: positional argument, `-`, or anything after `--`
    Operand(&'a str),
}

/// Tokenizer for a clustered short-option argument (e.g., `-rf`, `-t/dest`).
#[derive(Debug, PartialEq, Eq)]
pub struct ShortCluster<'a> {
    raw: &'a str,
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ShortCluster<'a> {
    fn new(raw: &'a str) -> Self {
        Self {
            raw,
            bytes: raw.as_bytes(),
            // Skip the leading '-' byte
            offset: 1,
        }
    }

    /// Yield the next character in this short-option cluster.
    pub fn next_flag(&mut self) -> Option<char> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let rest = &self.raw[self.offset..];
        let ch = rest.chars().next()?;
        self.offset += ch.len_utf8();
        Some(ch)
    }

    /// Consume a required value for the most recently read flag.
    ///
    /// If there are remaining characters in this cluster (e.g. `-t/tmp` after reading `'t'`),
    /// the remainder is consumed as an inline value. Otherwise, the next argument from
    /// `scanner` is consumed as a separate value.
    pub fn take_value(&mut self, scanner: &mut ArgScanner<'a>) -> Option<&'a str> {
        if self.offset < self.bytes.len() {
            let val = &self.raw[self.offset..];
            self.offset = self.bytes.len();
            Some(val)
        } else {
            scanner.next_raw_arg()
        }
    }

    /// Consume a value if present either inline (remainder of cluster) or as the next argument.
    /// If cluster is at end and no next argument exists, returns `None`.
    pub fn take_optional_value(&mut self, scanner: &mut ArgScanner<'a>) -> Option<&'a str> {
        self.take_value(scanner)
    }
}

/// Scanner for command-line arguments that handles short clustering, long options,
/// values, and the `--` delimiter.
pub struct ArgScanner<'a> {
    args: &'a [String],
    index: usize,
    only_operands: bool,
}

impl<'a> ArgScanner<'a> {
    /// Create a new scanner over a slice of argument strings.
    pub fn new(args: &'a [String]) -> Self {
        Self {
            args,
            index: 0,
            only_operands: false,
        }
    }

    /// Peek at the current argument index.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Whether the `--` end-of-options marker has been encountered.
    pub fn is_only_operands(&self) -> bool {
        self.only_operands
    }

    /// Consume and return the next raw argument string from the stream.
    pub fn next_raw_arg(&mut self) -> Option<&'a str> {
        if self.index < self.args.len() {
            let arg = self.args[self.index].as_str();
            self.index += 1;
            Some(arg)
        } else {
            None
        }
    }

    /// Read the next parsed token.
    pub fn next_token(&mut self) -> Option<ArgToken<'a>> {
        let arg = self.next_raw_arg()?;

        if self.only_operands {
            return Some(ArgToken::Operand(arg));
        }

        // End-of-options delimiter
        if arg == "--" {
            self.only_operands = true;
            // The argument immediately following -- is an operand
            return self.next_token();
        }

        // Standalone dash is an operand (e.g. stdin/stdout), not a flag
        if arg == "-" || !arg.starts_with('-') {
            return Some(ArgToken::Operand(arg));
        }

        // Long option: --foo or --foo=bar
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            return Some(ArgToken::Long {
                name,
                inline_value: inline,
            });
        }

        // Short option cluster: -abc
        Some(ArgToken::Short(ShortCluster::new(arg)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scans_short_flags_and_operands() {
        let args = to_strings(&["-rf", "dir1", "dir2"]);
        let mut scanner = ArgScanner::new(&args);

        match scanner.next_token().unwrap() {
            ArgToken::Short(mut cluster) => {
                assert_eq!(cluster.next_flag(), Some('r'));
                assert_eq!(cluster.next_flag(), Some('f'));
                assert_eq!(cluster.next_flag(), None);
            }
            other => panic!("expected Short cluster, got {:?}", other),
        }

        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("dir1")));
        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("dir2")));
        assert_eq!(scanner.next_token(), None);
    }

    #[test]
    fn scans_long_options_inline_and_separate_values() {
        let args = to_strings(&[
            "--recursive",
            "--target-directory=/tmp/dest",
            "--suffix",
            "~",
            "file.txt",
        ]);
        let mut scanner = ArgScanner::new(&args);

        assert_eq!(
            scanner.next_token(),
            Some(ArgToken::Long {
                name: "recursive",
                inline_value: None,
            })
        );

        assert_eq!(
            scanner.next_token(),
            Some(ArgToken::Long {
                name: "target-directory",
                inline_value: Some("/tmp/dest"),
            })
        );

        match scanner.next_token().unwrap() {
            ArgToken::Long { name, inline_value } => {
                assert_eq!(name, "suffix");
                assert_eq!(inline_value, None);
                let value = inline_value.or_else(|| scanner.next_raw_arg());
                assert_eq!(value, Some("~"));
            }
            other => panic!("expected Long, got {:?}", other),
        }

        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("file.txt")));
        assert_eq!(scanner.next_token(), None);
    }

    #[test]
    fn scans_short_option_with_inline_and_separate_values() {
        // -t/dest vs -t /dest
        let args_inline = to_strings(&["-t/dest", "file1"]);
        let mut scanner = ArgScanner::new(&args_inline);
        match scanner.next_token().unwrap() {
            ArgToken::Short(mut cluster) => {
                assert_eq!(cluster.next_flag(), Some('t'));
                assert_eq!(cluster.take_value(&mut scanner), Some("/dest"));
            }
            other => panic!("expected Short, got {:?}", other),
        }
        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("file1")));

        let args_sep = to_strings(&["-t", "/dest", "file1"]);
        let mut scanner = ArgScanner::new(&args_sep);
        match scanner.next_token().unwrap() {
            ArgToken::Short(mut cluster) => {
                assert_eq!(cluster.next_flag(), Some('t'));
                assert_eq!(cluster.take_value(&mut scanner), Some("/dest"));
            }
            other => panic!("expected Short, got {:?}", other),
        }
        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("file1")));
    }

    #[test]
    fn handles_end_of_options_delimiter() {
        let args = to_strings(&["-f", "--", "-not-a-flag", "--also-operand"]);
        let mut scanner = ArgScanner::new(&args);

        match scanner.next_token().unwrap() {
            ArgToken::Short(mut cluster) => {
                assert_eq!(cluster.next_flag(), Some('f'));
            }
            other => panic!("expected Short, got {:?}", other),
        }

        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("-not-a-flag")));
        assert_eq!(
            scanner.next_token(),
            Some(ArgToken::Operand("--also-operand"))
        );
        assert_eq!(scanner.next_token(), None);
    }

    #[test]
    fn handles_standalone_dash_as_operand() {
        let args = to_strings(&["-", "file.txt"]);
        let mut scanner = ArgScanner::new(&args);
        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("-")));
        assert_eq!(scanner.next_token(), Some(ArgToken::Operand("file.txt")));
        assert_eq!(scanner.next_token(), None);
    }

    #[test]
    fn handles_unicode_and_spaces() {
        let args = to_strings(&["-v", "my document 📄.txt", "路径 with spaces/file"]);
        let mut scanner = ArgScanner::new(&args);
        match scanner.next_token().unwrap() {
            ArgToken::Short(mut cluster) => {
                assert_eq!(cluster.next_flag(), Some('v'));
            }
            other => panic!("expected Short, got {:?}", other),
        }
        assert_eq!(
            scanner.next_token(),
            Some(ArgToken::Operand("my document 📄.txt"))
        );
        assert_eq!(
            scanner.next_token(),
            Some(ArgToken::Operand("路径 with spaces/file"))
        );
        assert_eq!(scanner.next_token(), None);
    }
}
