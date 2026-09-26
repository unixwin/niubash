//! niu entry point (the niubash shell)
//!
//! Usage:
//!   niu                  → interactive REPL
//!   niu -c "command"     → execute one command, print exit code, exit
//!   niu -C "command"     → execute one REPL-style command, then exit
//!   niu script.sh        → execute a script file
//!   niu --help | -h      → usage
//!   niu --version        → version (niubash / rubash / winuxcmd)
//!   niu setup            → re-run the interactive prompt/plugin wizard
//!   niu plugin list [--json] → list official Niubash plugins
//!   niu plugin info <name> [--json] → inspect one official plugin
//!   niu plugin search [query] [--json] → discover official plugins
//!   niu plugin themes [--json] → list user and bundle themes
//!   niu plugin bundle status [--json] → inspect official bundle install state
//!   niu plugin doctor [--json] [--verbose] → diagnose plugin configuration health
//!   niu plugin review <name> [--json] → review plugin permissions
//!   niu plugin update oh-my-niu --from <path> → install a bundle release
//!   niu plugin update oh-my-niu --github-release latest → download/install bundle
//!   niu plugin rollback oh-my-niu → roll back to the previous bundle
//!   niu plugin add <url>[@ref] [name] → clone a third-party bundle (untrusted)
//!   niu plugin trust <name> → trust a third-party bundle
//!   niu plugin use <name> → activate a trusted third-party bundle
//!   niu plugin remove <name> → remove a third-party bundle
//!   niu --completion-probe "line" [cursor] → print REPL completions
//!   niu --install-wt-profile → add/update the Windows Terminal profile
//!   niu --self-update → download and run the latest installer
//!   self-update / update-niubash → REPL commands for Niubash self-update

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use rubash::invocation::ShellInvocation;

mod self_update;
const OFFICIAL_PLUGIN_BUNDLE_REPO: &str = "unixwin/oh-my-niu";
const PLUGIN_BUNDLE_DOWNLOAD_CACHE: &str = "niubash-plugin-bundles";
// GNU variables.c FUNCNEST: 0/unset means no limit, so recursion depth is
// bounded only by the real stack. Debug frames in the engine's call chain
// run ~150KB each; 512MiB (reserved, not committed) covers func4.sub's
// FUNCNEST=0 recursion to f=201 with headroom — mirrors rubash's main.rs.
const NIU_MAIN_STACK_SIZE: usize = 512 * 1024 * 1024;

fn main() -> ExitCode {
    // Restore the console (raw mode, cursor) on the panic path before the
    // default hook reports; with `panic = "abort"` this is the last code
    // that runs because no Drop guards execute.
    niubash_runtime::panic_restore::install_panic_hook();
    std::thread::Builder::new()
        .name("niu-main".to_string())
        .stack_size(NIU_MAIN_STACK_SIZE)
        .spawn(run_main)
        .expect("spawn niubash main thread")
        .join()
        .unwrap_or_else(|_| ExitCode::from(1))
}

fn run_main() -> ExitCode {
    // Initialize logging (only error level by default)
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Error)
        .parse_env("RUST_LOG")
        .init();

    // Install Ctrl+C handler (best-effort)
    niubash_runtime::ctrl_c::install();
    niubash_runtime::console_guard::prefer_utf8_code_page();
    niubash_runtime::console_guard::enable_vt_output();

    // Expose the host binary path so rubash's bash shim can forward to niu.
    // WINUXSH_SHELL is a deprecated bridge for current rubash upstream.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(path) = exe.to_str() {
            std::env::set_var("NIU_SHELL", path);
            std::env::set_var("WINUXSH_SHELL", path);
        }
    }

    let args: Vec<String> = std::env::args().collect();
    if let Some(name) = args
        .get(1)
        .and_then(|arg| arg.strip_prefix("--internal-"))
        .filter(|name| matches!(*name, "yes" | "head" | "wc"))
    {
        run_internal_pipeline_utility(name, &args[2..]);
    }

    if let Err(e) = run(&args) {
        if is_broken_pipe_error(&e) {
            return ExitCode::from(1);
        }
        eprintln!("niu: {}", e);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 2 {
        return if niubash_runtime::terminal::stdio_is_interactive() {
            run_repl()
        } else {
            run_stdin_script()
        };
    }

    let first = &args[1];
    // GNU shell.c parse_shell_options: every argv word starting with '-' or
    // '+' is shell-option syntax (-c/-i/-o/-O/+B/+o ...), never a script
    // name. Route all of them through the engine's ShellInvocation parser —
    // a rejected option is a usage error, not "No such file or directory".
    if (first.starts_with('-') || first.starts_with('+'))
        && !matches!(
            first.as_str(),
            "-h" | "--help"
                | "-V"
                | "--version"
                | "-C"
                | "--repl-command"
                | "--gitstatus-daemon"
                | "--completion-probe"
                | "--install-wt-profile"
                | "--self-update"
        )
        && !legacy_command_mode_has_post_c_login_flag(args)
    {
        // P3 invocation alignment: a leading-dash argument the engine parser
        // rejects is a usage error with the GNU surface (shell.c:874-881):
        // "<shell>: <option>: invalid option" + usage block, rc 2
        // (EX_BADUSAGE). The engine (rubash main.rs) reports under the
        // literal "bash" name so the upstream invocation suite normalizes
        // byte-for-byte; keep that convention.
        return match ShellInvocation::parse(&args[1..]) {
            Ok(_) => run_shell_invocation(&args[1..]),
            Err(message) => {
                eprintln!("bash: {message}");
                if message.contains("invalid option") {
                    show_shell_usage();
                }
                std::process::exit(2);
            }
        };
    }
    match first.as_str() {
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "--version" | "-V" => {
            print_version();
            Ok(())
        }
        "--gitstatus-daemon" => niubash_runtime::git_status::run_daemon_stdio(),
        "--completion-probe" => {
            print_completion_probe(args)?;
            Ok(())
        }
        "--install-wt-profile" => {
            install_windows_terminal_profile(args)?;
            Ok(())
        }
        "--self-update" => self_update::run(&args[2..]),
        "setup" | "configure" => match setup_preset_arg(&args[2..]) {
            Some(name) => niubash_runtime::setup_wizard::apply_preset(&name),
            None => niubash_runtime::setup_wizard::rerun_wizard(),
        },
        "font" => niubash_runtime::fonts::run_font_command(),
        "doctor" => niubash_runtime::doctor::run_doctor(),
        "plugin" => run_plugin_command(args),
        "-C" | "--repl-command" => run_repl_command(args),
        "-c" => {
            let command_mode = parse_legacy_command_mode(args)?;
            let mut shell = niubash_runtime::Shell::new()?;
            niubash_runtime::startup_trace::tick("-c: Shell::new");
            shell.executor.inherit_process_stdin();
            shell.enable_process_stdin_pipeline_bridge();
            shell
                .executor
                .set_env("BASH_EXECUTION_STRING", command_mode.command);
            if let Some(command_name) = command_mode.command_name {
                shell.set_script_name(command_name);
                shell
                    .executor
                    .set_positional_params(command_mode.positional_params.to_vec());
            }
            let code = shell.execute_script(command_mode.command)?;
            niubash_runtime::startup_trace::tick("-c: execute_script");
            let code = shell.finish_with_exit_trap(code)?;
            niubash_runtime::startup_trace::tick("-c: exit trap");
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        _ => {
            // Treat as a script file to execute
            let mut shell = niubash_runtime::Shell::new()?;
            // GNU shell.c:1572-1601 (open_shell_script): the script name is
            // tried as given; when that fails and the name has no path
            // separator it is searched in $PATH (findcmd.c find_path_file) —
            // that is how `bash ls` finds and then refuses the binary
            // /usr/bin/ls.
            let mut script = script_arg_to_host_path(first);
            if !script.exists() && !first.contains('/') && !first.contains('\\') {
                if let Some(found) = shell.executor.find_script_on_path(first) {
                    script = found;
                }
            }
            if !script.exists() {
                // shell.c shell_execve on a name that is neither option,
                // builtin, nor file: ENOENT surface, EX_NOTFOUND (127).
                eprintln!("niu: {}: No such file or directory", first);
                std::process::exit(127);
            }
            // general.c:718-741 check_binary_file: NUL in the first line(s)
            // or an ELF image is refused with EX_BINARY_FILE (126).
            let bytes = std::fs::read(&script)?;
            if rubash::script_driver::check_binary_file(&bytes)
                || std::str::from_utf8(&bytes).is_err()
            {
                eprintln!("cannot execute binary file");
                std::process::exit(126);
            }
            let content = String::from_utf8(bytes).unwrap_or_default();
            shell.set_script_name(first);
            shell.executor.inherit_process_stdin();
            shell.enable_process_stdin_pipeline_bridge();
            shell.source_non_interactive_env();
            shell.executor.set_positional_params(args[2..].to_vec());
            let code = shell.execute_script(&content)?;
            let code = shell.finish_with_exit_trap(code)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
    }
}

struct LegacyCommandMode<'a> {
    command: &'a str,
    command_name: Option<&'a str>,
    positional_params: &'a [String],
}

fn parse_legacy_command_mode(args: &[String]) -> anyhow::Result<LegacyCommandMode<'_>> {
    let mut index = 2;
    while matches!(args.get(index).map(String::as_str), Some("-l" | "--login")) {
        index += 1;
    }
    let Some(command) = args.get(index) else {
        anyhow::bail!("-c requires an argument");
    };
    let command_name = args.get(index + 1).map(String::as_str);
    let positional_params = args.get(index + 2..).unwrap_or(&[]);
    Ok(LegacyCommandMode {
        command,
        command_name,
        positional_params,
    })
}

fn legacy_command_mode_has_post_c_login_flag(args: &[String]) -> bool {
    matches!(args.get(1).map(String::as_str), Some("-c"))
        && matches!(args.get(2).map(String::as_str), Some("-l" | "--login"))
}

fn run_shell_invocation(args: &[String]) -> anyhow::Result<()> {
    // Read before Shell::new overwrites the process variable (shell name
    // setup writes BASH_ARGV0 back into the environment).
    let inherited_argv0 = std::env::var("BASH_ARGV0").ok().filter(|v| !v.is_empty());
    let invocation =
        ShellInvocation::parse(args).map_err(|error| anyhow::anyhow!("niu: {}", error))?;

    if invocation.dump_strings {
        let input = invocation_input(&invocation)?;
        let source_name = invocation_source_name(&invocation);
        print_locale_strings(&input, invocation.dump_po, &source_name);
        return Ok(());
    }
    if invocation.pretty_print {
        let input = invocation_input(&invocation)?;
        pretty_print_script(&input);
        return Ok(());
    }

    let mut shell = if invocation.read_stdin {
        niubash_runtime::Shell::new_for_stdin_script()?
    } else {
        niubash_runtime::Shell::new()?
    };
    niubash_runtime::startup_trace::tick("invocation: Shell::new");
    shell.no_rc = invocation.no_rc;
    shell.no_profile = invocation.no_profile;
    shell.rc_file = invocation.rc_file.clone().map(PathBuf::from);
    shell.no_editing = invocation.no_editing;
    invocation
        .apply_to_executor(&mut shell.executor)
        .map_err(|error| {
            // shell.c reports a bad -o/-O option name through the line-0
            // diagnostic ("bash: line 0: badopt: invalid shell option name").
            if error.contains("invalid shell option name") {
                eprintln!("bash: line 0: {error}");
                std::process::exit(2);
            }
            anyhow::anyhow!("niu: {error}")
        })?;
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();

    if let Some(command) = invocation.command {
        shell.source_non_interactive_env();
        niubash_runtime::startup_trace::tick("invocation: setup done");
        // GNU shell.c: $0 for -c is the word after the command string, or
        // $BASH_ARGV0 from the environment when exported by the caller.
        if let Some(argv0) = inherited_argv0.clone() {
            shell.set_script_name(&argv0);
        } else if let Some(name) = invocation.command_name.clone() {
            shell.set_script_name(&name);
        }
        shell.executor.set_env("BASH_EXECUTION_STRING", &command);
        let code = shell.execute_script(&command)?;
        niubash_runtime::startup_trace::tick("invocation: execute_script");
        let code = shell.finish_with_exit_trap(code)?;
        niubash_runtime::startup_trace::tick("invocation: exit trap");
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if let Some(script_name) = invocation.script {
        shell.source_non_interactive_env();
        shell.set_script_name(&script_name);
        let content = std::fs::read_to_string(script_arg_to_host_path(&script_name))?;
        let code = shell.execute_script(&content)?;
        let code = shell.finish_with_exit_trap(code)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    // GNU bash -i with a non-tty stdin still drives readline
    // (parse.y yy_readline_get -> bashline.c bash_readline): prompts and the
    // input echo go to stderr, editing keys are honored, and every command
    // is recorded to engine history. The reedline product REPL cannot run
    // without a terminal, so delegate to the engine's interactive stdin
    // driver instead of enter_interactive()/run_repl.
    if invocation.interactive && !niubash_runtime::terminal::stdio_is_interactive() {
        shell.executor.set_env("__RUBASH_INTERACTIVE", "1");
        shell.executor.set_shopt_option("expand_aliases", true);
        rubash::script_driver::prepare_interactive_history(&mut shell.executor);
        let code = rubash::script_driver::run_interactive_stdin(&mut shell.executor);
        std::process::exit(code);
    }
    // Bash -i forces an interactive shell even when stdin is not a terminal;
    // with no command or script, a terminal (or -i) means the REPL.
    if invocation.interactive || niubash_runtime::terminal::stdio_is_interactive() {
        shell.enter_interactive();
        return niubash_runtime::repl::run_repl(shell);
    }
    shell.source_non_interactive_env();
    let mut content = String::new();
    std::io::stdin().read_to_string(&mut content)?;
    let code = shell.execute_script(&content)?;
    let code = shell.finish_with_exit_trap(code)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn invocation_input(invocation: &ShellInvocation) -> anyhow::Result<String> {
    if let Some(command) = &invocation.command {
        return Ok(command.clone());
    }
    if let Some(script_name) = &invocation.script {
        let path = script_arg_to_host_path(script_name);
        return Ok(std::fs::read_to_string(&path)?);
    }
    let mut content = String::new();
    std::io::stdin().read_to_string(&mut content)?;
    Ok(content)
}

/// -D / --dump-strings: list every locale string ($"...") without executing,
/// the way GNU bash's dump-strings option does. --dump-po-strings selects the
/// GNU gettext PO output format.
///
/// GNU recognizes a locale string only where the word lexer reads `$` followed
/// by `"` while scanning a word (parse.y read_token_word's
/// `character == '$' && peek_char == '"'` branch; the dump itself is
/// locale.c locale_expand: printf("\"%s\"\n")). Comment text and here-doc
/// bodies are never lexed as words, so they never dump; single-quoted text,
/// double-quoted spans and backtick bodies are skipped as units; word-
/// embedded, quoted and arithmetic-embedded command substitutions are
/// re-lexed, so locale strings inside them do dump (parse.y:4100 processes
/// `$(` units encountered inside a matched pair).
///
/// This pass therefore walks the rubash token stream -- which already
/// excludes the comment and here-doc-body classes structurally, since the
/// lexer never yields word-shaped tokens from them -- and applies the
/// word-level quote rules to the raw spelling of word-shaped tokens. The
/// old implementation byte-scanned the raw script instead and misfired in
/// exactly those positions.
fn print_locale_strings(input: &str, po: bool, source_name: &str) {
    let mut strings = Vec::new();
    collect_locale_strings(input, 1, &mut strings);
    print!("{}", render_locale_string_dump(&strings, po, source_name));
}

/// The `#: name:lineno` anchor GNU bash prints in --dump-po-strings entries
/// (locale.c locale_expand passes yy_input_name()): the script path as given
/// on argv, the literal `-c` for -c input, and the shell's own argv[0] for
/// standard input.
fn invocation_source_name(invocation: &ShellInvocation) -> String {
    if invocation.command.is_some() {
        return "-c".to_string();
    }
    if let Some(script) = &invocation.script {
        return script.clone();
    }
    std::env::args().next().unwrap_or_else(|| "niu".to_string())
}

/// Collects `(line, raw body)` for every locale string in `input`, in source
/// order. `base_line` is the line the token stream's own numbering starts
/// from: top-level tokens carry real script lines in `token.position`, while
/// a re-lexed substitution body restarts at 1, so nested strings are mapped
/// back with `base_line + position - 1`. GNU reports the physical line of
/// each nested string; the two agree whenever the substitution body starts
/// on its token's start line (the overwhelmingly common single-line word).
fn collect_locale_strings(input: &str, base_line: usize, out: &mut Vec<(usize, String)>) {
    for token in rubash::lexer::tokenize(input) {
        let line = base_line + token.position.saturating_sub(1);
        match token.kind {
            rubash::TokenKind::Word
            | rubash::TokenKind::Assignment
            | rubash::TokenKind::BraceExpand => scan_locale_words(&token.raw, line, out),
            rubash::TokenKind::CommandSubst => match substitution_span(&token.raw) {
                SubstitutionSpan::Command(body) => collect_locale_strings(&body, line, out),
                SubstitutionSpan::Arithmetic(body) => scan_arithmetic_text(&body, line, out),
                SubstitutionSpan::None => {}
            },
            _ => {}
        }
    }
}

/// Word-level scan of one token's raw spelling, mirroring where GNU's word
/// lexer recognizes `$"`: outside single quotes, double-quoted spans,
/// backtick bodies and `${...}`/`$'...'` units. `\"` at word level escapes
/// the next character, so `\$"x"` is not a locale string introducer.
fn scan_locale_words(raw: &str, line: usize, out: &mut Vec<(usize, String)>) {
    let bytes = raw.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => i = skip_single_quoted(bytes, i + 1),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'"' => i = scan_double_quoted(raw, i + 1, line, out),
            b'$' => match bytes.get(i + 1) {
                Some(b'"') => {
                    let close = locale_body_end(raw, i + 2, line, out);
                    out.push((line, raw[i + 2..close].to_string()));
                    i = close + 1;
                }
                Some(b'\'') => i = skip_ansi_c_quoted(bytes, i + 2),
                Some(b'{') => i = skip_dollar_brace(bytes, i + 2),
                Some(b'(') => i = scan_substitution_unit(raw, i + 1, line, out),
                _ => i += 1,
            },
            b'\\' => i = (i + 2).min(bytes.len()),
            _ => i += 1,
        }
    }
}

/// Byte index of the closing `"` of a locale string body whose text starts at
/// `start` (just past the opening quote). Nested `${...}`/`` `...` ``/`$(...)`
/// units are skipped the way GNU parse_matched_pair skips them while it
/// extracts the pair, and command-substitution units are re-lexed so their
/// own locale strings dump first (GNU order: inner before outer). The
/// surrounding body is reported verbatim: GNU additionally rewrites nested
/// `$"..."` units to `"..."` inside the body it dumps, which needs a
/// byte-exact body serializer rubash does not expose (host-semantic-layer
/// plan, C1 residual).
fn locale_body_end(raw: &str, start: usize, line: usize, out: &mut Vec<(usize, String)>) -> usize {
    let bytes = raw.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return i,
            b'\\' => i = (i + 2).min(bytes.len()),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'$' => match bytes.get(i + 1) {
                Some(b'{') => i = skip_dollar_brace(bytes, i + 2),
                Some(b'(') => i = scan_substitution_unit(raw, i + 1, line, out),
                _ => i += 1,
            },
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Walks a double-quoted span. `$` followed by `"` is literal data here (GNU
/// dumps nothing for `echo "$"dqp" tail"`), while `$(...)` units are re-lexed
/// (parse.y:4100) and their locale strings dump.
fn scan_double_quoted(
    raw: &str,
    start: usize,
    line: usize,
    out: &mut Vec<(usize, String)>,
) -> usize {
    let bytes = raw.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return i + 1,
            b'\\' => i = (i + 2).min(bytes.len()),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'$' => match bytes.get(i + 1) {
                Some(b'{') => i = skip_dollar_brace(bytes, i + 2),
                Some(b'(') => i = scan_substitution_unit(raw, i + 1, line, out),
                _ => i += 1,
            },
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Extracts the span of a `$(...)` / `$((...))` unit whose `(` sits at `open`
/// and dispatches it: command-substitution bodies are re-lexed (GNU dumps
/// their locale strings, unquoted, word-embedded or double-quoted alike) and
/// arithmetic bodies keep dumping only through the quoted command
/// substitutions they contain.
fn scan_substitution_unit(
    raw: &str,
    open: usize,
    line: usize,
    out: &mut Vec<(usize, String)>,
) -> usize {
    let bytes = raw.as_bytes();
    let Some(close) = paren_close(bytes, open) else {
        return bytes.len();
    };
    if bytes.get(open + 1) == Some(&b'(') {
        scan_arithmetic_text(&raw[open + 2..close - 1], line, out);
    } else {
        collect_locale_strings(&raw[open + 1..close], line, out);
    }
    close + 1
}

/// Arithmetic text (`$(( ... ))` inner span): `$"..."` never fires here, but
/// quoted command substitutions are parsed by GNU and their locale strings
/// dump (GNU 5.3.0: `$(( "$(echo $"x")" + 1 ))` dumps `"x"`).
fn scan_arithmetic_text(text: &str, line: usize, out: &mut Vec<(usize, String)>) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => i = scan_double_quoted(text, i + 1, line, out),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                if bytes.get(i + 2) == Some(&b'(') {
                    // Nested arithmetic span: no locale recognition inside.
                    i += 3;
                } else {
                    i = scan_substitution_unit(text, i + 1, line, out);
                }
            }
            b'\\' => i = (i + 2).min(bytes.len()),
            _ => i += 1,
        }
    }
}

enum SubstitutionSpan {
    Command(String),
    Arithmetic(String),
    None,
}

/// Classifies a CommandSubst token's raw spelling: a `` `...` `` token shares
/// the kind but never starts with `$(`, and its body must not be re-lexed
/// (GNU keeps backtick bodies verbatim at parse time, so `echo `echo $"x"``
/// dumps nothing). `$((...))` yields its arithmetic inner span.
fn substitution_span(raw: &str) -> SubstitutionSpan {
    let bytes = raw.as_bytes();
    if bytes.first() != Some(&b'$') || bytes.get(1) != Some(&b'(') {
        return SubstitutionSpan::None;
    }
    let Some(close) = paren_close(bytes, 1) else {
        return SubstitutionSpan::None;
    };
    if bytes.get(2) == Some(&b'(') {
        SubstitutionSpan::Arithmetic(raw[3..close - 1].to_string())
    } else {
        SubstitutionSpan::Command(raw[2..close].to_string())
    }
}

/// Byte index of the `)` matching the `(` at `open`, honoring quoting the way
/// GNU parse_matched_pair does while it extracts a substitution span. Returns
/// None when the span never closes; callers then treat the rest of the text
/// as the unit, which keeps the scan total on malformed input.
fn paren_close(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i - 1);
                }
            }
            b'\'' => i = skip_single_quoted(bytes, i + 1),
            b'"' => i = skip_double_span(bytes, i + 1),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'\\' => i = (i + 2).min(bytes.len()),
            _ => i += 1,
        }
    }
    None
}

fn skip_single_quoted(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_ansi_c_quoted(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => return i + 1,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn skip_backquoted(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'`' => return i + 1,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    bytes.len()
}

fn skip_double_span(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return i + 1,
            b'\\' => i = (i + 2).min(bytes.len()),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'$' if bytes.get(i + 1) == Some(&b'{') => i = skip_dollar_brace(bytes, i + 2),
            _ => i += 1,
        }
    }
    bytes.len()
}

fn skip_dollar_brace(bytes: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            b'\'' => i = skip_single_quoted(bytes, i + 1),
            b'"' => i = skip_double_span(bytes, i + 1),
            b'`' => i = skip_backquoted(bytes, i + 1),
            b'\\' => i = (i + 2).min(bytes.len()),
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Renders collected locale strings exactly the way GNU bash prints them
/// (locale.c locale_expand): plain mode is `"body"` with the raw body text
/// verbatim (escapes stay escaped, embedded newlines split the output line),
/// and PO mode is the mk_msgstr form anchored by `#: name:lineno`.
fn render_locale_string_dump(strings: &[(usize, String)], po: bool, source_name: &str) -> String {
    let mut out = String::new();
    for (line, body) in strings {
        if po {
            let mut escaped = String::new();
            let mut multiline = false;
            for ch in body.chars() {
                match ch {
                    '\n' => {
                        escaped.push_str("\\n\"\n\"");
                        multiline = true;
                    }
                    '"' | '\\' => {
                        escaped.push('\\');
                        escaped.push(ch);
                    }
                    _ => escaped.push(ch),
                }
            }
            if multiline {
                out.push_str(&format!(
                    "#: {source_name}:{line}\nmsgid \"\"\n\"{escaped}\"\nmsgstr \"\"\n"
                ));
            } else {
                out.push_str(&format!(
                    "#: {source_name}:{line}\nmsgid \"{escaped}\"\nmsgstr \"\"\n"
                ));
            }
        } else {
            out.push('"');
            out.push_str(body);
            out.push_str("\"\n");
        }
    }
    out
}

/// --pretty-print: GNU pretty_print_loop (eval.c:215-253) reads one command
/// at a time: a blank input line ends the current command, an empty parse
/// prints one newline (suppressed right after another newline), and each
/// parsed command prints as its canonical text plus one newline. Mirrors the
/// engine's rubash main.rs implementation over the public parser API.
fn pretty_print_script(input: &str) {
    let posix = std::env::var("__RUBASH_POSIX_MODE").as_deref() == Ok("1");
    let mut output = String::new();
    let mut pending = String::new();
    let mut last_was_newline = false;
    for line in input.lines() {
        if line.trim().is_empty() && !rubash::lexer::has_unclosed_input_syntax(&pending) {
            last_was_newline =
                flush_pretty_print_chunk(&pending, posix, &mut output, last_was_newline);
            pending.clear();
            if !last_was_newline {
                output.push('\n');
                last_was_newline = true;
            }
            continue;
        }
        if !pending.is_empty() {
            pending.push('\n');
        }
        pending.push_str(line);
    }
    last_was_newline = flush_pretty_print_chunk(&pending, posix, &mut output, last_was_newline);
    if !last_was_newline && !output.is_empty() {
        output.push('\n');
    }
    print!("{output}");
}

fn flush_pretty_print_chunk(
    chunk: &str,
    posix: bool,
    output: &mut String,
    last_was_newline: bool,
) -> bool {
    let tokens = rubash::lexer::tokenize_with_initial_posix(chunk, posix);
    let ast = rubash::parser::parse(&tokens);
    let mut printed = false;
    for command in &ast.commands {
        if is_pretty_print_empty(command) {
            continue;
        }
        output.push_str(&rubash::parser::ast_print::pretty_print_command(command));
        output.push('\n');
        printed = true;
    }
    if printed {
        return false;
    }
    last_was_newline
}

fn is_pretty_print_empty(command: &rubash::parser::CommandNode) -> bool {
    command.words.is_empty()
        && command.assignments.is_empty()
        && command.compound_assignments.is_empty()
        && command.array_element_assignments.is_empty()
        && command.for_command.is_none()
        && command.select_command.is_none()
        && command.loop_command.is_none()
        && command.if_command.is_none()
        && command.case_command.is_none()
        && command.function_command.is_none()
        && command.arithmetic_command.is_none()
        && command.conditional_command.is_none()
        && command.coproc_command.is_none()
        && command.brace_group.is_none()
        && command.pipeline_command.is_none()
        && command.and_or_list.is_none()
}

fn script_arg_to_host_path(value: &str) -> PathBuf {
    if cfg!(windows) {
        let normalized = value.replace('\\', "/");
        let bytes = normalized.as_bytes();
        if bytes.len() >= 2
            && bytes[0] == b'/'
            && bytes[1].is_ascii_alphabetic()
            && (bytes.len() == 2 || bytes.get(2) == Some(&b'/'))
        {
            let drive = (bytes[1] as char).to_ascii_uppercase();
            let rest = if normalized.len() == 2 {
                "/"
            } else {
                &normalized[2..]
            };
            return PathBuf::from(format!("{drive}:{rest}"));
        }
    }

    PathBuf::from(value)
}

fn run_repl() -> anyhow::Result<()> {
    self_update::maybe_print_update_hint();
    let mut shell = niubash_runtime::Shell::new()?;
    shell.enter_interactive();
    niubash_runtime::repl::run_repl(shell)
}

fn run_repl_command(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 3 {
        anyhow::bail!("{} requires an argument", args[1]);
    }
    if let Some(self_update_args) = niubash_runtime::repl::self_update_command_args(&args[2]) {
        if let Some(code) = niubash_runtime::repl::spawn_self_update(&self_update_args) {
            std::process::exit(code);
        }
    }
    let mut shell = niubash_runtime::Shell::new()?;
    niubash_runtime::startup_trace::tick("-C: Shell::new");
    shell.enter_interactive();
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();
    if let Some(command_name) = args.get(3) {
        shell.set_script_name(command_name);
        shell.executor.set_positional_params(args[4..].to_vec());
    }
    shell.run_startup_rc();
    niubash_runtime::startup_trace::tick("-C: startup rc");
    shell.run_precmd_hooks();
    niubash_runtime::startup_trace::tick("-C: precmd hooks");
    let code = shell.execute_interactive_line(&args[2])?;
    niubash_runtime::startup_trace::tick("-C: execute_interactive_line");
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn run_stdin_script() -> anyhow::Result<()> {
    let mut shell = niubash_runtime::Shell::new_for_stdin_script()?;
    shell.executor.inherit_process_stdin();
    shell.source_non_interactive_env();
    let mut line = String::new();
    let mut pending = Vec::new();

    loop {
        line.clear();
        // GNU input.c bash_input binds the script reader to fd 0: a
        // permanent `exec 0<file` (redir.c do_redirections) moves the
        // script source to the new input. read_unbuffered_line on the raw
        // fd also avoids StdinLock prefetch stealing bytes from `&`
        // children that inherit fd 0 (redir1.sub, redir.tests heredocs).
        match shell
            .executor
            .script_fd0_line(&mut line)
            .map(Ok)
            .unwrap_or_else(|| read_unbuffered_line(&mut line))?
        {
            0 => {
                if !pending.is_empty() {
                    let code = shell.execute_script(&pending.join("\n"))?;
                    let code = shell.finish_with_exit_trap(code)?;
                    if code != 0 {
                        std::process::exit(code);
                    }
                }
                break;
            }
            _ => {}
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if pending.is_empty() && line.trim().is_empty() {
            continue;
        }
        pending.push(line.to_string());
        let script = pending.join("\n");
        if !niubash_runtime::repl::is_script_input_complete(&script) {
            continue;
        }

        let code = match shell.stdin_current_shell_child(&script) {
            Some(child) => {
                let mut child_stdin = String::new();
                let _ = read_unbuffered_line(&mut child_stdin)?;
                shell.execute_stdin_current_shell_child(child, &child_stdin)?
            }
            None => shell.execute_script(&script)?,
        };
        if code != 0 {
            let code = shell.finish_with_exit_trap(code)?;
            std::process::exit(code);
        }
        pending.clear();
    }

    let code = shell.finish_with_exit_trap(0)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn read_unbuffered_line(output: &mut String) -> std::io::Result<usize> {
    let mut stdin = std::io::stdin().lock();
    let mut bytes = [0_u8; 1];
    let mut read = 0;

    loop {
        match stdin.read(&mut bytes)? {
            0 => break,
            count => {
                read += count;
                output.push(bytes[0] as char);
                if bytes[0] == b'\n' {
                    break;
                }
            }
        }
    }

    Ok(read)
}

fn run_internal_pipeline_utility(name: &str, args: &[String]) -> ! {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    match name {
        "yes" => {
            let line = if args.is_empty() {
                "y".to_string()
            } else {
                args.join(" ")
            };
            let chunk = format!("{line}\n").repeat(256);
            loop {
                if stdout.write_all(chunk.as_bytes()).is_err() || stdout.flush().is_err() {
                    std::process::exit(0);
                }
            }
        }
        "head" => {
            let count = internal_head_line_count(args).unwrap_or(10);
            let mut input = std::io::BufReader::new(stdin.lock());
            let mut line = Vec::new();
            for _ in 0..count {
                line.clear();
                match input.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if stdout.write_all(&line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stdout.flush();
            std::process::exit(0);
        }
        "wc" => {
            let mut input = stdin.lock();
            let mut buffer = [0_u8; 8192];
            let mut lines = 0usize;
            loop {
                match input.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(size) => {
                        lines += buffer[..size].iter().filter(|byte| **byte == b'\n').count()
                    }
                    Err(_) => break,
                }
            }
            let _ = writeln!(stdout, "{lines}");
            std::process::exit(0);
        }
        _ => std::process::exit(127),
    }
}

fn internal_head_line_count(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if arg == "-n" {
            return args.get(index + 1)?.parse().ok();
        }
        if let Some(value) = arg.strip_prefix("-n") {
            if !value.is_empty() {
                return value.parse().ok();
            }
        }
        if let Some(value) = arg.strip_prefix('-') {
            if !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()) {
                return value.parse().ok();
            }
        }
        if let Some(value) = arg.strip_prefix("--lines=") {
            return value.parse().ok();
        }
        index += 1;
    }
    None
}

/// GNU shell.c show_shell_usage (shell.c:2056-2103) with extra=0: the usage
/// block the upstream invocation suite expects after an invalid option. The
/// "bash" spelling is the engine convention (rubash main.rs) so the suite's
/// `sed 's|^.*/bash|bash|'` normalization matches byte for byte.
fn show_shell_usage() {
    eprint!(
        "bash [GNU long option] [option] ...
bash [GNU long option] [option] script-file ...
"
    );
    eprintln!("GNU long options:");
    for name in LONG_OPTIONS {
        eprintln!("	--{name}");
    }
    eprintln!("Shell options:");
    eprintln!("	-ilrsD or -c command or -O shopt_option		(invocation only)");
    eprintln!("	-abefhkmnptuvxBCEHPT or -o option");
}

const LONG_OPTIONS: &[&str] = &[
    "debug",
    "debugger",
    "dump-po-strings",
    "dump-strings",
    "help",
    "init-file",
    "login",
    "noediting",
    "noprofile",
    "norc",
    "posix",
    "pretty-print",
    "rcfile",
    "restricted",
    "verbose",
    "version",
];

fn print_usage() {
    println!(
        "Niubash {} \u{2014} a bash-compatible shell that feels at home on Windows.",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage:  niu [option]");
    println!("        niu -c <cmd>         Run a command then exit");
    println!("        niu -C <cmd>         Run one REPL-style command then exit");
    println!("        niu setup           Re-run prompt/plugin setup");
    println!("        niu font            Install a Nerd Font for icon-rich themes");
    println!("        niu doctor          Health-check the installation");
    println!("        niu <script> [args]  Run a script file");
    println!();
    println!("Options:");
    println!("  -h, --help                Show this help");
    println!("  -V, --version             Version and component info");
    println!("  -c <command>              Execute a command ad-hoc");
    println!("  -C, --repl-command <cmd>  Execute one non-interactive REPL command");
    println!();
    println!("  --install-wt-profile      Add/update the Windows Terminal profile");
    println!("      --set-default         Also set Niubash as the WT default profile");
    println!("      --quiet               Suppress non-error profile output");
    println!("  --self-update             Download and run the latest release installer");
    println!("      --check               Only report the latest release");
    println!("      --dry-run             Download installer without running it");
    println!("  self-update               REPL command: update Niubash and exit this shell");
    println!("  update-niubash            Alias for self-update");
    println!();
    println!("  plugin list [--json] [--verbose]");
    println!("                            List plugins (human view; --verbose adds diagnostics)");
    println!("  plugin info <name> [--json] [--verbose]");
    println!("                            Inspect one official Niubash plugin");
    println!("  plugin search [query] [--json]  Discover official plugins");
    println!("  plugin themes [--json]    List user and bundle themes");
    println!("  plugin bundle status [--json] [--verbose]");
    println!("                            Inspect official bundle install state");
    println!("  plugin update oh-my-niu --from <path>");
    println!("      [--checksum <sha>|--checksum-file <path>] [--json]");
    println!("  plugin update oh-my-niu --github-release latest|vX.Y.Z [--json]");
    println!("                            Install bundle release");
    println!("  plugin rollback oh-my-niu [--json]  Roll back bundle release");
    println!("  plugin add <url>[@ref] [name]  Clone a third-party bundle (untrusted)");
    println!("  plugin trust <name>       Trust a third-party bundle");
    println!("  plugin use <name>         Activate a trusted third-party bundle");
    println!("  plugin remove <name>      Remove a third-party bundle");
    println!();
    println!("  --completion-probe <line> [cursor]  Debug: print completion candidates");
    println!();
    println!("Configuration: ~/.niubashrc for interactive startup; a pre-rename ~/.winuxshrc is migrated once into ~/.niubashrc");
    println!();
    println!("Environment:");
    println!(
        "  NIU_ENV=<file>          Non-interactive init file sourced by -c, scripts, and stdin"
    );
    println!(
        "                          before running the command (bash BASH_ENV is also honored,"
    );
    println!(
        "                          NIU_ENV takes precedence). Unset by default, keeping -c fast."
    );
    println!("  BASH_ENV=<file>         GNU bash compatible: same as NIU_ENV, lower precedence.");
    println!("  NIU_LANG=<lang>         Setup wizard language (zh / en). Falls back to the");
    println!("                          Windows UI language, then LC_ALL/LANG.");
}

fn run_plugin_command(args: &[String]) -> anyhow::Result<()> {
    let Some(subcommand) = args.get(2) else {
        print_plugin_usage();
        return Ok(());
    };

    match subcommand.as_str() {
        "-h" | "--help" => {
            print_plugin_usage();
            Ok(())
        }
        "list" => {
            let rest = &args[3..];
            let json = rest.iter().any(|arg| arg == "--json");
            let verbose = rest.iter().any(|arg| arg == "--verbose");
            if json {
                println!("{}", niubash_runtime::plugins::plugin_packs_json()?);
            } else {
                println!(
                    "{}",
                    niubash_runtime::plugins::plugin_packs_text_verbose(verbose)
                );
            }
            Ok(())
        }
        "search" => run_plugin_search_command(&args[3..]),
        "themes" => run_plugin_themes_command(&args[3..]),
        "info" => {
            let Some(name) = args.get(3) else {
                anyhow::bail!("plugin info requires a plugin name");
            };
            let rest = &args[4..];
            let json = rest.iter().any(|arg| arg == "--json");
            let verbose = rest.iter().any(|arg| arg == "--verbose");
            if json {
                match niubash_runtime::plugins::plugin_pack_json(name)? {
                    Some(output) => println!("{}", output),
                    None => anyhow::bail!("unknown plugin '{}'", name),
                }
            } else {
                match niubash_runtime::plugins::plugin_pack_text_verbose(name, verbose) {
                    Some(output) => println!("{}", output),
                    None => anyhow::bail!("unknown plugin '{}'", name),
                }
            }
            Ok(())
        }
        "bundle" => run_plugin_bundle_command(&args[3..]),
        "doctor" => run_plugin_doctor_command(&args[3..]),
        "review" => run_plugin_review_command(&args[3..]),
        "update" => run_plugin_update_command(&args[3..]),
        "rollback" => run_plugin_rollback_command(&args[3..]),
        "add" => run_plugin_add_command(&args[3..]),
        "trust" => run_plugin_trust_command(&args[3..]),
        "use" => run_plugin_use_command(&args[3..]),
        "remove" => run_plugin_remove_command(&args[3..]),
        "enable" => run_plugin_enable_command(&args[3..]),
        "disable" => run_plugin_disable_command(&args[3..]),
        unknown => anyhow::bail!("unknown plugin subcommand '{}'", unknown),
    }
}

fn run_plugin_add_command(args: &[String]) -> anyhow::Result<()> {
    let Some(url) = args.first() else {
        anyhow::bail!("plugin add requires a git url: niu plugin add <url>[@ref] [name]");
    };
    let name = args.get(1).map(String::as_str);
    let record = niubash_runtime::plugins::external::add_bundle(url, name)?;
    println!(
        "{} '{}' into {}",
        niubash_runtime::text_style::green("Cloned"),
        record.name,
        niubash_runtime::text_style::dim(&record.path.display().to_string())
    );
    println!("the bundle is untrusted; review it, then run:");
    println!("  niu plugin trust {}", record.name);
    println!("  niu plugin use {}", record.name);
    Ok(())
}

fn run_plugin_trust_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("plugin trust requires a bundle name");
    };
    let record = niubash_runtime::plugins::external::trust_bundle(name)?;
    println!(
        "{} external bundle '{}' is now trusted",
        niubash_runtime::text_style::green("Trusted:"),
        record.name
    );
    println!("activate it with: niu plugin use {}", record.name);
    Ok(())
}

fn run_plugin_use_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("plugin use requires a bundle name");
    };
    let path = niubash_runtime::plugins::activate_external_bundle(name)?;
    println!(
        "{} external bundle '{}' at {}",
        niubash_runtime::text_style::green("Active bundle:"),
        name,
        niubash_runtime::text_style::dim(&path.display().to_string())
    );
    println!("restart niu to load it; go back with niu plugin rollback");
    Ok(())
}

fn run_plugin_remove_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("plugin remove requires a bundle name");
    };
    let path = niubash_runtime::plugins::external::remove_bundle(name)?;
    println!(
        "{} external bundle '{}' ({})",
        niubash_runtime::text_style::green("Removed"),
        name,
        niubash_runtime::text_style::dim(&path.display().to_string())
    );
    Ok(())
}

fn run_plugin_enable_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("plugin enable requires a plugin name");
    };
    // Validate the pack exists in the active inventory so the user gets a
    // clear error instead of silently writing a bogus name into ~/.niubashrc.
    let inventory = niubash_runtime::plugins::active_plugin_inventory();
    if !inventory
        .packs
        .iter()
        .any(|pack| pack.name.eq_ignore_ascii_case(name))
    {
        anyhow::bail!(
            "unknown plugin '{}'; run `niu plugin list` to see available packs",
            name
        );
    }
    let path = niubash_runtime::plugins::enable_pack_in_rc(name)?;
    println!(
        "{} '{}' in {}",
        niubash_runtime::text_style::green("Enabled"),
        name,
        niubash_runtime::text_style::dim(&path.display().to_string())
    );
    println!("restart niu (or reload ~/.niubashrc) for the change to take effect");
    Ok(())
}

fn run_plugin_disable_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.first() else {
        anyhow::bail!("plugin disable requires a plugin name");
    };
    let inventory = niubash_runtime::plugins::active_plugin_inventory();
    if !inventory
        .packs
        .iter()
        .any(|pack| pack.name.eq_ignore_ascii_case(name))
    {
        anyhow::bail!(
            "unknown plugin '{}'; run `niu plugin list` to see available packs",
            name
        );
    }
    let is_default = inventory
        .packs
        .iter()
        .any(|pack| pack.name.eq_ignore_ascii_case(name) && pack.default);
    let path = niubash_runtime::plugins::disable_pack_in_rc(name, &inventory)?;
    println!(
        "{} '{}' in {}",
        niubash_runtime::text_style::green("Disabled"),
        name,
        niubash_runtime::text_style::dim(&path.display().to_string())
    );
    if is_default {
        println!(
            "{}",
            niubash_runtime::text_style::dim(
                "'{}' is on by default; the rc was rewritten with NIU_DISABLE_DEFAULT_PLUGINS=1 \
                 and the remaining active packs listed in NIU_PLUGINS."
            )
        );
    }
    println!("restart niu (or reload ~/.niubashrc) for the change to take effect");
    Ok(())
}

fn run_plugin_doctor_command(args: &[String]) -> anyhow::Result<()> {
    let json = args.iter().any(|arg| arg == "--json");
    let verbose = args.iter().any(|arg| arg == "--verbose");
    let config = niubash_runtime::config::load();
    let report = niubash_runtime::plugins::plugin_doctor_report(&config.plugins);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_doctor_text_verbose(&report, verbose)
        );
    }
    Ok(())
}

fn run_plugin_review_command(args: &[String]) -> anyhow::Result<()> {
    let Some(name) = args.get(0) else {
        anyhow::bail!("plugin review requires a plugin name");
    };
    let json = parse_plugin_json_flag(&args[1..])?;
    let config = niubash_runtime::config::load();
    let review = niubash_runtime::plugins::plugin_permission_review(name, &config.plugins)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&review)?);
    } else {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_permission_review_text(&review)
        );
    }
    Ok(())
}

fn run_plugin_search_command(args: &[String]) -> anyhow::Result<()> {
    let (query, json, verbose) = parse_plugin_search_args(args)?;
    if json {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_search_json(query.as_deref())?
        );
    } else {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_search_text(query.as_deref(), verbose)
        );
    }
    Ok(())
}

fn run_plugin_themes_command(args: &[String]) -> anyhow::Result<()> {
    let mut json = false;
    let mut verbose = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--verbose" => verbose = true,
            unknown => anyhow::bail!("unknown plugin option '{}'", unknown),
        }
    }
    if json {
        println!("{}", niubash_runtime::plugins::plugin_theme_catalog_json()?);
    } else {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_theme_catalog_text(verbose)
        );
    }
    Ok(())
}

fn run_plugin_bundle_command(args: &[String]) -> anyhow::Result<()> {
    let Some(subcommand) = args.get(0) else {
        anyhow::bail!("plugin bundle requires a subcommand: status");
    };

    match subcommand.as_str() {
        "status" => {
            let rest = &args[1..];
            let json = rest.iter().any(|arg| arg == "--json");
            let verbose = rest.iter().any(|arg| arg == "--verbose");
            if json {
                println!("{}", niubash_runtime::plugins::plugin_bundle_status_json()?);
            } else {
                println!(
                    "{}",
                    niubash_runtime::plugins::plugin_bundle_status_text_verbose(verbose)
                );
            }
            Ok(())
        }
        unknown => anyhow::bail!("unknown plugin bundle subcommand '{}'", unknown),
    }
}

fn run_plugin_update_command(args: &[String]) -> anyhow::Result<()> {
    let Some(bundle) = args.get(0) else {
        anyhow::bail!("plugin update requires a bundle name");
    };
    let options = parse_plugin_update_options(&args[1..])?;
    let checksum = match (options.checksum, options.checksum_file) {
        (Some(_), Some(_)) => anyhow::bail!("use only one of --checksum or --checksum-file"),
        (Some(checksum), None) => Some(checksum),
        (None, Some(path)) => Some(read_checksum_file(&path)?),
        (None, None) => None,
    };
    let github_release = options.github_release;
    let source_path = options.source_path;
    let (source_path, checksum, downloaded) = match (source_path, github_release) {
        (Some(_), Some(_)) => anyhow::bail!("use only one of --from or --github-release"),
        (Some(path), None) => (path, checksum, None),
        (None, Some(release)) => {
            if checksum.is_some() {
                anyhow::bail!(
                    "--github-release downloads and verifies the release .sha256; do not pass --checksum or --checksum-file"
                );
            }
            let downloaded = download_plugin_bundle_github_release(bundle, &release)?;
            let checksum = Some(downloaded.checksum.clone());
            (downloaded.archive_path.clone(), checksum, Some(downloaded))
        }
        (None, None) => anyhow::bail!(
            "plugin update requires --from <bundle-dir-or-zip> or --github-release latest|vX.Y.Z"
        ),
    };
    let summary = niubash_runtime::plugins::apply_plugin_bundle_update_from_path(
        bundle,
        &source_path,
        checksum.as_deref(),
    )?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        if let Some(downloaded) = downloaded {
            println!(
                "Downloaded GitHub release {} from {}",
                downloaded.tag, OFFICIAL_PLUGIN_BUNDLE_REPO
            );
            println!("Downloaded archive: {}", downloaded.archive_path.display());
            println!(
                "Downloaded checksum: {}",
                downloaded.checksum_path.display()
            );
        }
        println!(
            "{} bundle '{}' to {}",
            niubash_runtime::text_style::green("Updated"),
            summary.bundle,
            summary.version
        );
        println!(
            "Installed path: {}",
            niubash_runtime::text_style::dim(&summary.installed_path.display().to_string())
        );
        if let Some(previous_path) = summary.previous_path {
            println!("Previous path: {}", previous_path.display());
        }
        if let Some(checksum) = summary.checksum_sha256 {
            println!("SHA-256: {}", checksum);
        }
        println!("Lock file: {}", summary.lock_path.display());
    }
    Ok(())
}
fn run_plugin_rollback_command(args: &[String]) -> anyhow::Result<()> {
    let Some(bundle) = args.get(0) else {
        anyhow::bail!("plugin rollback requires a bundle name");
    };
    let json = parse_plugin_json_flag(&args[1..])?;
    let summary = niubash_runtime::plugins::apply_plugin_bundle_rollback(bundle)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "{} bundle '{}' to {}",
            niubash_runtime::text_style::green("Rolled back"),
            summary.bundle,
            summary.version
        );
        println!(
            "Active path: {}",
            niubash_runtime::text_style::dim(&summary.active_path.display().to_string())
        );
        if let Some(previous_path) = summary.previous_path {
            println!("Previous path: {}", previous_path.display());
        }
        println!("Lock file: {}", summary.lock_path.display());
    }
    Ok(())
}
#[derive(Default)]
struct PluginUpdateOptions {
    source_path: Option<PathBuf>,
    github_release: Option<String>,
    checksum: Option<String>,
    checksum_file: Option<PathBuf>,
    json: bool,
}
fn parse_plugin_update_options(args: &[String]) -> anyhow::Result<PluginUpdateOptions> {
    let mut options = PluginUpdateOptions::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    anyhow::bail!("--from requires a bundle directory or zip path");
                };
                options.source_path = Some(PathBuf::from(path));
            }
            "--checksum" => {
                i += 1;
                let Some(checksum) = args.get(i) else {
                    anyhow::bail!("--checksum requires a SHA-256 value");
                };
                options.checksum = Some(checksum.clone());
            }
            "--checksum-file" => {
                i += 1;
                let Some(path) = args.get(i) else {
                    anyhow::bail!("--checksum-file requires a path");
                };
                options.checksum_file = Some(PathBuf::from(path));
            }
            "--github-release" => {
                i += 1;
                let Some(release) = args.get(i) else {
                    anyhow::bail!("--github-release requires latest or a vX.Y.Z tag");
                };
                options.github_release = Some(release.clone());
            }
            "--json" => options.json = true,
            unknown => anyhow::bail!("unknown plugin update option '{}'", unknown),
        }
        i += 1;
    }
    Ok(options)
}
struct DownloadedPluginBundle {
    archive_path: PathBuf,
    checksum_path: PathBuf,
    checksum: String,
    tag: String,
}

fn download_plugin_bundle_github_release(
    bundle: &str,
    release: &str,
) -> anyhow::Result<DownloadedPluginBundle> {
    if bundle != niubash_runtime::plugins::OFFICIAL_BUNDLE_NAME {
        anyhow::bail!(
            "GitHub bundle updates are only supported for {}",
            niubash_runtime::plugins::OFFICIAL_BUNDLE_NAME
        );
    }
    let tag = resolve_plugin_bundle_release_tag(release)?;
    let version = tag.trim_start_matches('v');
    let asset_name = format!("{bundle}-{version}.zip");
    let checksum_name = format!("{asset_name}.sha256");
    let archive_path = self_update::download_github_release_asset(
        OFFICIAL_PLUGIN_BUNDLE_REPO,
        &tag,
        &asset_name,
        PLUGIN_BUNDLE_DOWNLOAD_CACHE,
    )?;
    let checksum_path = self_update::download_github_release_asset(
        OFFICIAL_PLUGIN_BUNDLE_REPO,
        &tag,
        &checksum_name,
        PLUGIN_BUNDLE_DOWNLOAD_CACHE,
    )?;
    let checksum = read_checksum_file(&checksum_path)?;
    Ok(DownloadedPluginBundle {
        archive_path,
        checksum_path,
        checksum,
        tag,
    })
}

fn resolve_plugin_bundle_release_tag(release: &str) -> anyhow::Result<String> {
    let release = release.trim();
    if release.eq_ignore_ascii_case("latest") {
        return self_update::resolve_latest_github_release_tag(OFFICIAL_PLUGIN_BUNDLE_REPO);
    }
    normalize_plugin_bundle_release_tag(release)
}

fn normalize_plugin_bundle_release_tag(release: &str) -> anyhow::Result<String> {
    let version = release.strip_prefix('v').unwrap_or(release);
    let parts: Vec<&str> = version.split('.').collect();
    let valid = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
    if !valid {
        anyhow::bail!("--github-release must be latest or a semver tag like v1.0.0");
    }
    Ok(format!("v{version}"))
}

fn read_checksum_file(path: &PathBuf) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path).map_err(|err| {
        anyhow::anyhow!("failed to read checksum file {}: {}", path.display(), err)
    })?;
    let checksum = text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("checksum file {} is empty", path.display()))?;
    Ok(checksum.to_string())
}

fn parse_plugin_json_flag(args: &[String]) -> anyhow::Result<bool> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            unknown => anyhow::bail!("unknown plugin option '{}'", unknown),
        }
    }
    Ok(json)
}

fn parse_plugin_search_args(args: &[String]) -> anyhow::Result<(Option<String>, bool, bool)> {
    let mut query = None;
    let mut json = false;
    let mut verbose = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--verbose" => verbose = true,
            value if value.starts_with("-") => {
                anyhow::bail!("unknown plugin search option {}", value)
            }
            value => {
                if query.is_some() {
                    anyhow::bail!("plugin search accepts at most one query");
                }
                query = Some(value.to_string());
            }
        }
    }
    Ok((query, json, verbose))
}

fn print_plugin_usage() {
    println!("Usage:  niu plugin <command>");
    println!();
    println!("Commands:");
    println!("  list [--json] [--verbose] List official Niubash plugins (active state)");
    println!("  info <name> [--json] [--verbose]  Inspect one plugin");
    println!("  search [query] [--json] [--verbose]  Discover plugins");
    println!("  themes [--json] [--verbose]  List user and bundle themes");
    println!("  enable <name>             Enable a plugin in ~/.niubashrc");
    println!("  disable <name>            Disable a plugin in ~/.niubashrc");
    println!("  bundle status [--json]    Inspect official bundle install state");
    println!("  doctor [--json] [--verbose]  Diagnose plugin configuration health");
    println!("  review <name> [--json]    Review plugin permissions before enabling");
    println!("  update oh-my-niu --from <path>");
    println!("      [--checksum <sha>|--checksum-file <path>] [--json]");
    println!("                            Install a local bundle directory or zip");
    println!("  update oh-my-niu --github-release latest|vX.Y.Z [--json]");
    println!("                            Download, verify, and install GitHub release");
    println!("  rollback oh-my-niu [--json]");
    println!("                            Roll back to the previous bundle");
    println!("  install <name>           Install official plugin from active bundle");
    println!("  uninstall <name>         Uninstall official plugin from active bundle");
}

/// Parse `--preset <name>` / `--preset=<name>` from `niu setup` arguments.
/// Unknown flags are ignored so the interactive wizard keeps working.
fn setup_preset_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--preset" {
            return iter.next().cloned();
        }
        if let Some(name) = arg.strip_prefix("--preset=") {
            return Some(name.to_string());
        }
    }
    None
}

#[cfg(windows)]
fn install_windows_terminal_profile(args: &[String]) -> anyhow::Result<()> {
    let mut set_default = false;
    let mut quiet = false;

    for arg in &args[2..] {
        match arg.as_str() {
            "--set-default" => set_default = true,
            "--quiet" => quiet = true,
            unknown => anyhow::bail!("unknown --install-wt-profile option '{}'", unknown),
        }
    }

    let commandline = std::env::current_exe()?;
    let icon = windows_terminal_icon_path(&commandline);
    let summary = niubash_runtime::windows_terminal::install_niubash_profile(
        &commandline,
        icon.as_deref(),
        set_default,
        None,
    )?;

    if !quiet {
        if summary.updated.is_empty() {
            println!("No Windows Terminal settings path was found.");
        } else {
            for path in summary.updated {
                println!("Updated Windows Terminal profile: {}", path.display());
            }
        }
    }

    Ok(())
}

/// Windows Terminal profile management is Windows-only; fail explicitly
/// instead of silently succeeding on Unix.
#[cfg(not(windows))]
fn install_windows_terminal_profile(_args: &[String]) -> anyhow::Result<()> {
    anyhow::bail!("--install-wt-profile is only supported on Windows")
}

#[cfg(windows)]
fn windows_terminal_icon_path(commandline: &std::path::Path) -> Option<PathBuf> {
    let app_dir = commandline.parent()?;
    [
        app_dir.join("assets").join("niubash-icon-256.png"),
        app_dir.join("assets").join("niubash-icon.png"),
        app_dir.join("niubash-icon-256.png"),
        app_dir.join("niubash-icon.png"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn print_completion_probe(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 3 {
        anyhow::bail!("--completion-probe requires an input line");
    }
    let line = &args[2];
    let cursor_pos = if let Some(raw) = args.get(3) {
        raw.parse::<usize>()
            .map_err(|_| anyhow::anyhow!("invalid cursor position '{}'", raw))?
    } else {
        line.len()
    };
    let mut shell = niubash_runtime::Shell::new()?;
    shell.run_startup_rc();
    for suggestion in shell.completion_probe(line, cursor_pos) {
        println!("{}", suggestion);
    }
    Ok(())
}

fn print_version() {
    // niubash#140: println! panics when stdout is a pipe the reader already
    // closed (os error 232), so `niu --version | true` aborted the launcher.
    // Write through an explicit handle and swallow the error, matching the
    // engine-side #125 policy (is_broken_pipe_error below).
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "Niubash {} \u{2014} bash-compatible shell for Windows",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(out, "  rubash   {}", rubash_revision_label());
    if let Some(v) = niubash_runtime::winuxcmd::version() {
        let _ = writeln!(out, "  winuxcmd {}", v);
    }
}

/// Format the embedded rubash revision. The `git ` prefix is only truthful
/// when build.rs resolved a real commit; a build without git access resolves
/// to "unknown" and must not be advertised as a branch name.
fn rubash_revision_label() -> String {
    let revision = option_env!("NIU_RUBASH_REV").unwrap_or("unknown");
    if revision == "unknown" {
        revision.to_string()
    } else {
        format!("git {revision}")
    }
}

fn is_broken_pipe_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(is_broken_pipe_io_error)
            || cause.to_string().contains("os error 232")
            || cause.to_string().contains("管道正在被关闭")
    })
}

fn is_broken_pipe_io_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::BrokenPipe || error.raw_os_error() == Some(232)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_update_parses_github_release() {
        let args = vec![
            "--github-release".to_string(),
            "latest".to_string(),
            "--json".to_string(),
        ];
        let options = parse_plugin_update_options(&args).unwrap();

        assert_eq!(options.github_release.as_deref(), Some("latest"));
        assert!(options.json);
        assert!(options.source_path.is_none());
    }

    #[test]
    fn plugin_release_tag_normalizes_semver() {
        assert_eq!(
            normalize_plugin_bundle_release_tag("1.2.3").unwrap(),
            "v1.2.3"
        );
        assert_eq!(
            normalize_plugin_bundle_release_tag("v1.2.3").unwrap(),
            "v1.2.3"
        );
        assert!(normalize_plugin_bundle_release_tag("stable").is_err());
        assert!(normalize_plugin_bundle_release_tag("v1.2").is_err());
        assert!(normalize_plugin_bundle_release_tag("v1.2.3.4").is_err());
    }

    fn dumped_bodies(input: &str) -> Vec<String> {
        let mut strings = Vec::new();
        collect_locale_strings(input, 1, &mut strings);
        strings.into_iter().map(|(_, body)| body).collect()
    }

    /// GNU bash 5.3.0 (`--dump-strings`, probed case-by-case): locale
    /// strings dump only where the word lexer sees `$"`, never in comment
    /// text, here-doc bodies, single-quoted text, double-quoted spans or
    /// backtick bodies; assignment RHS and command substitutions do.
    #[test]
    fn dump_strings_recognition_matches_gnu_word_lexer() {
        assert_eq!(dumped_bodies("echo $\"plain\""), vec!["plain"]);
        assert_eq!(dumped_bodies("echo a$\"mid\"dle"), vec!["mid"]);
        assert_eq!(dumped_bodies("x=$\"assign.rhs\""), vec!["assign.rhs"]);
        assert_eq!(dumped_bodies("echo $\"one\" $\"two\""), vec!["one", "two"]);
        assert_eq!(dumped_bodies("echo $\"a\"$\"b\""), vec!["a", "b"]);
        assert!(dumped_bodies("# comment with $\"in.comment\" text").is_empty());
        assert!(dumped_bodies("echo '$\"single.quoted\" not locale'").is_empty());
        assert!(dumped_bodies("echo \"$\"dqp\" tail\"").is_empty());
        assert!(dumped_bodies("echo \"a$\"b\"c\"").is_empty());
        assert!(dumped_bodies("echo `echo $\"in.backtick\"`").is_empty());
        assert!(dumped_bodies("echo \\$\"escaped.dollar\"").is_empty());
        assert_eq!(
            dumped_bodies("cat <<EOF\nheredoc body with $\"in.heredoc\"\nEOF\necho $\"after\""),
            vec!["after"]
        );
    }

    /// GNU prints the raw body text between the quotes, verbatim: escapes
    /// stay escaped (`locale.c locale_expand` printf("\"%s\"\n", temp)).
    #[test]
    fn dump_strings_keeps_escapes_raw() {
        assert_eq!(
            dumped_bodies("echo $\"esc \\\"q1\\\" q2\""),
            vec!["esc \\\"q1\\\" q2"]
        );
        assert_eq!(
            dumped_bodies("echo $\"tail.backslash\\\\\""),
            vec!["tail.backslash\\\\"]
        );
        // A real newline inside the string stays in the dumped body.
        assert_eq!(dumped_bodies("echo $\"multi\nline\""), vec!["multi\nline"]);
    }

    /// Command substitutions are re-lexed (parse.y:4100), whichever quoting
    /// context hides them; backtick bodies are not.
    #[test]
    fn dump_strings_recurses_into_command_substitutions() {
        assert_eq!(
            dumped_bodies("echo $(echo $\"in.comsub\")"),
            vec!["in.comsub"]
        );
        assert_eq!(
            dumped_bodies("echo pre$(echo $\"midcomsub\")post"),
            vec!["midcomsub"]
        );
        assert_eq!(
            dumped_bodies("echo \"$(echo $\"quotedcomsub\")\""),
            vec!["quotedcomsub"]
        );
        assert_eq!(
            dumped_bodies("echo $(( $(echo $\"arithcomsub\") + 1 ))"),
            vec!["arithcomsub"]
        );
    }

    /// locale.c mk_msgstr: `"` and `\` backslash-escaped, embedded newlines
    /// split as `\n` + quote close/reopen with an empty first msgid, entry
    /// anchored by `#: name:lineno` (line = the `$"` line).
    #[test]
    fn dump_po_strings_matches_gnu_format() {
        let render = |input: &str| {
            let mut strings = Vec::new();
            collect_locale_strings(input, 1, &mut strings);
            render_locale_string_dump(&strings, true, "probe.sh")
        };
        assert_eq!(
            render("echo $\"plain\""),
            "#: probe.sh:1\nmsgid \"plain\"\nmsgstr \"\"\n"
        );
        assert_eq!(
            render("echo $\"esc \\\"q1\\\" q2\""),
            "#: probe.sh:1\nmsgid \"esc \\\\\\\"q1\\\\\\\" q2\"\nmsgstr \"\"\n"
        );
        assert_eq!(
            render("echo $\"multi\nline\""),
            "#: probe.sh:1\nmsgid \"\"\n\"multi\\n\"\n\"line\"\nmsgstr \"\"\n"
        );
        // One entry per string, anchored on its own line.
        assert_eq!(
            render("echo $\"one\" $\"two\"\necho $\"three\""),
            "#: probe.sh:1\nmsgid \"one\"\nmsgstr \"\"\n\
             #: probe.sh:1\nmsgid \"two\"\nmsgstr \"\"\n\
             #: probe.sh:2\nmsgid \"three\"\nmsgstr \"\"\n"
        );
        assert_eq!(
            render("echo $\"\""),
            "#: probe.sh:1\nmsgid \"\"\nmsgstr \"\"\n"
        );
    }

    /// GNU yy_input_name() convention: the script path as given, the literal
    /// `-c` for -c input, the shell's own argv[0] for standard input.
    #[test]
    fn invocation_source_name_follows_gnu_convention() {
        let mut invocation = ShellInvocation::parse(&[]).unwrap();
        assert_eq!(
            invocation_source_name(&invocation),
            std::env::args().next().unwrap_or_else(|| "niu".to_string())
        );
        invocation.command = Some("echo hi".to_string());
        assert_eq!(invocation_source_name(&invocation), "-c");
        invocation.command = None;
        invocation.script = Some("D:/repo/probe.sh".to_string());
        assert_eq!(invocation_source_name(&invocation), "D:/repo/probe.sh");
    }
}
