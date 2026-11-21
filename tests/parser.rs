use rewind::engine::args::{ArgScanner, ArgToken};

fn strings(slice: &[&str]) -> Vec<String> {
    slice.iter().map(|v| (*v).to_string()).collect()
}

#[test]
fn test_arg_scanner_short_clustering() {
    let raw = strings(&["-rf", "-fR", "-d"]);
    let mut scanner = ArgScanner::new(&raw);

    // First token: -rf
    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('r'));
        assert_eq!(cluster.next_flag(), Some('f'));
        assert_eq!(cluster.next_flag(), None);
    } else {
        panic!("expected short cluster");
    }

    // Second token: -fR
    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('f'));
        assert_eq!(cluster.next_flag(), Some('R'));
        assert_eq!(cluster.next_flag(), None);
    } else {
        panic!("expected short cluster");
    }

    // Third token: -d
    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('d'));
        assert_eq!(cluster.next_flag(), None);
    } else {
        panic!("expected short cluster");
    }

    assert!(scanner.next_token().is_none());
}

#[test]
fn test_arg_scanner_values_inline_and_separate() {
    // Test short cluster with attached value: -t/tmp/dest
    let raw = strings(&["-t/tmp/dest", "file1.txt"]);
    let mut scanner = ArgScanner::new(&raw);

    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('t'));
        let val = cluster.take_value(&mut scanner).unwrap();
        assert_eq!(val, "/tmp/dest");
    } else {
        panic!("expected short cluster");
    }
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("file1.txt")));
    assert!(scanner.next_token().is_none());

    // Test short cluster with separate value: -t /tmp/dest
    let raw2 = strings(&["-t", "/tmp/dest", "file1.txt"]);
    let mut scanner2 = ArgScanner::new(&raw2);

    let token = scanner2.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('t'));
        let val = cluster.take_value(&mut scanner2).unwrap();
        assert_eq!(val, "/tmp/dest");
    } else {
        panic!("expected short cluster");
    }
    assert_eq!(scanner2.next_token(), Some(ArgToken::Operand("file1.txt")));
    assert!(scanner2.next_token().is_none());

    // Test long option with inline value: --target-directory=/tmp/dest
    let raw3 = strings(&["--target-directory=/tmp/dest", "file2.txt"]);
    let mut scanner3 = ArgScanner::new(&raw3);

    assert_eq!(
        scanner3.next_token(),
        Some(ArgToken::Long {
            name: "target-directory",
            inline_value: Some("/tmp/dest")
        })
    );
    assert_eq!(scanner3.next_token(), Some(ArgToken::Operand("file2.txt")));
    assert!(scanner3.next_token().is_none());

    // Test long option with separate value: --target-directory /tmp/dest
    let raw4 = strings(&["--target-directory", "/tmp/dest", "file3.txt"]);
    let mut scanner4 = ArgScanner::new(&raw4);

    if let Some(ArgToken::Long { name, inline_value }) = scanner4.next_token() {
        assert_eq!(name, "target-directory");
        assert_eq!(inline_value, None);
        let val = inline_value.or_else(|| scanner4.next_raw_arg()).unwrap();
        assert_eq!(val, "/tmp/dest");
    } else {
        panic!("expected long token");
    }
    assert_eq!(scanner4.next_token(), Some(ArgToken::Operand("file3.txt")));
    assert!(scanner4.next_token().is_none());
}

#[test]
fn test_arg_scanner_dash_dash_and_single_dash() {
    let raw = strings(&["-r", "--", "-f", "dir with spaces/file", "normal_file", "-"]);
    let mut scanner = ArgScanner::new(&raw);

    // -r
    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('r'));
    } else {
        panic!("expected short flag");
    }

    // After --, everything is an operand
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("-f")));
    assert_eq!(
        scanner.next_token(),
        Some(ArgToken::Operand("dir with spaces/file"))
    );
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("normal_file")));
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("-")));
    assert!(scanner.next_token().is_none());
}

#[test]
fn test_arg_scanner_interspersed_and_unicode() {
    let raw = strings(&["föö.txt", "-f", "bär.txt", "--force", "テスト.dat"]);
    let mut scanner = ArgScanner::new(&raw);

    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("föö.txt")));
    let token = scanner.next_token().unwrap();
    if let ArgToken::Short(mut cluster) = token {
        assert_eq!(cluster.next_flag(), Some('f'));
    } else {
        panic!("expected short flag");
    }
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("bär.txt")));
    assert_eq!(
        scanner.next_token(),
        Some(ArgToken::Long {
            name: "force",
            inline_value: None
        })
    );
    assert_eq!(scanner.next_token(), Some(ArgToken::Operand("テスト.dat")));
    assert!(scanner.next_token().is_none());
}
