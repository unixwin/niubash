//! niubash#385 death gate: `niu --help` must carry the complete GNU bash
//! invocation surface, in the `bash --help` (GNU 5.3) shape, and the list it
//! carries must be exactly the parser's accepted long-option set.
//!
//! The report (owner, five times): the help only advertised the product
//! flags while the parser accepted the full GNU face — and the Windows
//! release journey must die if that ever regresses. These tests run on every
//! CI lane, so a missing flag turns master red and stops the release
//! journey. They gate on strings, not snapshots, so a wording fix cannot
//! silently drop a flag again.

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

/// The GNU face leads: usage pair, every GNU long option, both shell-option
/// lines, the value-carrying annotations, and the tail notes — the `bash
/// --help` block order (GNU 5.3 show_shell_usage), adapted to the niu face.
#[test]
fn help_opens_with_the_complete_gnu_invocation_face() {
    let help = niu_help();
    let required = [
        // Usage pair (GNU 5.3 wording, niu program word).
        "Usage:  niu [GNU long option] [option] ...",
        "        niu [GNU long option] [option] script-file ...",
        // GNU long options, complete list — one assert per option so a red
        // names the exact flag that went missing.
        "    --debug",
        "    --debugger",
        "    --dump-po-strings",
        "    --dump-strings",
        "    --help",
        "    --init-file <file>",
        "    --login",
        "    --noediting",
        "    --noprofile",
        "    --norc",
        "    --posix",
        "    --pretty-print",
        "    --rcfile <file>",
        "    --restricted",
        "    --verbose",
        "    --version",
        // Shell options block, GNU 5.3 wording.
        "-ilrsD or -c command or -O shopt_option",
        "(invocation only)",
        "-abefhkmnptuvxBCEHPT or -o option",
        // Value-carrying invocation options carry their argument notes
        // (-c/-s/--init-file/--rcfile).
        "-c <command> runs the command string",
        "-s reads commands from standard input",
        "--init-file and --rcfile each",
        // Tail notes.
        "Type `niu -c \"help set\"` for more information about shell options.",
        "Type `niu -c help` for more information about shell builtin commands.",
        "Report bugs at <https://github.com/unixwin/niubash/issues>.",
        "Niubash home page: <https://github.com/unixwin/niubash>.",
        // Portal lines closing the output — the GNU home-page tail
        // convention ("bash home page: <…>"), mirrored by --version.
        "Home: <https://unixwin.dev>",
        "Docs & downloads: <https://unixwin.dev>",
        // The restricted invocation face is advertised in both spellings.
        "    --restricted",
        "-r, --restricted",
    ];
    for marker in required {
        assert!(
            help.contains(marker),
            "niu --help lost the GNU invocation-face line `{marker}` \
             (niubash#385 death gate):\n{help}"
        );
    }
}

/// The native debugger frontends are advertised on the product face: the
/// interactive CLI, the DAP server, the spelled-out editor form, and GNU
/// bashdb parity. The lines are deliberately not in the GNU long-option
/// column (`--dap` is a native citizen, not a shell.c option), so the
/// long-option manifest count above still matches the engine parser.
#[test]
fn help_advertises_the_native_debugger_frontends() {
    let help = niu_help();
    for marker in [
        "niu --debug <script> [args]",
        "niu --dap <script> [args]",
        "niu --debug --dap <script>",
        "niu --debugger <script>",
    ] {
        assert!(
            help.contains(marker),
            "niu --help lost the debugger line `{marker}`:\n{help}"
        );
    }
}

/// The face leads the help: the GNU long-options header comes before the
/// product Options block, and the usage pair comes first of all.
#[test]
fn help_orders_the_gnu_face_before_the_product_block() {
    let help = niu_help();
    let banner = help.find("Niubash ").expect("version banner");
    let usage = help
        .find("Usage:  niu [GNU long option]")
        .expect("GNU usage");
    let long_header = help.find("GNU long options:").expect("GNU long options");
    let shell_header = help.find("Shell options:").expect("Shell options");
    let product = help.find("\nOptions:\n").expect("product Options block");
    assert!(banner < usage);
    assert!(usage < long_header);
    assert!(long_header < shell_header);
    assert!(shell_header < product);
}

/// Manifest <-> parser consistency: the parser-side long-option table
/// (`rubash::usage::LONG_OPTIONS` — the table the engine's argv parser
/// walks, and the one `show_shell_usage` prints after an invalid option)
/// and the help must enumerate the same set. A flag added to the parser
/// without a help line turns red here, and so does a help line the parser
/// does not accept: one too many is as wrong as one too few.
#[test]
fn help_manifest_matches_the_parser_long_option_table() {
    let help = niu_help();
    for name in rubash::usage::LONG_OPTIONS {
        assert!(
            help.contains(&format!("    --{name}")),
            "parser accepts --{name} but niu --help does not list it \
             (niubash#385 death gate):\n{help}"
        );
    }
    let listed = help
        .lines()
        .filter(|line| line.starts_with("    --"))
        .count();
    assert_eq!(
        listed,
        rubash::usage::LONG_OPTIONS.len(),
        "niu --help lists {listed} long options but the parser table has {} \
         (niubash#385 death gate):\n{help}",
        rubash::usage::LONG_OPTIONS.len()
    );
}

/// Every long option the help lists must be one the invocation actually
/// accepts. `--help`/`--version` are answered by the launcher face before
/// the engine parser sees them (accepted above); every other table entry
/// must round-trip `ShellInvocation::parse`. And `--interactive`, which the
/// parser rejects exactly like GNU, must stay out of the help: one too many
/// is a lie the gate refuses.
#[test]
fn help_never_lists_an_option_the_invocation_rejects() {
    for name in rubash::usage::LONG_OPTIONS {
        if matches!(*name, "help" | "version") {
            continue;
        }
        let mut argv = vec![format!("--{name}")];
        if rubash::usage::LONG_OPTIONS_WITH_VALUE.contains(name) {
            argv.push(".".to_string());
        }
        let parsed = rubash::invocation::ShellInvocation::parse(&argv);
        assert!(
            parsed.is_ok(),
            "--{name} is in niu --help but ShellInvocation::parse rejects it: {parsed:?}"
        );
    }
    let help = niu_help();
    assert!(
        !help.contains("--interactive"),
        "niu --help advertises --interactive but the invocation rejects it \
         (GNU parity; niubash#385 death gate):\n{help}"
    );
}

/// The `-r`/`--restricted` regression lock: the invocation-level restricted
/// mode must be accepted and armed — never re-routed into the shopt space
/// (`bash: line 0: restricted: invalid shell option name`, the defect this
/// ticket was reopened for) — and the red lines must bite.
#[test]
fn restricted_invocation_flags_are_accepted_and_enforced() {
    let probe = |argv: &[&str]| {
        let output = Command::new(niu_binary())
            .args(argv)
            .env_remove("NIU_INVOKED_AS")
            .stdin(Stdio::null())
            .output()
            .expect("spawn niu");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    // Both spellings run the command: accepted, not a usage error.
    for flag in ["-r", "--restricted"] {
        let (code, stdout, stderr) = probe(&[flag, "-c", "echo probe-ok"]);
        assert_eq!(code, Some(0), "{flag} -c failed: {stderr}");
        assert!(
            stdout.contains("probe-ok"),
            "{flag} accepted but the command did not run: {stdout:?} / {stderr}"
        );
        assert!(
            !stderr.contains("invalid shell option name"),
            "{flag} was routed into the shopt space (niubash#385 regression): {stderr}"
        );
    }

    // The red lines bite: cd refused, PATH read-only.
    let (code, _, stderr) = probe(&["-r", "-c", "cd /"]);
    assert_ne!(code, Some(0), "restricted cd must not succeed");
    assert!(
        stderr.contains("cd: restricted"),
        "restricted cd diagnostic missing: {stderr}"
    );
    let (code, _, stderr) = probe(&["-r", "-c", "PATH=/x"]);
    assert_ne!(code, Some(0), "restricted PATH assignment must not succeed");
    assert!(
        stderr.contains("readonly"),
        "restricted PATH diagnostic missing: {stderr}"
    );
}
