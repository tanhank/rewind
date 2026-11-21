//! Shared argument model for `mv` and `cp`.
//!
//! rewind only needs to understand which paths the command will read and which
//! destinations it may overwrite. Everything else (output, prompts, unusual
//! flags) is left to the real binary. Unknown or hard-to-model options cause a
//! transparent pass-through rather than a guess.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::staging::absolutize;

/// A resolved source/destination pair.
#[derive(Debug, Clone)]
pub struct Pair {
    pub src_raw: String,
    pub src: PathBuf,
    pub dest: PathBuf,
}

/// Options relevant to undo.
#[derive(Debug, Clone, Default)]
pub struct Transfer {
    pub sources: Vec<String>,
    pub dest: Option<String>,
    pub target_dir: Option<String>,
    pub no_target_dir: bool,
    pub no_clobber: bool,
    pub recursive: bool,
}

/// Parsed invocation.
pub enum Parsed {
    Run(Box<Transfer>),
    /// Let the real command handle this invocation (help, version, backups,
    /// exchange mode, unknown options, too few operands).
    Passthrough,
}

/// Parse `mv`/`cp` arguments.
pub fn parse(argv: &[String], command: &str) -> Parsed {
    let mut transfer = Transfer::default();
    let mut operands: Vec<String> = Vec::new();
    let mut scanner = super::args::ArgScanner::new(argv);

    while let Some(token) = scanner.next_token() {
        match token {
            super::args::ArgToken::Operand(op) => {
                operands.push(op.to_string());
            }
            super::args::ArgToken::Long { name, inline_value } => {
                match name {
                    "target-directory" => {
                        let value = match inline_value.or_else(|| scanner.next_raw_arg()) {
                            Some(val) => val.to_string(),
                            None => return Parsed::Passthrough,
                        };
                        transfer.target_dir = Some(value);
                    }
                    "no-target-directory" => transfer.no_target_dir = true,
                    "no-clobber" => transfer.no_clobber = true,
                    "recursive" => transfer.recursive = true,
                    "verbose"
                    | "force"
                    | "interactive"
                    | "update"
                    | "link"
                    | "symbolic-link"
                    | "one-file-system"
                    | "strip-trailing-slashes"
                    | "remove-destination"
                    | "attributes-only"
                    | "archive"
                    | "no-dereference"
                    | "dereference"
                    | "parents"
                    | "reflink"
                    | "sparse"
                    | "preserve"
                    | "no-preserve"
                    | "context" => {}
                    "suffix" => {
                        if inline_value.is_none() && scanner.next_raw_arg().is_none() {
                            return Parsed::Passthrough;
                        }
                    }
                    // Backups, exchange mode, and anything unrecognized are
                    // delegated: they create extra paths we do not model.
                    _ => return Parsed::Passthrough,
                }
            }
            super::args::ArgToken::Short(mut cluster) => {
                while let Some(flag) = cluster.next_flag() {
                    match flag {
                        't' => {
                            let value = match cluster.take_value(&mut scanner) {
                                Some(val) => val.to_string(),
                                None => return Parsed::Passthrough,
                            };
                            transfer.target_dir = Some(value);
                        }
                        'S' => {
                            if cluster.take_value(&mut scanner).is_none() {
                                return Parsed::Passthrough;
                            }
                        }
                        'T' => transfer.no_target_dir = true,
                        'n' => transfer.no_clobber = true,
                        'r' | 'R' | 'a' => transfer.recursive = true,
                        'v' | 'f' | 'i' | 'p' | 'd' | 'u' | 'l' | 's' | 'x' | 'L' | 'P' | 'H'
                        | 'Z' => {}
                        // -b/--backup creates side files; leave it to the real command.
                        'b' => return Parsed::Passthrough,
                        _ => return Parsed::Passthrough,
                    }
                }
            }
        }
    }

    if transfer.target_dir.is_some() {
        if operands.is_empty() {
            return Parsed::Passthrough;
        }
        transfer.sources = operands;
    } else {
        if operands.len() < 2 {
            return Parsed::Passthrough;
        }
        transfer.dest = operands.pop();
        transfer.sources = operands;
    }

    if transfer.sources.is_empty() {
        return Parsed::Passthrough;
    }
    if transfer.no_target_dir && transfer.sources.len() > 1 {
        // The real command reports this error; do not guess at snapshots.
        return Parsed::Passthrough;
    }
    let _ = command;
    Parsed::Run(Box::new(transfer))
}

/// Resolve every source to its final destination path, mirroring the kernel of
/// coreutils' target-directory rules.
pub fn resolve_pairs(transfer: &Transfer) -> Result<Vec<Pair>> {
    let dest_argument = match (&transfer.target_dir, &transfer.dest) {
        (Some(dir), _) => dir.clone(),
        (None, Some(dest)) => dest.clone(),
        (None, None) => {
            return Err(Error::InvalidInput("no destination specified".into()));
        }
    };
    let dest_abs = absolutize(Path::new(&dest_argument))?;
    let dest_is_dir = fs::metadata(&dest_abs)
        .map(|meta| meta.is_dir())
        .unwrap_or(false);

    let mut pairs = Vec::new();
    for source in &transfer.sources {
        let src = absolutize(Path::new(source))?;
        let dest = if transfer.target_dir.is_some() || (dest_is_dir && !transfer.no_target_dir) {
            match basename(source) {
                Some(name) => dest_abs.join(name),
                None => dest_abs.clone(),
            }
        } else {
            dest_abs.clone()
        };
        pairs.push(Pair {
            src_raw: source.clone(),
            src,
            dest,
        });
    }
    Ok(pairs)
}

/// Basename of an operand, tolerating trailing slashes and `.`/`..`.
pub fn basename(raw: &str) -> Option<&str> {
    let trimmed = raw.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    path.file_name().and_then(|name| name.to_str()).or_else(|| {
        if trimmed == "." || trimmed == ".." {
            Some(trimmed)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn parses_simple_pair() {
        let Parsed::Run(transfer) = parse(&args(&["-v", "a", "b"]), "mv") else {
            panic!("expected run");
        };
        assert_eq!(transfer.sources, vec!["a"]);
        assert_eq!(transfer.dest.as_deref(), Some("b"));
    }

    #[test]
    fn parses_target_directory() {
        let Parsed::Run(transfer) = parse(&args(&["-t", "dir", "a", "b"]), "cp") else {
            panic!("expected run");
        };
        assert_eq!(transfer.target_dir.as_deref(), Some("dir"));
        assert_eq!(transfer.sources, vec!["a", "b"]);
    }

    #[test]
    fn parses_combined_target_directory() {
        let Parsed::Run(transfer) = parse(&args(&["-rvt", "dir", "a"]), "cp") else {
            panic!("expected run");
        };
        assert_eq!(transfer.target_dir.as_deref(), Some("dir"));
        assert!(transfer.recursive);
    }

    #[test]
    fn passthrough_for_backup_and_unknown() {
        assert!(matches!(
            parse(&args(&["-b", "a", "b"]), "cp"),
            Parsed::Passthrough
        ));
        assert!(matches!(
            parse(&args(&["--exchange", "a", "b"]), "mv"),
            Parsed::Passthrough
        ));
        assert!(matches!(
            parse(&args(&["onlyone"]), "cp"),
            Parsed::Passthrough
        ));
    }

    #[test]
    fn basename_handles_trailing_slashes() {
        assert_eq!(basename("a/b/"), Some("b"));
        assert_eq!(basename("/"), None);
        assert_eq!(basename("."), Some("."));
    }
}
