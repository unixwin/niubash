//! niubash#385: `niu --help` lists the GNU-compatible short invocation
//! options, generated from `rubash::usage::SHORT_OPTIONS`.
//!
//! The launcher's product help used to advertise only `-h -V -c -C` (plus
//! the generated long-option table), even though the engine parser accepts
//! `-e`, `-o`, `-u`, `-x`, `-l`, `-i`, `-s`, ... — the omission is why an
//! invalid-looking clustered option produced a parser error rather than an
//! "unknown option" error. The help is now generated from the shared table,
//! so every accepted short option has a line; this test fails if the table
//! and the printed help drift apart.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn niu_binary() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_niu"));
    if p.exists() {
        return p;
    }
    let mut fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fallback.push("target");
    fallback.push("debug");
    fallback.push(if cfg!(windows) { "niu.exe" } else { "niubash" });
    fallback
}

fn niu_help() -> String {
    let output = Command::new(niu_binary())
        .arg("--help")
        .env_remove("NIU_INVOKED_AS")
        .stdin(Stdio::null())
        .output()
        .expect("spawn niu --help");
    assert!(
        output.status.success(),
        "niu --help failed: {:?}",
        output.status.code()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn help_lists_every_short_option_from_the_shared_table() {
    let help = niu_help();
    assert!(help.contains("Niubash "), "not the product help:\n{help}");
    for option in rubash::usage::SHORT_OPTIONS {
        let mut spec = format!("-{}", option.letter);
        if let Some(value) = option.value_name {
            spec.push_str(" <");
            spec.push_str(value);
            spec.push('>');
        }
        if let Some(long) = option.long_alias {
            spec.push_str(", ");
            spec.push_str(long);
        }
        let line = format!("  {spec:<26}{}", option.description);
        assert!(
            help.contains(&line),
            "niu --help is missing the generated line:\n  {line}\n--- help ---\n{help}"
        );
    }
}

#[test]
fn help_covers_the_options_named_in_the_issue() {
    // The report's explicit list: -e, -o, -u, -x, -l, -i, -s.
    let help = niu_help();
    for letter in ['e', 'o', 'u', 'x', 'l', 'i', 's'] {
        let marker = format!("  -{letter}");
        assert!(
            help.lines().any(|line| line.starts_with(&marker)
                && line[marker.len()..].starts_with(|c: char| c == ' ' || c == ',')),
            "niu --help has no line for -{letter}:\n{help}"
        );
    }
}

#[test]
fn help_still_lists_the_generated_long_options() {
    let help = niu_help();
    for name in rubash::usage::LONG_OPTIONS {
        assert!(
            help.contains(&format!("    --{name}")),
            "niu --help is missing long option --{name}:\n{help}"
        );
    }
}
