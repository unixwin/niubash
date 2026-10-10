//! niu entry point (the niubash shell)
//!
//! Usage:
//!   niu                  → interactive REPL
//!   niu -c "command"     → execute one command, print exit code, exit
//!   niu -C "command"     → execute one REPL-style command, then exit
//!   niu script.sh        → execute a script file
//!   niu --help | -h      → usage
//!   niu --version        → version (niubash / rubash / winuxcmd)
//!   niu setup            → re-run the interactive setup wizard
//!   niu plugin discover → read-only overview of external plugin sources
//!   niu plugin source <cmd> → manage external plugin-manager sources
//!   niu --completion-probe "line" [cursor] → print REPL completions
//!   niu --install-wt-profile → add/update the Windows Terminal profile
//!   niu --self-update → download and run the latest installer
//!   self-update / update-niubash → REPL commands for Niubash self-update

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use rubash::invocation::ShellInvocation;

/// #125/#140: std `println!`/`print!` panic on any stdout write error, and
/// this binary builds with `panic = "abort"`, so a reader closing the pipe
/// (`niu --version | head -1`; Windows reports os error 232 = ERROR_NO_DATA
/// rather than EPIPE) aborts the launcher mid-output. Shadow both macros
/// file-wide with writers that follow the engine's closed-pipe rule
/// (`is_closed_output_io_error`: BrokenPipe or raw os error 232) — write
/// what fits, then exit 0 quietly, matching the SIGPIPE termination GNU
/// exhibits when its stdout reader goes away. `eprint!`/`eprintln!` get the
/// same treatment minus the exit: a dead stderr must not abort either, but
/// the launcher's own exit status still belongs to the command that ran.
fn write_stdout_lossy(text: &str) {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::BrokenPipe
                || error.raw_os_error() == Some(232) =>
        {
            std::process::exit(0)
        }
        Err(_) => {}
    }
}

fn write_stderr_lossy(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr
        .write_all(text.as_bytes())
        .and_then(|()| stderr.flush());
}

macro_rules! print {
    ($($arg:tt)*) => {
        crate::write_stdout_lossy(&format!($($arg)*))
    };
}
macro_rules! println {
    () => {
        crate::write_stdout_lossy("\n")
    };
    ($($arg:tt)*) => {
        crate::write_stdout_lossy(&format!("{}\n", format_args!($($arg)*)))
    };
}
macro_rules! eprint {
    ($($arg:tt)*) => {
        crate::write_stderr_lossy(&format!($($arg)*))
    };
}
macro_rules! eprintln {
    () => {
        crate::write_stderr_lossy("\n")
    };
    ($($arg:tt)*) => {
        crate::write_stderr_lossy(&format!("{}\n", format_args!($($arg)*)))
    };
}

mod piped_stdout;
mod self_update;
mod skill;
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
    let code = std::thread::Builder::new()
        .name("niu-main".to_string())
        .stack_size(NIU_MAIN_STACK_SIZE)
        .spawn(run_main)
        .expect("spawn niubash main thread")
        .join()
        .unwrap_or_else(|_| ExitCode::from(1));
    // niubash#245: before the process tears down its threads, let the
    // piped-stdout pump (when installed) forward everything still in
    // flight — the main-return path of every non-interactive run drains
    // here so normal runs never lose buffered output.
    piped_stdout::finish();
    code
}

fn run_main() -> ExitCode {
    // niubash#245: guard a piped stdout for non-interactive executions
    // BEFORE anything can write (Rust caches the stdio handle on first
    // use, so the interposition must land first). A reader that closes
    // the pipe must terminate niu — one diagnostic, status 1 — instead
    // of letting an unbounded producer loop on per-write failures.
    let args: Vec<String> = std::env::args().collect();
    piped_stdout::install_for_argv(&args);

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
    if first.starts_with('-') || first.starts_with('+') {
        if !matches!(
            first.as_str(),
            "-h" | "--help"
                | "-V"
                | "--version"
                | "-C"
                | "--repl-command"
                | "--completion-probe"
                | "--install-wt-profile"
                | "--self-update"
        ) && !legacy_command_mode_has_post_c_login_flag(args)
        {
            // GNU shell.c parse_shell_options walks argv in one left-to-right
            // pass: option words may stand in any order before the first
            // non-option operand, and the shell does not re-dispatch on the
            // first word alone. A launcher-owned word therefore keeps its
            // meaning wherever it appears among the leading options
            // (niubash#148): `niu --norc -C 'cmd'` used to fall through to
            // the engine parser here, which has no REPL-command -C (GNU -C
            // is noclobber) and then treated the command string as a script
            // path. Words after the launcher flag's own argument keep GNU
            // operand semantics, so `niu -C 'cmd' --norc` still binds
            // --norc as $0 exactly like `bash -c 'cmd' --norc`.
            if let Some(index) = launcher_dispatch_index(args) {
                return dispatch_launcher_word(args, index);
            }
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
                    piped_stdout::finish_and_exit(2);
                }
            };
        }
        // The leading word is itself a launcher word (or the legacy
        // `niu -c -l cmd` shape kept above): dispatch on argv[1].
        return dispatch_launcher_word(args, 1);
    }
    match first.as_str() {
        "setup" | "configure" => {
            // niubash#195: `--help`/`-h` is a usage request, never a wizard
            // run — always answer in plain text so piped output stays clean.
            if args[2..].iter().any(|arg| arg == "-h" || arg == "--help") {
                show_setup_usage();
                return Ok(());
            }
            match setup_preset_arg(&args[2..]) {
                Some(name) => niubash_runtime::setup_wizard::apply_preset(&name),
                None => niubash_runtime::setup_wizard::rerun_wizard(),
            }
        }
        "font" => niubash_runtime::fonts::run_font_command(),
        "doctor" => niubash_runtime::doctor::run_doctor(skill::SKILL_FILES),
        "config" => run_config_command(&args[2..]),
        "plugin" => run_plugin_command(args),
        "skill" => skill::run_skill_command(args),
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
                piped_stdout::finish_and_exit(127);
            }
            // general.c:718-741 check_binary_file: NUL in the first line(s)
            // or an ELF image is refused with EX_BINARY_FILE (126).
            let bytes = std::fs::read(&script)?;
            if rubash::script_driver::check_binary_file(&bytes)
                || std::str::from_utf8(&bytes).is_err()
            {
                eprintln!("cannot execute binary file");
                piped_stdout::finish_and_exit(126);
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
                piped_stdout::finish_and_exit(code);
            }
            Ok(())
        }
    }
}

/// Index of the launcher-owned word among the leading option words of `args`
/// (argv[1]..), honoring GNU parse_shell_options' single left-to-right walk:
/// engine options that take a separate argument (`-o`, `-O`, `--rcfile`,
/// `--init-file`) consume the following word, `-c`/`-s` and the first
/// non-option word hand the rest to the engine route, and every other
/// `-`/`+` word is engine shell-option syntax. Returns `None` when the whole
/// leading option run belongs to the engine.
fn launcher_dispatch_index(args: &[String]) -> Option<usize> {
    const LAUNCHER_WORDS: &[&str] = &[
        "-h",
        "--help",
        "-V",
        "--version",
        "-C",
        "--repl-command",
        "--completion-probe",
        "--install-wt-profile",
        "--self-update",
    ];
    let mut index = 1usize;
    while let Some(arg) = args.get(index) {
        if LAUNCHER_WORDS.contains(&arg.as_str()) {
            return Some(index);
        }
        match arg.as_str() {
            // -c consumes the rest as the command string + operands, -s the
            // remaining words as positional parameters; both belong to the
            // engine route either way.
            "-c" | "-s" => return None,
            "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" => index += 2,
            word if word.starts_with('-') || word.starts_with('+') => index += 1,
            // First non-option word is the script operand (engine route).
            _ => return None,
        }
    }
    None
}

/// Run the launcher word at `args[index]`. `args[1..index]` are the option
/// words that stood before it (applied by the handlers that understand
/// them); `args[index + 1..]` are the word's own arguments.
fn dispatch_launcher_word(args: &[String], index: usize) -> anyhow::Result<()> {
    let word = args[index].as_str();
    let leading_options = &args[1..index];
    let rest = &args[index + 1..];
    match word {
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "--version" | "-V" => {
            print_version();
            Ok(())
        }
        "--completion-probe" => {
            print_completion_probe(rest)?;
            Ok(())
        }
        "--install-wt-profile" => {
            install_windows_terminal_profile(rest)?;
            Ok(())
        }
        "--self-update" => self_update::run(rest),
        "-C" | "--repl-command" => run_repl_command(word, leading_options, rest),
        // Only reachable for the legacy `niu -c -l <cmd>` shape: a plain
        // leading -c routes to the engine parser above.
        "-c" => {
            let command_mode = parse_legacy_command_mode(rest)?;
            let mut shell = niubash_runtime::Shell::new()?;
            niubash_runtime::startup_trace::tick("-c: Shell::new");
            shell.executor.inherit_process_stdin();
            shell.enable_process_stdin_pipeline_bridge();
            shell
                .executor
                .set_env("BASH_EXECUTION_STRING", command_mode.command);
            // Same -c diagnostic tag as the plain invocation route
            // (niubash#160): `$0: -c: line N:` parser diagnostics.
            shell.executor.set_env("__RUBASH_IS_C", "1");
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
                piped_stdout::finish_and_exit(code);
            }
            Ok(())
        }
        other => anyhow::bail!("unknown launcher word '{other}'"),
    }
}

struct LegacyCommandMode<'a> {
    command: &'a str,
    command_name: Option<&'a str>,
    positional_params: &'a [String],
}

/// `niu -c [-l|--login] <command> [name [params...]]` — the legacy shape kept
/// for the `-c -l` combination. `rest` starts right after the `-c` word.
fn parse_legacy_command_mode(rest: &[String]) -> anyhow::Result<LegacyCommandMode<'_>> {
    let mut index = 0;
    while matches!(rest.get(index).map(String::as_str), Some("-l" | "--login")) {
        index += 1;
    }
    let Some(command) = rest.get(index) else {
        anyhow::bail!("-c requires an argument");
    };
    let command_name = rest.get(index + 1).map(String::as_str);
    let positional_params = rest.get(index + 2..).unwrap_or(&[]);
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
                piped_stdout::finish_and_exit(2);
            }
            anyhow::anyhow!("niu: {error}")
        })?;
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();

    if let Some(command) = invocation.command {
        // GNU shell.c: -i sets forced_interactive during option parsing, so
        // `bash -i -c 'cmd'` takes run_startup_files' interactive branch
        // (shell.c:1222: rc file) and never reads BASH_ENV; plain `-c` runs
        // the shell.c:1214-1220 non-interactive BASH_ENV branch (the
        // shell.c:1156 sshd bashrc case is compiled out of the reference
        // build — see source_non_interactive_env).
        if invocation.interactive {
            shell.run_interactive_startup_rc();
        } else {
            shell.source_non_interactive_env();
        }
        niubash_runtime::startup_trace::tick("invocation: setup done");
        // GNU shell.c: $0 for -c is the word after the command string, or
        // $BASH_ARGV0 from the environment when exported by the caller.
        if let Some(argv0) = inherited_argv0.clone() {
            shell.set_script_name(&argv0);
        } else if let Some(name) = invocation.command_name.clone() {
            shell.set_script_name(&name);
        }
        shell.executor.set_env("BASH_EXECUTION_STRING", &command);
        // __RUBASH_IS_C is already live here: invocation.apply_to_executor
        // (rubash invocation.rs:273-275) sets it for every command string,
        // so parser diagnostics take the `$0: -c: line N:` shape (error.c
        // get_name_for_error; niubash#160).
        // niubash#170 theme-preview channel: a `-c` child gated by
        // NIU_PRINT_RENDERED_PS1=1 (spawned by plugins::theme_preview) has
        // just sourced a theme's managed block; print the session's rendered
        // PS1 between the preview markers — the prompt renders before any
        // EXIT trap in a real session, so the print lands before the trap.
        // The child's own exit code still propagates: the preview parent
        // treats a failing block like any other degradation.
        //
        // The gated child also carries the engine's interactive marker
        // (exactly what the piped `niu -i` route sets, main.rs above): a
        // theme loader guards on it — oh-my-bash.sh opens with
        // `case $- in *i*) ;; *) return ;;` — and the preview must render
        // the face an interactive session draws, not the bare script face.
        if std::env::var_os(niubash_runtime::plugins::theme_preview::PRINT_RENDERED_PS1_ENV)
            .is_some_and(|value| value == "1")
        {
            shell.executor.set_env("__RUBASH_INTERACTIVE", "1");
        }
        let code = shell.execute_script(&command)?;
        niubash_runtime::startup_trace::tick("invocation: execute_script");
        if std::env::var_os(niubash_runtime::plugins::theme_preview::PRINT_RENDERED_PS1_ENV)
            .is_some_and(|value| value == "1")
        {
            shell.print_rendered_prompt_for_preview();
        }
        let code = shell.finish_with_exit_trap(code)?;
        niubash_runtime::startup_trace::tick("invocation: exit trap");
        if code != 0 {
            piped_stdout::finish_and_exit(code);
        }
        return Ok(());
    }
    if let Some(script_name) = invocation.script {
        // GNU: `bash -i script` is an interactive shell (forced_interactive)
        // and sources the rc file, not BASH_ENV (shell.c:1214 checks
        // interactive_shell == 0).
        if invocation.interactive {
            shell.run_interactive_startup_rc();
        } else {
            shell.source_non_interactive_env();
        }
        shell.set_script_name(&script_name);
        let content = std::fs::read_to_string(script_arg_to_host_path(&script_name))?;
        let code = shell.execute_script(&content)?;
        let code = shell.finish_with_exit_trap(code)?;
        if code != 0 {
            piped_stdout::finish_and_exit(code);
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
        // This path renders prompts through the engine's interactive stdin
        // driver (which expands executor-env PS1 itself), so the foreign
        // inherited PS1 must be discarded here too, before run_startup_rc
        // (unixwin/niubash#117; same rule as enter_interactive).
        shell.discard_foreign_inherited_prompt();
        // GNU decides "interactive" from the -i flag, never from the shape
        // of stdin (shell.c:672 forced_interactive is set in option parsing,
        // before run_startup_files at shell.c:722 sources ~/.bashrc for an
        // interactive shell). The interactive startup rc — and --rcfile,
        // which the shell already carries — must therefore run on the piped
        // -i path too (niubash#146), still before the interactive history
        // setup, which shell.c:806-811 runs only after the startup files.
        shell.run_startup_rc();
        rubash::script_driver::prepare_interactive_history(&mut shell.executor);
        let code = rubash::script_driver::run_interactive_stdin(&mut shell.executor);
        piped_stdout::finish_and_exit(code);
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
        piped_stdout::finish_and_exit(code);
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

/// `niu [-C|--repl-command] <command> [name [params...]]`: execute one
/// REPL-style command and exit. `leading_options` are the shell option words
/// that stood before the -C word (niubash#148); they are parsed with the
/// engine's `ShellInvocation` — the same surface the engine route uses — so
/// `niu --norc -C 'cmd'` and `niu -C 'cmd'` see one consistent option model,
/// with rc-affecting fields applied before the startup rc runs.
fn run_repl_command(flag: &str, leading_options: &[String], rest: &[String]) -> anyhow::Result<()> {
    let Some(command) = rest.first() else {
        anyhow::bail!("{flag} requires an argument");
    };
    if let Some(self_update_args) = niubash_runtime::repl::self_update_command_args(command) {
        if let Some(code) = niubash_runtime::repl::spawn_self_update(&self_update_args) {
            std::process::exit(code);
        }
    }
    let mut shell = niubash_runtime::Shell::new()?;
    niubash_runtime::startup_trace::tick("-C: Shell::new");
    if !leading_options.is_empty() {
        // Same option words, same GNU error surface as the engine route
        // below (shell.c:874-881): a rejected option is a usage error under
        // the engine's "bash" name, rc 2, with the usage block when the
        // word is an invalid option.
        let invocation = match ShellInvocation::parse(leading_options) {
            Ok(invocation) => invocation,
            Err(message) => {
                eprintln!("bash: {message}");
                if message.contains("invalid option") {
                    show_shell_usage();
                }
                std::process::exit(2);
            }
        };
        shell.no_rc = invocation.no_rc;
        shell.no_profile = invocation.no_profile;
        shell.rc_file = invocation.rc_file.clone().map(PathBuf::from);
        shell.no_editing = invocation.no_editing;
        invocation
            .apply_to_executor(&mut shell.executor)
            .map_err(|error| {
                if error.contains("invalid shell option name") {
                    eprintln!("bash: line 0: {error}");
                    std::process::exit(2);
                }
                anyhow::anyhow!("{error}")
            })?;
    }
    shell.enter_interactive();
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();
    if let Some(command_name) = rest.get(1) {
        shell.set_script_name(command_name);
        shell.executor.set_positional_params(rest[2..].to_vec());
    }
    shell.run_startup_rc();
    niubash_runtime::startup_trace::tick("-C: startup rc");
    shell.run_precmd_hooks();
    niubash_runtime::startup_trace::tick("-C: precmd hooks");
    let code = shell.execute_interactive_line(command)?;
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
    println!("        niu font            Nerd Font detection & install recommendations");
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
    println!("  plugin add <id|owner/repo|url|path>");
    println!("                            Install an external plugin source (untrusted)");
    println!("  plugin list [--json]      Sources, their assets, activation state");
    println!("  plugin enable|disable <t> Activate or deactivate a source or asset");
    println!("  plugin update|sync|restore|rollback|clean");
    println!("                            Lockfile verbs (vim-plug/lazy.nvim-style)");
    println!("  plugin trust <id>         Review and activate a source's assets");
    println!("  plugin discover [--verbose]");
    println!("                            Read-only overview of external plugin sources");
    println!("  plugin source <command>   Full source protocol (add/trust/sign/verify/");
    println!("                            remove/update/rollback/list)");
    println!("  plugin recipe <command>   Recipe index (list/show/add)");
    println!("  plugin distro <command>   Collections (list/import/remove/apply)");
    println!("  plugin mirror <command>   Git fetch mirroring (list/show/set)");
    println!("  plugin ui                 Menu UI (sections by state, same verbs)");
    println!();
    println!("  skill install|status      Install/check the AI agent skill bundle");
    println!("                            ([--target claude|zcode|cursor|all|<dir>])");
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
        "discover" => run_plugin_discover_command(&args[3..]),
        "source" | "sources" => run_plugin_source_command(&args[3..]),
        // Recipe index (mason-registry pattern): data rows for the whole
        // bash ecosystem; `add` routes through the git driver or prints a
        // package-manager recommendation (download retraction 2026-10-04).
        "recipe" | "recipes" => run_plugin_recipe_command(&args[3..]),
        // Collections (LazyVim extras pattern): data manifests of recipe
        // ids, built-in or imported, applied without auto-trust.
        "distro" | "distros" | "collection" | "collections" => {
            run_plugin_distro_command(&args[3..])
        }
        // Transport-layer git mirroring (§14.8): a configurable insteadOf
        // rewrite base, not a curated source list (git-only since the
        // download retraction — the shell has no HTTP transport left).
        "mirror" | "mirrors" => run_plugin_mirror_command(&args[3..]),
        // Menu-level UI (lazy view IA): sections by state, verbs from the
        // command table, actions through the same runtime calls.
        "ui" => niubash_runtime::plugins::ui::run_ui(),
        // The external ecosystem is first-class (owner ruling 2026-10-02):
        // vim-plug/lazy.nvim-style verbs over the real manager assets.
        "add" => run_plugin_add_command(&args[3..]),
        "list" => run_plugin_list_command(&args[3..]),
        "enable" => run_plugin_enable_command(&args[3..], true),
        "disable" => run_plugin_enable_command(&args[3..], false),
        "trust" => run_plugin_source_trust_command(&args[3..]),
        "update" => run_plugin_source_update_command(&args[3..]),
        "rollback" => run_plugin_source_rollback_command(&args[3..]),
        "restore" => run_plugin_restore_command(&args[3..]),
        "sync" => run_plugin_sync_command(&args[3..]),
        "clean" => run_plugin_clean_command(&args[3..]),
        // The built-in pack/bundle subcommands retired with the plugin
        // stack (niubash#145).
        "info" | "search" | "themes" | "bundle" | "doctor" | "review" | "use" => {
            anyhow::bail!(
                "plugin '{}' retired with the built-in plugin/theme stack (niubash#145); \
                 see `niu plugin --help` for the external ecosystem",
                subcommand
            )
        }
        // The downloaded-tools channel retired with the download driver
        // (owner ruling 2026-10-04): executable tools install through your
        // system package manager (wpm first on Windows, native managers
        // elsewhere) — `niu plugin add <recipe-id>` prints the commands.
        "tool" | "tools" => anyhow::bail!(
            "plugin tool retired with the download retraction (niu carries zero download \
             responsibility); executable tools install via your package manager — \
             see `niu plugin add <recipe-id>` for the commands, or remove an old \
             download-era install by deleting ~/.niubash/tools/<id>"
        ),
        unknown => anyhow::bail!("unknown plugin subcommand '{}'", unknown),
    }
}

/// `niu plugin discover`: a dry, read-only overview of the external plugin
/// ecosystem. Shows installed sources (with their ready/untrusted/degraded
/// state) and the known plugin managers that are *not* installed yet —
/// without installing, trusting, sourcing, or writing anything. Every
/// install stays an explicit command the user runs.
fn run_plugin_discover_command(args: &[String]) -> anyhow::Result<()> {
    for arg in args {
        match arg.as_str() {
            "--verbose" => {}
            unknown => anyhow::bail!("unknown plugin option '{}'", unknown),
        }
    }
    println!(
        "{}",
        niubash_runtime::text_style::bold("Niubash plugin ecosystem")
    );
    println!(
        "{}",
        niubash_runtime::text_style::dim(
            "  read-only overview — nothing is installed, sourced, or changed"
        )
    );
    println!();

    println!(
        "{}",
        niubash_runtime::text_style::cyan("Plugin sources (external plugin managers)")
    );
    let statuses = niubash_runtime::plugins::sources::list_sources();
    if statuses.is_empty() {
        println!("  (none installed)");
    }
    for status in &statuses {
        let marker = match status.state.as_str() {
            "ready" => niubash_runtime::text_style::green("ready"),
            "untrusted" => niubash_runtime::text_style::yellow("untrusted"),
            _ => niubash_runtime::text_style::red("degraded"),
        };
        let assets = status
            .asset_count
            .map(|count| format!(" ({count} assets)"))
            .unwrap_or_default();
        let gutted_hint = if status.state == "ready" && status.asset_count == Some(0) {
            niubash_runtime::text_style::dim(&format!(
                "\n     no assets found in the tree — verify with `niu plugin source verify {}`",
                status.record.id
            ))
        } else {
            String::new()
        };
        println!(
            "  {} {:<12} {:<12} {}{}{}",
            marker,
            status.record.id,
            status.record.version,
            status.record.license,
            assets,
            gutted_hint
        );
    }
    println!();

    println!("{}", niubash_runtime::text_style::cyan("Available sources"));
    let mut listed = 0usize;
    for adapter in niubash_runtime::plugins::sources::builtin_source_adapters() {
        if statuses
            .iter()
            .any(|status| status.record.id == adapter.id())
        {
            continue;
        }
        let add_hint = match adapter.default_origin() {
            Some(origin) => {
                format!(
                    "niu plugin add {}   ({})",
                    adapter.id(),
                    niubash_runtime::text_style::dim(origin)
                )
            }
            None => format!(
                "niu plugin add {} --path <dir>   ({})",
                adapter.id(),
                niubash_runtime::text_style::dim(
                    adapter.install_note().unwrap_or("local installs only")
                )
            ),
        };
        println!(
            "  {:<16} {:<18} {}",
            adapter.display_name(),
            adapter.license(),
            adapter.summary()
        );
        println!("  {}", niubash_runtime::text_style::dim(&add_hint));
        listed += 1;
    }
    if listed == 0 {
        println!(
            "  {}",
            niubash_runtime::text_style::dim("(every known manager is already installed)")
        );
    }
    println!();
    // niubash#179 L05-1: the wizard's empty-gallery note and the finish
    // screen both promise that `niu plugin discover` browses sources &
    // themes — so discover must actually enumerate the theme layer.
    println!(
        "{}",
        niubash_runtime::text_style::cyan("Themes (from trusted sources)")
    );
    let themes = niubash_runtime::plugins::sources::source_theme_entries();
    if themes.is_empty() {
        println!(
            "  {}",
            niubash_runtime::text_style::dim(
                "(none yet — install a source with `niu plugin add`, then `niu plugin trust <id>`)"
            )
        );
    }
    for theme in &themes {
        println!(
            "  {:<28} {}",
            theme.name,
            niubash_runtime::text_style::dim(&format!(
                "{} ({})  enable: `niu plugin enable {}`",
                theme.adapter_display, theme.source_id, theme.name
            ))
        );
    }
    println!();
    println!(
        "{}",
        niubash_runtime::text_style::dim(
            "Themes render through the bash-compatible PS1 channel; the built-in \
             plugin/theme stack is retired (niubash#145)."
        )
    );
    Ok(())
}

/// `niu plugin source <verb>` — external plugin-manager sources
/// (oh-my-bash loader, bash-it, bpkg) as first-class plugin origins.
/// Design: docs/planning/oh-my-niu-ecosystem.md §11-§12.
fn run_plugin_source_command(args: &[String]) -> anyhow::Result<()> {
    let Some(verb) = args.first() else {
        print_plugin_source_usage();
        return Ok(());
    };
    match verb.as_str() {
        "-h" | "--help" => {
            print_plugin_source_usage();
            Ok(())
        }
        "list" => run_plugin_source_list_command(&args[1..]),
        "add" => run_plugin_source_add_command(&args[1..]),
        "trust" => run_plugin_source_trust_command(&args[1..]),
        "remove" => run_plugin_source_remove_command(&args[1..]),
        "update" => run_plugin_source_update_command(&args[1..]),
        "rollback" => run_plugin_source_rollback_command(&args[1..]),
        "verify" => run_plugin_source_verify_command(&args[1..]),
        "sign" => run_plugin_source_sign_command(&args[1..]),
        unknown => anyhow::bail!("unknown plugin source subcommand '{}'", unknown),
    }
}

fn print_plugin_source_usage() {
    println!("Usage:  niu plugin source <command>");
    println!();
    println!("External plugin-manager sources (oh-my-bash loader, bash-it, bpkg).");
    println!("Sources install untrusted; assets activate only after trust.");
    println!();
    println!("Commands:");
    println!("  list [--json]           List installed sources and their state");
    println!("  add <id|url|path> [--ref <ref>] [--checksum <sha256>]");
    println!("                          Install a source tree (untrusted)");
    println!("  trust <id>              Review and activate a source's assets");
    println!("  remove <id>             Uninstall a source tree and its record");
    println!("  update [<id>] [--ref <ref>] [--checksum <sha256>]");
    println!("                          Update source(s) to the ref tip (no id =");
    println!("                          all); previous state kept");
    println!("  rollback <id>           Restore the previous source state");
    println!("  verify <id>             Re-check the source tree checksum");
    println!("  sign <id>               Pin trust with a local ed25519 signature");
    println!("                          (updates then re-gate until re-signed)");
}

/// `niu plugin source sign <id>` — the local-signature trust tier: verify
/// the tree, sign its digest with the machine-local key, and mark the
/// source trusted at the stricter policy.
fn run_plugin_source_sign_command(args: &[String]) -> anyhow::Result<()> {
    let Some(id) = args.first() else {
        anyhow::bail!("plugin source sign requires a source id");
    };
    let record = niubash_runtime::plugins::sources::sign_source(id)?;
    println!(
        "{} source '{}' at the local-signature tier",
        niubash_runtime::text_style::green("Signed"),
        record.id
    );
    println!(
        "  key fingerprint {}",
        niubash_runtime::text_style::dim(
            record
                .signature
                .as_ref()
                .map(|s| s.public_key.as_str())
                .unwrap_or("?")
        )
    );
    println!(
        "  {}",
        niubash_runtime::text_style::dim(
            "any update that changes the tree re-enters the trust gate until re-signed"
        )
    );
    Ok(())
}

#[derive(Clone)]
struct PluginSourceArgs {
    /// First positional: adapter id, git url, or local path.
    target: Option<String>,
    ref_name: Option<String>,
    checksum: Option<String>,
    path: Option<String>,
    url: Option<String>,
    id: Option<String>,
}

fn parse_plugin_source_args(args: &[String]) -> anyhow::Result<PluginSourceArgs> {
    let mut parsed = PluginSourceArgs {
        target: None,
        ref_name: None,
        checksum: None,
        path: None,
        url: None,
        id: None,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--ref" {
            parsed.ref_name = Some(
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("--ref requires a value"))?
                    .clone(),
            );
        } else if let Some(value) = arg.strip_prefix("--ref=") {
            parsed.ref_name = Some(value.to_string());
        } else if arg == "--checksum" {
            parsed.checksum = Some(
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("--checksum requires a value"))?
                    .clone(),
            );
        } else if let Some(value) = arg.strip_prefix("--checksum=") {
            parsed.checksum = Some(value.to_string());
        } else if arg == "--path" {
            parsed.path = Some(
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("--path requires a value"))?
                    .clone(),
            );
        } else if arg == "--url" {
            parsed.url = Some(
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("--url requires a value"))?
                    .clone(),
            );
        } else if arg == "--id" {
            parsed.id = Some(
                iter.next()
                    .ok_or_else(|| anyhow::anyhow!("--id requires a value"))?
                    .clone(),
            );
        } else if !arg.starts_with('-') {
            if parsed.target.is_some() {
                anyhow::bail!("plugin source accepts at most one positional argument");
            }
            parsed.target = Some(arg.clone());
        } else {
            anyhow::bail!("unknown plugin source option '{}'", arg);
        }
    }
    Ok(parsed)
}

/// Resolve CLI target/flags into an install request. The first positional
/// may be an adapter id (origin then comes from --url/--path), or the origin
/// itself (git url / local directory), with the adapter auto-detected from
/// the fetched tree's layout.
fn resolve_source_install_request(args: PluginSourceArgs) -> anyhow::Result<PluginSourceRequest> {
    let explicit_origin = args.url.clone().or_else(|| args.path.clone());
    let (adapter, origin) = match (&args.target, &explicit_origin) {
        (Some(target), Some(origin)) => {
            // Target names a known adapter kind.
            let known = niubash_runtime::plugins::sources::adapter_for(target).is_some();
            if !known {
                anyhow::bail!(
                    "unknown source kind '{}'; {}",
                    target,
                    supported_sources_hint()
                );
            }
            (Some(target.clone()), origin.clone())
        }
        (Some(target), None) => {
            // The positional is the origin (git url / GitHub shorthand /
            // local directory); a bare adapter id resolves to its catalog
            // origin so `niu plugin add oh-my-bash` just works.
            if niubash_runtime::plugins::sources::adapter_for(target).is_some()
                && !std::path::Path::new(target).exists()
                && !target.contains('/')
                && !target.contains('\\')
                && !target.contains("://")
            {
                if let Some(entry) = niubash_runtime::plugins::catalog::catalog_entry(target) {
                    return Ok(PluginSourceRequest {
                        adapter: Some(entry.id.to_string()),
                        origin: entry.origin.to_string(),
                        ref_name: args.ref_name.clone(),
                        expected_checksum: args.checksum.clone(),
                        id: args.id.clone(),
                    });
                }
                anyhow::bail!(
                    "source '{target}' needs an origin: add --url <git-url> or --path <dir>"
                );
            }
            (None, target.clone())
        }
        (None, Some(origin)) => (None, origin.clone()),
        (None, None) => {
            anyhow::bail!("plugin source add requires <id|url|path> (or --url/--path with an id)")
        }
    };
    Ok(PluginSourceRequest {
        adapter,
        origin,
        ref_name: args.ref_name,
        expected_checksum: args.checksum,
        id: args.id,
    })
}

struct PluginSourceRequest {
    adapter: Option<String>,
    origin: String,
    ref_name: Option<String>,
    expected_checksum: Option<String>,
    id: Option<String>,
}

impl PluginSourceRequest {
    fn to_install_request(&self) -> niubash_runtime::plugins::sources::SourceInstallRequest {
        niubash_runtime::plugins::sources::SourceInstallRequest {
            adapter: self.adapter.clone(),
            origin: self.origin.clone(),
            ref_name: self.ref_name.clone(),
            commit: None,
            expected_checksum: self.expected_checksum.clone(),
            id: self.id.clone(),
            entry: None,
            fetch_budget: None,
        }
    }
}

fn supported_sources_hint() -> String {
    let ids: Vec<&str> = niubash_runtime::plugins::sources::builtin_source_adapters()
        .iter()
        .map(|adapter| adapter.id())
        .collect();
    format!("supported plugin-manager sources: {}", ids.join(", "))
}

/// Trust-boundary notice printed before fetching third-party shell code
/// (§12.2 fetch gate). Fetching never executes the fetched code and the
/// result registers untrusted, so non-interactive runs stay safe.
fn print_source_trust_boundary(id: &str, request: &PluginSourceRequest) {
    println!(
        "{}: fetching third-party shell code",
        niubash_runtime::text_style::yellow("Trust boundary")
    );
    println!("  source:   {}", id);
    println!("  origin:   {}", request.origin);
    if let Some(ref_name) = &request.ref_name {
        println!("  ref:      {}", ref_name);
    }
    println!(
        "  license:  {}",
        niubash_runtime::plugins::sources::adapter_for(id)
            .map(|adapter| adapter.license())
            .unwrap_or("(detected after fetch)")
    );
    if let Some(checksum) = &request.expected_checksum {
        println!("  checksum: {}", checksum);
    }
    println!(
        "  {}",
        niubash_runtime::text_style::dim(
            "fetched code is inert until you review and trust it; nothing is sourced yet"
        )
    );
}

fn run_plugin_source_add_command(args: &[String]) -> anyhow::Result<()> {
    let parsed = parse_plugin_source_args(args)?;
    let request = resolve_source_install_request(parsed)?;
    let display_id = request
        .adapter
        .clone()
        .unwrap_or_else(|| "(auto-detect)".to_string());
    print_source_trust_boundary(&display_id, &request);
    let record = niubash_runtime::plugins::sources::add_source(request.to_install_request())?;
    println!(
        "{} source '{}' ({}) into {}",
        niubash_runtime::text_style::green("Installed"),
        record.id,
        niubash_runtime::text_style::dim(&record.version),
        niubash_runtime::text_style::dim(&record.path.display().to_string())
    );
    println!(
        "license {} | tree sha256 {}",
        record.license, record.checksum_sha256
    );
    println!("the source is untrusted; review it, then run:");
    println!("  niu plugin source trust {}", record.id);
    Ok(())
}

fn run_plugin_source_list_command(args: &[String]) -> anyhow::Result<()> {
    let json = args.iter().any(|arg| arg == "--json");
    let statuses = niubash_runtime::plugins::sources::list_sources();
    if json {
        println!("{}", serde_json::to_string_pretty(&statuses)?);
        return Ok(());
    }
    println!(
        "{}",
        niubash_runtime::text_style::bold("Niubash plugin sources")
    );
    println!(
        "{}",
        niubash_runtime::text_style::dim("  (external plugin managers; untrusted until trusted)")
    );
    if statuses.is_empty() {
        println!("(no sources installed; add one with niu plugin source add <id|url|path>)");
        return Ok(());
    }
    for status in statuses {
        let marker = match status.state.as_str() {
            "ready" => niubash_runtime::text_style::green("ready"),
            "untrusted" => niubash_runtime::text_style::yellow("untrusted"),
            _ => niubash_runtime::text_style::red("degraded (native fallback active)"),
        };
        let assets = status
            .asset_count
            .map(|count| {
                format!(
                    "{} asset{} ({})",
                    count,
                    if count == 1 { "" } else { "s" },
                    status.asset_kinds.join("/")
                )
            })
            .unwrap_or_else(|| "no assets".to_string());
        println!(
            "  {} {:<12} {:<10} {} {}",
            marker,
            status.record.id,
            status.record.version,
            niubash_runtime::text_style::dim(&assets),
            niubash_runtime::text_style::dim(&status.record.path.display().to_string())
        );
    }
    Ok(())
}

fn run_plugin_source_trust_command(args: &[String]) -> anyhow::Result<()> {
    let Some(id) = args.first() else {
        anyhow::bail!("plugin source trust requires a source id");
    };
    // Review summary before flipping the execution gate (§12.2).
    let statuses = niubash_runtime::plugins::sources::list_sources();
    let Some(status) = statuses.iter().find(|status| status.record.id == *id) else {
        anyhow::bail!("unknown source '{}'; run niu plugin source add first", id);
    };
    let verify = niubash_runtime::plugins::sources::verify_source(id)?;
    println!(
        "{} source '{}' review",
        niubash_runtime::text_style::bold("Trust"),
        status.record.id
    );
    println!("  origin:   {}", status.record.url);
    println!("  version:  {}", status.record.version);
    println!("  license:  {}", status.record.license);
    println!(
        "  checksum: {} ({})",
        status.record.checksum_sha256,
        if verify.verified {
            niubash_runtime::text_style::green("verified")
        } else if verify.degraded {
            niubash_runtime::text_style::red("tree missing")
        } else {
            niubash_runtime::text_style::red("MISMATCH")
        }
    );
    println!("  path:     {}", status.record.path.display());
    if verify.degraded {
        anyhow::bail!("cannot trust a degraded source (tree missing)");
    }
    let trusted = niubash_runtime::plugins::sources::trust_source(id)?;
    println!(
        "{} '{}' is now trusted; its themes/assets join the catalog",
        niubash_runtime::text_style::green("Trusted:"),
        trusted.id
    );
    println!("restart niu (or reload ~/.niubashrc) for the change to take effect");
    Ok(())
}

fn run_plugin_source_remove_command(args: &[String]) -> anyhow::Result<()> {
    let Some(id) = args.first() else {
        anyhow::bail!("plugin source remove requires a source id");
    };
    let path = niubash_runtime::plugins::sources::remove_source(id)?;
    println!(
        "{} source '{}' ({})",
        niubash_runtime::text_style::green("Removed"),
        id,
        niubash_runtime::text_style::dim(&path.display().to_string())
    );
    Ok(())
}

fn run_plugin_source_update_command(args: &[String]) -> anyhow::Result<()> {
    let parsed = parse_plugin_source_args(args)?;
    let Some(id) = parsed.target.clone() else {
        // No id: update every git-origin source to its ref's tip (the
        // pre-14.6 meaning of bare `niu plugin sync`).
        let outcomes = niubash_runtime::plugins::sync::update_all_to_ref_tip();
        if outcomes.is_empty() {
            println!("(no sources installed)");
            return Ok(());
        }
        for outcome in outcomes {
            let marker = match outcome.outcome.as_str() {
                "updated" => niubash_runtime::text_style::green("updated"),
                "skipped" => niubash_runtime::text_style::yellow("skipped"),
                _ => niubash_runtime::text_style::red("failed"),
            };
            println!("  {marker} {:<15} {}", outcome.id, outcome.detail);
        }
        return Ok(());
    };
    let request = PluginSourceRequest {
        adapter: None,
        // Empty origin means "re-fetch the registered origin".
        origin: parsed
            .url
            .clone()
            .or(parsed.path.clone())
            .unwrap_or_default(),
        ref_name: parsed.ref_name,
        expected_checksum: parsed.checksum,
        id: None,
    };
    let summary =
        niubash_runtime::plugins::sources::update_source(&id, request.to_install_request())?;
    println!(
        "{} source '{}' to {}",
        niubash_runtime::text_style::green("Updated"),
        summary.id,
        niubash_runtime::text_style::dim(&summary.version)
    );
    println!("tree sha256 {}", summary.checksum_sha256);
    if summary.previous.is_some() {
        println!(
            "{} niu plugin source rollback {}",
            niubash_runtime::text_style::dim("undo:"),
            summary.id
        );
    }
    Ok(())
}

fn run_plugin_source_rollback_command(args: &[String]) -> anyhow::Result<()> {
    let Some(id) = args.first() else {
        anyhow::bail!("plugin source rollback requires a source id");
    };
    let summary = niubash_runtime::plugins::sources::rollback_source(id)?;
    println!(
        "{} source '{}' to {}",
        niubash_runtime::text_style::green("Rolled back"),
        summary.id,
        niubash_runtime::text_style::dim(&summary.version)
    );
    Ok(())
}

fn run_plugin_source_verify_command(args: &[String]) -> anyhow::Result<()> {
    let Some(id) = args.first() else {
        anyhow::bail!("plugin source verify requires a source id");
    };
    let report = niubash_runtime::plugins::sources::verify_source(id)?;
    if report.degraded {
        anyhow::bail!(
            "source '{}' tree is missing ({})",
            report.id,
            report.recorded_checksum
        );
    }
    if !report.verified {
        anyhow::bail!(
            "checksum mismatch for source '{}': recorded {}, got {}",
            report.id,
            report.recorded_checksum,
            report.actual_checksum.as_deref().unwrap_or("?")
        );
    }
    println!(
        "{} source '{}' tree checksum {}",
        niubash_runtime::text_style::green("Verified"),
        report.id,
        report.recorded_checksum
    );
    Ok(())
}

/// `niu plugin add <catalog-id | owner/repo | url | path>` — the
/// vim-plug/lazy.nvim front door, declarative since §14.6.3: the entry is
/// appended to `~/.niubash/plugins.toml` and `niu plugin sync` installs it
/// (fetch gate only; everything lands untrusted). Catalog ids resolve to
/// their official origin, `owner/repo` expands to GitHub; wild repos and
/// bpkg trees come in the same way (auto-detected layout, per-plugin id).
fn run_plugin_add_command(args: &[String]) -> anyhow::Result<()> {
    // `add -h` is a help request, not an unknown-option error (niubash#176):
    // the verb-level flags render here, the plugin-level usage one level up.
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_plugin_add_usage();
        return Ok(());
    }
    let parsed = parse_plugin_source_args(args)?;
    // Recipe routing (lazy-study merge): a bare word that is NOT a catalog
    // id but names a recipe routes through the recipe's driver (generic
    // file source with entry / direct binary download / manager-asset
    // chain). Catalog ids — the one-per-machine managers — keep the
    // spec-declaring flow below: the spec stays their source of truth.
    if parsed.target.is_some() && parsed.url.is_none() && parsed.path.is_none() {
        let target = parsed.target.clone().unwrap();
        if niubash_runtime::plugins::catalog::catalog_entry(&target).is_none()
            && niubash_runtime::plugins::recipes::recipe(&target).is_some()
        {
            return run_plugin_recipe_add(&target);
        }
    }
    let mut request = resolve_source_install_request(parsed.clone())?;
    // A local directory wins over the catalog (niubash#176): a cwd that
    // happens to contain an `oh-my-bash/` tree must not be silently
    // re-resolved to the official GitHub origin — the user spelled a local
    // path, and the path is stabilized below so later syncs (run from any
    // directory) resolve the same tree instead of a cwd-relative strand.
    if std::path::Path::new(&request.origin).exists() {
        request.origin = stable_local_target(&request.origin);
    }
    // Catalog shorthand first: `niu plugin add oh-my-bash` knows the origin
    // — but only for spellings that are NOT a local path (guarded above).
    if request.adapter.is_none() && !std::path::Path::new(&request.origin).exists() {
        if let Some(entry) = niubash_runtime::plugins::catalog::catalog_entry(&request.origin) {
            request.adapter = Some(entry.id.to_string());
            request.origin = entry.origin.to_string();
        }
    }
    request.origin = niubash_runtime::plugins::sources::normalize_origin(&request.origin);

    // Spec entry target: the *reproducible origin*, in the user's spelling
    // when that spelling resolves back to the same origin. When an explicit
    // origin was pinned (`--path`/`--url`), the entry MUST carry that
    // origin — a catalog-id spelling would re-resolve to the official
    // GitHub URL at sync time and clone from the network instead of the
    // user's tree. The id stays pinned (the catalog/adapter id), so
    // enable/disable and later syncs match the record directly.
    let mut spec = niubash_runtime::plugins::spec::load_spec()?.unwrap_or_default();
    let explicit_origin = parsed.url.clone().or_else(|| parsed.path.clone());
    let entry_target = explicit_origin.unwrap_or_else(|| {
        parsed
            .target
            .clone()
            .filter(|target| !target.is_empty())
            .unwrap_or_else(|| request.origin.clone())
    });
    // A local path target is stored cwd-independent (niubash#176): the raw
    // relative spelling (`./omb`, a bare dir name) would strand the spec
    // entry the moment sync runs from another directory.
    let entry_target = if std::path::Path::new(&entry_target).exists() {
        stable_local_target(&entry_target)
    } else {
        entry_target
    };
    // The entry id: the user's `--id` when given; else the manager id for
    // one-per-machine managers. Per-install shapes (wild file sources,
    // adopted bpkg trees) leave the id unset — sync derives it from the
    // origin tail and binds it back into the spec on first run.
    let entry_id = request.id.clone().or_else(|| {
        request.adapter.as_deref().and_then(|kind| {
            let adapter = niubash_runtime::plugins::sources::adapter_for(kind)?;
            (!adapter.per_install_id()).then(|| kind.to_string())
        })
    });
    // Persist the explicit kind when it is not already implied by the id:
    // a tree adopted as `bpkg` must re-install through the bpkg adapter on
    // later syncs (and refuse if its manifest stops matching), not silently
    // fall back to wild-file detection.
    let entry_kind = request
        .adapter
        .clone()
        .filter(|kind| !kind.is_empty() && entry_id.as_deref() != Some(kind.as_str()));
    if let Some((existing, installed)) = niubash_runtime::plugins::sync::declared_entry_state(
        &spec,
        &entry_target,
        entry_id.as_deref(),
        &request.origin,
    ) {
        // The removal hint must work for the state it names (niubash#176):
        // a stranded declaration (declared, never installed) has no source
        // to remove — `niu plugin source remove` would just fail, and
        // `niu plugin sync --prune` is the way out (wt83 #174).
        let removal_hint = if installed {
            format!("`niu plugin source remove {existing}` uninstalls")
        } else {
            "the entry is declared but not installed (a stranded declaration) — \
             `niu plugin sync --prune` removes it"
                .to_string()
        };
        anyhow::bail!(
            "'{}' is already declared in {} (entry target '{}'); \
             enable/disable manage it, {removal_hint}",
            existing,
            niubash_runtime::plugins::spec::spec_path().display(),
            entry_target,
        );
    }
    spec.sources
        .push(niubash_runtime::plugins::spec::SpecSource {
            target: entry_target.clone(),
            id: entry_id.clone(),
            kind: entry_kind,
            ref_name: request.ref_name.clone(),
            theme: None,
            enable: Vec::new(),
        });
    niubash_runtime::plugins::spec::save_spec(&spec)?;

    // Fetch gate notice, then the reconciliation that installs it. The
    // registry snapshot taken first is how "declared (already installed)"
    // stays honest: a record whose (id, installed_at) predates this add was
    // adopted, never fetched — whatever spelling matched it (catalog id,
    // url, path) and whichever path found it (entry resolution or the
    // reconciler's identity adoption).
    let registry_before: Vec<(String, String)> =
        niubash_runtime::plugins::sources::read_source_registry()
            .into_iter()
            .map(|record| (record.id, record.installed_at))
            .collect();
    let display_id = request
        .adapter
        .clone()
        .unwrap_or_else(|| "(auto-detect)".to_string());
    print_source_trust_boundary(&display_id, &request);
    // Failure rollback (wt83 #174): the entry above was persisted so the
    // reconciler could see it; any failure below takes it back out — a
    // failed add must not leave a spec strand no verb can remove.
    let rollback_declaration = |err: anyhow::Error| -> anyhow::Error {
        match niubash_runtime::plugins::sync::remove_declared_entry(
            &entry_target,
            entry_id.as_deref(),
        ) {
            Ok(true) => err.context(format!(
                "the '{entry_target}' spec declaration was rolled back"
            )),
            _ => err,
        }
    };
    // The user's `--checksum` rides into the reconciler's install request
    // (wt83 #173): the fetch refuses a mismatched tree, exactly like
    // `niu plugin source add --checksum` always did. Absent, the fetch
    // gate + untrusted landing stay the only guards.
    let report = match niubash_runtime::plugins::sync::sync_spec(
        niubash_runtime::plugins::sync::SyncOptions {
            prune: false,
            checksum_pin: request
                .expected_checksum
                .clone()
                .map(|checksum| (entry_target.clone(), checksum)),
            ..niubash_runtime::plugins::sync::SyncOptions::default()
        },
    ) {
        Ok(report) => report,
        Err(err) => return Err(rollback_declaration(err)),
    };
    print_sync_rows(&report.rows);
    // The add must fail honestly when the new entry could not install (an
    // unrecognized layout, an empty repo, a refused kind pin): the
    // reconciler records that as a failed row; exiting 0 would hide it.
    if let Some(row) = report.rows.iter().find(|row| {
        row.action == "failed"
            && (Some(row.id.as_str()) == entry_id.as_deref() || row.id == entry_target)
    }) {
        let removed = niubash_runtime::plugins::sync::remove_declared_entry(
            &entry_target,
            entry_id.as_deref(),
        )
        .unwrap_or(false);
        anyhow::bail!(
            "could not install '{}': {}{}",
            row.id,
            row.detail,
            if removed {
                " (spec declaration rolled back)"
            } else {
                ""
            }
        );
    }
    let new_id = report
        .rows
        .iter()
        .rev()
        .find(|row| {
            row.action == "awaiting-trust" || row.action == "activated" || row.action == "unchanged"
        })
        .map(|row| row.id.clone());
    let adopted_existing = new_id
        .as_ref()
        .map(|id| {
            let adopted_by_report = report.adopted.iter().any(|adopted| adopted == id);
            let installed_before = niubash_runtime::plugins::sources::read_source_registry()
                .into_iter()
                .any(|record| {
                    record.id == *id
                        && registry_before
                            .contains(&(record.id.clone(), record.installed_at.clone()))
                });
            adopted_by_report || installed_before
        })
        .unwrap_or(false);
    if let Some(id) = &new_id {
        if let Some(record) = niubash_runtime::plugins::sources::read_source_registry()
            .into_iter()
            .find(|record| record.id == *id)
        {
            if adopted_existing {
                println!(
                    "{} source '{}' — declared in the spec (already installed at {})",
                    niubash_runtime::text_style::green("Declared"),
                    record.id,
                    niubash_runtime::text_style::dim(&record.path.display().to_string())
                );
            } else {
                println!(
                    "{} source '{}' ({}) into {}",
                    niubash_runtime::text_style::green("Installed"),
                    record.id,
                    niubash_runtime::text_style::dim(&record.version),
                    niubash_runtime::text_style::dim(&record.path.display().to_string())
                );
                println!(
                    "license {} | tree sha256 {}",
                    record.license, record.checksum_sha256
                );
                if let Some(commit) = &record.commit_sha {
                    println!(
                        "pinned commit {} ({})",
                        niubash_runtime::text_style::dim(commit),
                        niubash_runtime::text_style::dim(
                            "niu plugin restore restores exactly this"
                        )
                    );
                }
            }
            if !record.trusted {
                println!("the source is untrusted; review it, then run:");
                println!("  niu plugin trust {}", record.id);
                println!(
                    "  {}",
                    niubash_runtime::text_style::dim(&format!(
                        "then activate it (or single assets) with `niu plugin enable {}` \
                         (wild sources pick files: `niu plugin enable {}/<file>.bash`)",
                        record.id, record.id
                    ))
                );
            }
        }
    }
    print_undeclared_hints(&report);
    Ok(())
}

/// Print the reconciler's rows in one stable shape.
fn print_sync_rows(rows: &[niubash_runtime::plugins::sync::SyncRow]) {
    for row in rows {
        let marker = match row.action.as_str() {
            "installed" | "activated" | "removed" => {
                niubash_runtime::text_style::green(&row.action)
            }
            "awaiting-trust" | "deactivated" | "unchanged" | "merged" | "reconciled" => {
                niubash_runtime::text_style::yellow(&row.action)
            }
            "degraded" => niubash_runtime::text_style::yellow(&row.action),
            "deferred" => niubash_runtime::text_style::dim(&row.action),
            _ => niubash_runtime::text_style::red(&row.action),
        };
        println!("  {marker} {:<18} {}", row.id, row.detail);
    }
}

fn print_undeclared_hints(report: &niubash_runtime::plugins::sync::SyncReport) {
    if report.undeclared.is_empty() {
        return;
    }
    println!(
        "{}",
        niubash_runtime::text_style::yellow("installed but not declared in the spec:")
    );
    for id in &report.undeclared {
        println!("  {id}  (keep by declaring it, or `niu plugin sync --prune`)");
    }
}

fn run_plugin_recipe_command(args: &[String]) -> anyhow::Result<()> {
    let Some(verb) = args.first() else {
        print_plugin_recipe_usage();
        return Ok(());
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => {
            print_plugin_recipe_usage();
            Ok(())
        }
        "list" => run_plugin_recipe_list_command(&args[1..]),
        "show" => {
            let Some(id) = args.get(1) else {
                anyhow::bail!("plugin recipe show requires a recipe id")
            };
            run_plugin_recipe_show_command(id)
        }
        "add" => {
            let Some(id) = args.get(1) else {
                anyhow::bail!("plugin recipe add requires a recipe id")
            };
            run_plugin_recipe_add(id)
        }
        unknown => anyhow::bail!("unknown plugin recipe subcommand '{unknown}'"),
    }
}

fn print_plugin_recipe_usage() {
    println!("Usage:  niu plugin recipe <command>");
    println!();
    println!("  list [--category <c>] [--json]   Index rows (bash-ecosystem assets)");
    println!("  show <id>                        One recipe: driver, license, state");
    println!("  add <id>                         Install through the recipe's driver");
    println!();
    println!("Categories: manager theme plugin alias completion prompt");
    println!("Drivers:    git (tree source) · package (recipe recommends your");
    println!("            package manager — niu downloads nothing) · info-only");
}

fn run_plugin_recipe_list_command(args: &[String]) -> anyhow::Result<()> {
    let json = args.iter().any(|arg| arg == "--json");
    let mut category: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--category" {
            category = iter.next().cloned();
        }
    }
    let rows: Vec<serde_json::Value> = niubash_runtime::plugins::recipes::recipes()
        .iter()
        .filter(|recipe| {
            category
                .as_deref()
                .map_or(true, |wanted| recipe.category.as_str() == wanted)
        })
        .map(|recipe| {
            serde_json::json!({
                "id": recipe.id,
                "category": recipe.category.as_str(),
                "summary": recipe.summary,
                "license": recipe.license,
                "url": recipe.url,
                "driver": match &recipe.driver {
                    Some(niubash_runtime::plugins::recipes::RecipeDriver::Git { .. }) => "git",
                    Some(niubash_runtime::plugins::recipes::RecipeDriver::Download { .. }) => {
                        "package"
                    }
                    None => "",
                },
                "state": format!("{:?}", niubash_runtime::plugins::recipes::recipe_state(&recipe.id)),
            })
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!(
        "{}",
        niubash_runtime::text_style::bold("Niubash plugin recipe index")
    );
    println!(
        "{}",
        niubash_runtime::text_style::dim("  data rows — install with `niu plugin add <id>`")
    );
    println!();
    for row in &rows {
        println!(
            "  {:<28} {:<12} {}",
            row["id"].as_str().unwrap_or_default(),
            row["category"].as_str().unwrap_or_default(),
            row["summary"].as_str().unwrap_or_default()
        );
    }
    println!(
        "{}",
        niubash_runtime::text_style::dim(&format!("{} recipes", rows.len()))
    );
    Ok(())
}

fn run_plugin_recipe_show_command(id: &str) -> anyhow::Result<()> {
    use niubash_runtime::plugins::recipes::{RecipeDriver, RecipeState};
    let recipe = niubash_runtime::plugins::recipes::recipe(id).ok_or_else(|| {
        anyhow::anyhow!("unknown recipe '{id}'; browse with `niu plugin recipe list`")
    })?;
    println!("{}", niubash_runtime::text_style::bold(&recipe.id));
    println!("  category  {}", recipe.category.as_str());
    println!("  summary   {}", recipe.summary);
    println!("  license   {}", recipe.license);
    println!("  url       {}", recipe.url);
    match &recipe.driver {
        Some(RecipeDriver::Git {
            kind,
            entry,
            origin,
        }) => {
            println!("  driver    git ({kind})");
            if let Some(origin) = origin {
                println!("  origin    {origin}");
            }
            if let Some(entry) = entry {
                println!("  entry     {entry}");
            }
        }
        Some(RecipeDriver::Download { version, .. }) => {
            println!("  driver    package-manager recommendation ({version})");
            println!(
                "            (niu does not download binaries — download retraction 2026-10-04)"
            );
        }
        None => println!("  driver    (info-only — no niu install driver)"),
    }
    if let Some(manager) = &recipe.manager {
        println!("  manager   {manager} (asset rides on this source)");
    }
    println!(
        "  state     {}",
        match niubash_runtime::plugins::recipes::recipe_state(id) {
            RecipeState::Available => "not installed".to_string(),
            RecipeState::ManagerAsset { manager_state } => format!("manager: {manager_state}"),
            RecipeState::InfoOnly => "info-only".to_string(),
        }
    );
    println!();
    println!("  niu plugin add {id}");
    Ok(())
}

/// Shared install path for `niu plugin add <recipe-id>` and
/// `niu plugin recipe add <id>`: run the driver, then print what happened
/// and the next verbs (health-style "name the repair", study §10.1).
fn run_plugin_recipe_add(id: &str) -> anyhow::Result<()> {
    let report = niubash_runtime::plugins::recipes::install(id)?;
    println!(
        "{}",
        niubash_runtime::text_style::green(&format!("recipe '{}':", report.recipe_id))
    );
    println!("  {}", report.summary);
    if !report.next.is_empty() {
        println!("next:");
        for step in &report.next {
            println!("  {step}");
        }
    }
    Ok(())
}

/// `niu plugin distro <verb>` — collections (LazyVim extras pattern):
/// data manifests of recipe ids; built-in or imported; apply never
/// auto-trusts (study §8/§10.2).
fn run_plugin_distro_command(args: &[String]) -> anyhow::Result<()> {
    let Some(verb) = args.first() else {
        print_plugin_distro_usage();
        return Ok(());
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => {
            print_plugin_distro_usage();
            Ok(())
        }
        "list" => {
            use niubash_runtime::plugins::distros::{collections, CollectionOrigin};
            let listings = collections();
            println!(
                "{}",
                niubash_runtime::text_style::bold("Niubash plugin collections")
            );
            println!(
                "{}",
                niubash_runtime::text_style::dim(
                    "  apply installs recipes; trust stays an explicit review step"
                )
            );
            println!();
            for listing in &listings {
                let origin = match &listing.origin {
                    CollectionOrigin::Builtin => "built-in".to_string(),
                    CollectionOrigin::Imported { origin, .. } => {
                        format!("imported: {origin}")
                    }
                };
                println!(
                    "  {:<14} {:<10} {}",
                    listing.collection.name, origin, listing.collection.description
                );
                let entries: Vec<&str> = listing
                    .collection
                    .entry
                    .iter()
                    .map(|entry| entry.recipe.as_str())
                    .collect();
                println!(
                    "  {}",
                    niubash_runtime::text_style::dim(&entries.join(", "))
                );
            }
            Ok(())
        }
        "import" => {
            let Some(origin) = args.get(1) else {
                anyhow::bail!("plugin distro import requires a git repo or a directory")
            };
            let collection = niubash_runtime::plugins::distros::import(origin)?;
            println!(
                "{} collection '{}' ({} recipes) — apply with `niu plugin distro apply {}`",
                niubash_runtime::text_style::green("Imported"),
                collection.name,
                collection.entry.len(),
                collection.name
            );
            Ok(())
        }
        "remove" => {
            let Some(name) = args.get(1) else {
                anyhow::bail!("plugin distro remove requires a collection name")
            };
            niubash_runtime::plugins::distros::remove(name)?;
            println!(
                "{} collection '{name}'",
                niubash_runtime::text_style::green("Removed")
            );
            Ok(())
        }
        "apply" => {
            let Some(name) = args.get(1) else {
                anyhow::bail!("plugin distro apply requires a collection name")
            };
            run_plugin_distro_apply(name)
        }
        unknown => anyhow::bail!("unknown plugin distro subcommand '{unknown}'"),
    }
}

fn run_plugin_distro_apply(name: &str) -> anyhow::Result<()> {
    let outcome = niubash_runtime::plugins::distros::apply(name)?;
    println!(
        "{}",
        niubash_runtime::text_style::green(&format!("collection '{}':", outcome.name))
    );
    for report in &outcome.reports {
        println!("  - {}", report.summary);
    }
    // Collected failures (lazy.nvim Spec:log pattern): name the entry and
    // the error; the rest of the collection still landed.
    for (recipe, error) in &outcome.failures {
        println!(
            "  {} {}: {}",
            niubash_runtime::text_style::red("failed"),
            recipe,
            error
        );
    }
    let landed = outcome.reports.iter().any(|report| !report.next.is_empty());
    if landed {
        println!("next:");
        // Deduplicate the per-entry next verbs (trust/enable chains overlap).
        let mut seen: Vec<&String> = Vec::new();
        for report in &outcome.reports {
            for step in &report.next {
                if !seen.contains(&step) {
                    seen.push(step);
                    println!("  {step}");
                }
            }
        }
        println!(
            "{}",
            niubash_runtime::text_style::dim(
                "sources stay untrusted until reviewed; restart niu after enabling"
            )
        );
    } else if outcome.failures.is_empty() {
        println!("  everything already active");
    }
    if !outcome.failures.is_empty() {
        anyhow::bail!(
            "{} of {} collection entries failed",
            outcome.failures.len(),
            outcome.reports.len() + outcome.failures.len()
        );
    }
    Ok(())
}

fn print_plugin_distro_usage() {
    println!("Usage:  niu plugin distro <command>");
    println!();
    println!("  list                        Built-in + imported collections");
    println!("  import <repo|path>          Import a collection (niu-collection.toml)");
    println!("  remove <name>               Remove an imported collection");
    println!("  apply <name>                Install every recipe in the collection");
    println!();
    println!("Built-ins: minimal (completions only) · recommended (oh-my-bash + theme +");
    println!("completions) · full (both frameworks + hooks + fzf + starship)");
}

/// `niu plugin mirror` — the China-network git mirroring control face
/// (§14.8, git-only since the download retraction 2026-10-04). The
/// pipeline is configurable, not curated: the user pastes a mirror URL
/// they trust, `set` makes every plugin git fetch go through it via
/// insteadOf, and the lockfile keeps canonical GitHub origins.
fn run_plugin_mirror_command(args: &[String]) -> anyhow::Result<()> {
    use niubash_runtime::plugins::mirrors;
    let Some(verb) = args.first() else {
        print_plugin_mirror_usage();
        return Ok(());
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => {
            print_plugin_mirror_usage();
            Ok(())
        }
        "list" | "show" => {
            match mirrors::load_mirror_config() {
                Err(err) => {
                    println!(
                        "{} mirrors.toml unreadable: {err}",
                        niubash_runtime::text_style::yellow("!")
                    );
                    println!(
                        "  fix or delete {} (git fetches currently go direct)",
                        mirrors::mirrors_path().display()
                    );
                }
                Ok(_) => {
                    let resolved = mirrors::resolve_active_mirror();
                    if resolved.is_none() {
                        println!(
                            "{} direct connection (no mirror)",
                            niubash_runtime::text_style::green("mirror:")
                        );
                    } else {
                        println!(
                            "{} custom mirror active",
                            niubash_runtime::text_style::green("mirror:")
                        );
                        if let Some(base) = resolved.git_instead_of_base.as_deref() {
                            println!("  git         {base} (insteadOf https://github.com/)");
                        } else {
                            println!(
                                "  {} active = custom but no git_instead_of set — fetches go direct",
                                niubash_runtime::text_style::yellow("!")
                            );
                        }
                        let unknown = mirrors::load_mirror_config()
                            .ok()
                            .flatten()
                            .and_then(|config| config.active)
                            .is_some_and(|active| active != "none" && active != "custom");
                        if unknown {
                            println!(
                                "{} unknown 'active' value — degrading to direct",
                                niubash_runtime::text_style::yellow("!")
                            );
                        }
                    }
                    println!("  config      {}", mirrors::mirrors_path().display());
                }
            }
            println!();
            println!("Set one with:    niu plugin mirror set <mirror-url>");
            println!("Go direct with: niu plugin mirror set none");
            Ok(())
        }
        "set" => {
            let Some(target) = args.get(1) else {
                anyhow::bail!("plugin mirror set requires a mirror url, or 'none' for direct")
            };
            if target == "none" {
                mirrors::set_direct()?;
                println!(
                    "{} git fetches now go direct",
                    niubash_runtime::text_style::green("Set:")
                );
                return Ok(());
            }
            let base = mirrors::set_custom_git_mirror(target)?;
            println!(
                "{} git mirror {base}",
                niubash_runtime::text_style::green("Set:")
            );
            println!(
                "  plugin git fetches now rewrite https://github.com/ through it (insteadOf);"
            );
            println!(
                "  the spec and registry keep canonical GitHub origins (edit {} to change).",
                mirrors::mirrors_path().display()
            );
            println!(
                "  {}",
                niubash_runtime::text_style::dim(
                    "community mirrors may disappear at any time — verify the one you pasted"
                )
            );
            Ok(())
        }
        unknown => anyhow::bail!("unknown plugin mirror subcommand '{unknown}'"),
    }
}

/// `niu config` — persisted user settings (`~/.niubash/config.toml`, the
/// mirrors.toml convention). Deliberately tiny: one flat file, one setting
/// today (the niubash#249 command-not-found hint policy). Any read problem
/// degrades to the default in the runtime; `get`/`list` surface values as
/// resolved (file, overridden by `NIU_COMMAND_NOT_FOUND_HINT`).
fn run_config_command(args: &[String]) -> anyhow::Result<()> {
    use niubash_runtime::config::{
        resolve_command_not_found_hint, set_command_not_found_hint, user_config_path,
        CommandNotFoundHint, COMMAND_NOT_FOUND_HINT_KEY,
    };
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            print_config_usage();
            Ok(())
        }
        Some("list" | "show") => {
            println!("Settings (persisted in {}):", user_config_path().display());
            println!(
                "  {COMMAND_NOT_FOUND_HINT_KEY} = {}",
                resolve_command_not_found_hint().as_str()
            );
            Ok(())
        }
        Some("get") => {
            let Some(key) = args.get(1) else {
                anyhow::bail!("config get requires a key (`niu config list` shows them)")
            };
            match key.as_str() {
                COMMAND_NOT_FOUND_HINT_KEY => {
                    println!("{}", resolve_command_not_found_hint().as_str());
                    Ok(())
                }
                unknown => anyhow::bail!(
                    "unknown config key '{unknown}' (`niu config list` shows the valid keys)"
                ),
            }
        }
        Some("set") => {
            let Some(key) = args.get(1) else {
                anyhow::bail!(
                    "config set requires a key and a value (`niu config list` shows them)"
                )
            };
            let Some(value) = args.get(2) else {
                anyhow::bail!("config set {key} requires a value")
            };
            match key.as_str() {
                COMMAND_NOT_FOUND_HINT_KEY => {
                    let Some(hint) = CommandNotFoundHint::parse(value) else {
                        anyhow::bail!(
                            "invalid value '{value}' for {COMMAND_NOT_FOUND_HINT_KEY}: expected off or wpm"
                        )
                    };
                    set_command_not_found_hint(hint)?;
                    println!(
                        "{} {COMMAND_NOT_FOUND_HINT_KEY} = {}",
                        niubash_runtime::text_style::green("Set:"),
                        hint.as_str()
                    );
                    if hint == CommandNotFoundHint::Wpm {
                        println!("  new niu sessions suggest one `wpm search --name` line after a");
                        println!(
                            "  high-confidence near-miss (wpm only — never winget/scoop/choco)."
                        );
                    } else {
                        println!("  command not found prints only the GNU one-liner (default).");
                    }
                    Ok(())
                }
                unknown => anyhow::bail!(
                    "unknown config key '{unknown}' (`niu config list` shows the valid keys)"
                ),
            }
        }
        Some(unknown) => {
            anyhow::bail!("unknown config subcommand '{unknown}' (try: niu config help)")
        }
    }
}

fn print_config_usage() {
    println!("Usage:  niu config <command>");
    println!();
    println!("Persisted user settings (~/.niubash/config.toml).");
    println!();
    println!("  get <key>          Print a setting's effective value");
    println!("  set <key> <value>  Persist a setting");
    println!("  list               Show every setting and its current value");
    println!();
    println!("Settings:");
    println!(
        "  {k}",
        k = niubash_runtime::config::COMMAND_NOT_FOUND_HINT_KEY
    );
    println!("      Package-search suggestion printed after \"command not found\".");
    println!("      off (default) | wpm — wpm suggests one `wpm search --name`");
    println!("      line for high-confidence near-misses; third-party managers are");
    println!("      never recommended (niubash#249).");
}

fn print_plugin_mirror_usage() {
    println!("Usage:  niu plugin mirror <command>");
    println!();
    println!("Transport-layer git mirroring for plugin git fetches (the lockfile");
    println!("keeps canonical origins — the mirror only affects the network");
    println!("request). Configure any mirror you trust; community mirror");
    println!("services are unsupported and may disappear. niu has no HTTP");
    println!("download transport (download retraction 2026-10-04): mirrors are");
    println!("git-only.");
    println!();
    println!("  list                    Show the active mirror and config path");
    println!("  set <mirror-url>        Route git fetches through an insteadOf");
    println!("                          mirror (url gets a trailing '/' if missing)");
    println!("  set none                Go back to direct connection");
    println!();
    println!("Config file (edit directly for exact control):");
    println!("  schema = \"niubash:mirrors@0.1.0\"");
    println!("  active = \"custom\"");
    println!("  [github]");
    println!("  git_instead_of = \"https://your-git-mirror/\"    # git clone/fetch");
}

/// `niu plugin list [--json]` — sources, their assets, and activation
/// state (the first-class inventory of the external ecosystem).
fn run_plugin_list_command(args: &[String]) -> anyhow::Result<()> {
    let json = args.iter().any(|arg| arg == "--json");
    let overview = niubash_runtime::plugins::assets::asset_overview();
    if json {
        println!("{}", serde_json::to_string_pretty(&overview)?);
        return Ok(());
    }
    println!(
        "{}",
        niubash_runtime::text_style::bold("Niubash plugin ecosystem")
    );
    if overview.is_empty() {
        println!(
            "(no sources installed; browse with `niu plugin discover`, \
             install with `niu plugin add <id|owner/repo|url|path>`)"
        );
        return Ok(());
    }
    for report in overview {
        let status = &report.status;
        let marker = match status.state.as_str() {
            "ready" => niubash_runtime::text_style::green("ready"),
            "untrusted" => niubash_runtime::text_style::yellow("untrusted"),
            _ => niubash_runtime::text_style::red("degraded"),
        };
        let trust = if status.record.trusted {
            format!("trusted: {}", status.record.trust_policy.as_str())
        } else {
            "untrusted".to_string()
        };
        println!(
            "  {} {:<15} {:<12} {:<18} {}",
            marker,
            status.record.id,
            status.record.version,
            trust,
            niubash_runtime::text_style::dim(&status.record.license)
        );
        if status.degraded {
            println!(
                "     {}",
                niubash_runtime::text_style::dim(&format!(
                    "tree missing — repair with `niu plugin restore {}`",
                    status.record.id
                ))
            );
            continue;
        }
        if !status.record.trusted {
            println!(
                "     {}",
                niubash_runtime::text_style::dim(&format!(
                    "assets hidden until trust: `niu plugin trust {}`",
                    status.record.id
                ))
            );
            continue;
        }
        // Group assets by kind with the enabled marker.
        let mut lines: Vec<String> = Vec::new();
        for row in &report.assets {
            let mark = if row.enabled { "*" } else { " " };
            // §14.6.1 honest presentation: wild candidates carry their tag
            // (installer/test-like, script, fragment, bpkg script) right in
            // the listing — a tagged file is visible as such, never hidden.
            let tag = row
                .asset
                .tag
                .as_deref()
                .filter(|tag| !tag.is_empty())
                .map(|tag| format!(" [{tag}]"))
                .unwrap_or_default();
            lines.push(format!("{}{}{}", mark, row.asset.name, tag));
            if lines.len() >= 40 {
                break;
            }
        }
        // niubash#179 L05-3: a *gutted* tree (directory present, contents
        // emptied) still reads "ready" here — the checksum state is the
        // only remaining truth, so name the verb that reports it.
        if lines.is_empty() {
            println!(
                "     {}",
                niubash_runtime::text_style::dim(&format!(
                    "no assets found in the tree — verify with `niu plugin source verify {}`",
                    status.record.id
                ))
            );
        }
        if !lines.is_empty() {
            println!(
                "     {}",
                niubash_runtime::text_style::dim(&lines.join("  "))
            );
        }
        if !report.activated {
            println!(
                "     {}",
                niubash_runtime::text_style::dim(&format!(
                    "not wired into ~/.niubashrc — `niu plugin enable {}`",
                    status.record.id
                ))
            );
        }
    }
    println!(
        "{}",
        niubash_runtime::text_style::dim(
            "* = enabled · themes/plugins/aliases/completions are the manager's own assets"
        )
    );
    Ok(())
}

/// `niu plugin enable|disable <target>` — source id, asset name, or
/// `<source>/<asset>` qualified name.
fn run_plugin_enable_command(args: &[String], enable: bool) -> anyhow::Result<()> {
    let Some(target) = args.first() else {
        anyhow::bail!(
            "plugin {} requires a source id, asset name, or <source>/<asset>",
            if enable { "enable" } else { "disable" }
        );
    };
    // Tools first (download-driver channel): their enable unit is a PATH
    // managed block; everything else is the asset layer.
    let outcome = if enable {
        niubash_runtime::plugins::recipes::enable(target)?
    } else {
        niubash_runtime::plugins::recipes::disable(target)?
    };
    println!(
        "{} {}",
        if enable {
            niubash_runtime::text_style::green("Enabled:")
        } else {
            niubash_runtime::text_style::green("Disabled:")
        },
        outcome.summary
    );
    println!(
        "  {} {}",
        niubash_runtime::text_style::dim("undo:"),
        outcome.undo
    );
    println!(
        "  {}",
        niubash_runtime::text_style::dim(
            "restart niu (or reload ~/.niubashrc) for the change to take effect"
        )
    );
    Ok(())
}

/// `niu plugin restore [id]` — lockfile repair (lazy.nvim `:Lazy restore`):
/// rebuild every (or one) source tree exactly as pinned in the registry.
fn run_plugin_restore_command(args: &[String]) -> anyhow::Result<()> {
    if let Some(id) = args.first() {
        let outcome = niubash_runtime::plugins::sources::restore_source(id)?;
        println!(
            "{} source '{}': {}",
            niubash_runtime::text_style::green("Restored"),
            outcome.id,
            niubash_runtime::text_style::dim(&outcome.detail)
        );
        return Ok(());
    }
    let records = niubash_runtime::plugins::sources::read_source_registry();
    if records.is_empty() {
        println!("(no sources installed)");
        return Ok(());
    }
    for record in records {
        match niubash_runtime::plugins::sources::restore_source(&record.id) {
            Ok(outcome) => println!(
                "  {} {} ({})",
                niubash_runtime::text_style::green("restored"),
                outcome.id,
                niubash_runtime::text_style::dim(&outcome.detail)
            ),
            Err(err) => println!(
                "  {} {} ({})",
                niubash_runtime::text_style::yellow("skipped"),
                record.id,
                niubash_runtime::text_style::dim(&err.to_string())
            ),
        }
    }
    Ok(())
}

/// `niu plugin sync` — reconcile the declarative spec with the machine
/// (§14.6.3; lazy.nvim `:Lazy sync`): install declared-but-missing sources
/// (fetch gate only — trust never auto-flips), materialize the managed rc
/// blocks from the spec idempotently, and suggest cleanup for installed
/// sources the spec no longer declares (`--prune` is the explicit confirm).
/// `--bootstrap` is the quiet rc-startup form: zero output when clean.
fn run_plugin_sync_command(args: &[String]) -> anyhow::Result<()> {
    let mut prune = false;
    let mut bootstrap = false;
    let mut adopt = false;
    for arg in args {
        match arg.as_str() {
            "--prune" => prune = true,
            "--bootstrap" => bootstrap = true,
            "--adopt" => adopt = true,
            unknown => anyhow::bail!("unknown plugin sync option '{}'", unknown),
        }
    }
    if bootstrap
        && std::env::var_os("NIU_PLUGIN_BOOTSTRAP") == Some(std::ffi::OsString::from("off"))
    {
        return Ok(());
    }
    let report =
        niubash_runtime::plugins::sync::sync_spec(niubash_runtime::plugins::sync::SyncOptions {
            prune,
            adopt,
            startup: bootstrap,
            checksum_pin: None,
        })?;
    // niubash#196: a hand-written theme variable outside the managed blocks
    // is not the supported channel — `niu plugin sync` rewrites the managed
    // blocks from the spec, so the rc line is ignored/dropped. Point at the
    // spec once (startup form stays silent; the stderr nag must not pollute
    // every shell boot).
    let stray = niubash_runtime::plugins::assets::stray_theme_assignment_lines();
    if !stray.is_empty() {
        eprintln!(
            "niu plugin sync: found hand-written theme assignment(s) outside the managed \
             blocks: {}. The theme belongs in {} as `theme = \"...\"` on the source entry — \
             hand-written OSH_THEME lines are overwritten by sync.",
            stray.join("; "),
            niubash_runtime::plugins::spec::spec_path().display()
        );
    }
    if bootstrap {
        // Startup form: silent when clean; install notices only otherwise
        // (iron law 2 keeps degraded state visible). In imperative mode
        // (no spec) there is nothing to reconcile — undeclared sources are
        // the LEGACY state, not drift, and startup must stay silent about
        // them (sync.rs §4; the 1.3.0 startup nag printed them forever).
        for row in &report.rows {
            if row.action == "unchanged" {
                continue;
            }
            eprintln!(
                "niu plugin sync: {} {} — {}",
                row.action, row.id, row.detail
            );
        }
        if report.spec_present {
            for id in &report.undeclared {
                eprintln!(
                    "niu plugin sync: {id} installed but not declared \
                     (`niu plugin sync` for details)"
                );
            }
        }
        return Ok(());
    }
    if !report.spec_present {
        // Imperative mode (no spec): the 1.3.0 dead end printed an advice
        // line its own verb could not fulfill. List what is installed and
        // the exact migration one-liner, plus the hand-write alternative.
        println!(
            "(no spec at {} — imperative mode)",
            niubash_runtime::plugins::spec::spec_path().display()
        );
        for id in &report.undeclared {
            println!("  {id}  (installed; undeclared)");
        }
        if !report.undeclared.is_empty() {
            println!("declare everything installed in one move (snapshots the live selection):");
            println!("  niu plugin sync --adopt");
            println!("or hand-write the spec, then sync:");
            println!(
                "  {}",
                niubash_runtime::text_style::dim(&format!(
                    "schema = \"{}\"\n\n[[sources]]\ntarget = \"oh-my-bash\"{}\
                     \nenable = [\"git\", \"npm\"]\ntheme  = \"agnoster\"",
                    niubash_runtime::plugins::spec::PLUGIN_SPEC_SCHEMA,
                    "            # catalog id | owner/repo | url | path"
                ))
            );
            println!(
                "  {}",
                niubash_runtime::text_style::dim(
                    "a single source can also be declared with `niu plugin add <target>`"
                )
            );
        }
        return Ok(());
    }
    for id in &report.adopted {
        println!(
            "{} {} {}",
            niubash_runtime::text_style::green("declared"),
            id,
            niubash_runtime::text_style::dim("(installed; adopted into the spec)")
        );
    }
    if !report.adopted.is_empty() {
        println!(
            "{}",
            niubash_runtime::text_style::dim(&format!(
                "adopted {} source(s) into {} — a plain `niu plugin sync` is now a no-op",
                report.adopted.len(),
                niubash_runtime::plugins::spec::spec_path().display()
            ))
        );
    }
    if report.rows.is_empty() && report.undeclared.is_empty() && report.adopted.is_empty() {
        println!("(spec declares nothing — every source listed is undeclared)");
        return Ok(());
    }
    print_sync_rows(&report.rows);
    print_undeclared_hints(&report);
    if report.clean {
        println!(
            "{}",
            niubash_runtime::text_style::dim("spec and machine are in sync")
        );
    }
    Ok(())
}

/// `niu plugin clean` — remove staging leftovers and orphaned trees
/// (vim-plug `:PlugClean`).
fn run_plugin_clean_command(_args: &[String]) -> anyhow::Result<()> {
    let outcomes = niubash_runtime::plugins::sources::clean_sources();
    if outcomes.is_empty() {
        println!("nothing to clean");
        return Ok(());
    }
    for outcome in outcomes {
        let marker = if outcome.outcome == "removed" {
            niubash_runtime::text_style::green("removed")
        } else {
            niubash_runtime::text_style::red("failed")
        };
        println!("  {marker} {:<25} {}", outcome.id, outcome.detail);
    }
    Ok(())
}

/// Normalize a local path target into a cwd-independent absolute form
/// (niubash#176): the spec is reconciled from any directory later, so a
/// relative spelling (`./omb`, a bare dir name in the cwd) is stored as a
/// lexically normalized absolute path instead of raw. `std::fs::canonicalize`
/// is avoided on purpose — on Windows it emits `\\?\`-prefixed paths that
/// would leak into the spec and every comparison against it.
fn stable_local_target(target: &str) -> String {
    let path = std::path::Path::new(target);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return target.to_string(),
        }
    };
    let mut normalized = std::path::PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => {
                normalized.push(other.as_os_str());
            }
        }
    }
    normalized.to_string_lossy().replace('\\', "/")
}

fn print_plugin_add_usage() {
    println!("Usage:  niu plugin add <id|owner/repo|url|path> [options]");
    println!();
    println!("Declare a source in the spec (~/.niubash/plugins.toml) and install it");
    println!("(fetch gate only; everything lands untrusted until you trust it).");
    println!();
    println!("Options:");
    println!("  --id <name>            Pin the source id (else derived/bound at sync)");
    println!("  --ref <ref>            Fetch this ref (first fetch only)");
    println!("  --checksum <sha256>    Refuse the fetch on a tree-checksum mismatch");
    println!("  --path <dir>           Pin the origin to a local directory");
    println!("  --url <git-url>        Pin the origin to a git url");
    println!();
    println!("After adding:");
    println!("  niu plugin trust <id>      Review and activate the source");
    println!("  niu plugin enable <target> Pick assets (or the whole source)");
    println!("  niu plugin sync --prune    Remove a stranded declaration");
}

fn print_plugin_usage() {
    println!("Usage:  niu plugin <command>");
    println!();
    println!("External plugin ecosystem, first-class: any sourceable bash — the");
    println!("known managers (oh-my-bash, bash-it, bash-completion, bpkg trees)");
    println!("and arbitrary wild plugins (single files, gist-style .bash, any");
    println!("repo). The declarative spec ~/.niubash/plugins.toml is the source");
    println!("of truth; these verbs are sugar over it plus `niu plugin sync`.");
    println!("Sources install untrusted and activate only after an explicit");
    println!("trust review.");
    println!();
    println!("Commands:");
    println!("  add <id|owner/repo|url|path> [--id <name>] [--ref <ref>]");
    println!("                           [--checksum <sha256>] [--path <dir>]");
    println!("                           [--url <git-url>]");
    println!("                           Declare + install a source (catalog id,");
    println!("                           GitHub shorthand, url, or local path; the");
    println!("                           entry lands in the spec, untrusted; recipe");
    println!("                           ids route through their driver)");
    println!("  list [--json]            Sources, their assets, activation state");
    println!("  enable <target>          Activate a source or asset (wild sources");
    println!("                           pick files: <id>/<file>.bash)");
    println!("  disable <target>         Deactivate a source or asset");
    println!("  sync [--prune] [--adopt] Reconcile the spec: install declared,");
    println!("                           materialize rc blocks, suggest cleanup");
    println!("                           (--prune removes undeclared sources;");
    println!("                           --adopt declares installed sources into");
    println!("                           the spec, snapshotting the live state)");
    println!("  sync --bootstrap         Same, quiet startup form (rc one-liner)");
    println!("  update [<id>]            Update source(s) to the ref tip (no id =");
    println!("                           all); the lockfile pin moves");
    println!("  restore [<id>]           Rebuild tree(s) from the lockfile pin");
    println!("  rollback <id>            Restore the previous source state");
    println!("  clean                    Remove staging leftovers and orphans");
    println!("  trust <id>               Review and activate a source's assets");
    println!("  discover [--verbose]     Dry ecosystem overview (read-only)");
    println!("  ui                       Menu UI (sections by state, same verbs)");
    println!();
    println!("  recipe <command>         Recipe index: list [--category <c>]");
    println!("                           [--json], show <id>, add <id>");
    println!("  distro <command>         Collections: list, import <repo|path>,");
    println!("                           remove <name>, apply <name>");
    println!("  mirror <command>         Git fetch mirroring: list,");
    println!("                           set <url|none> (insteadOf, git-only)");
    println!();
    println!("  source <command>         Full source protocol (add/trust/sign/");
    println!("                           verify/remove/update/rollback/list)");
    println!();
    println!("Spec (single source of truth, edit it directly):");
    println!("  [[sources]]");
    println!("  target = 'oh-my-bash'         # catalog id | owner/repo | url | path");
    println!("  id    = 'bash-preexec'        # optional (wild sources: bound at first sync)");
    println!("  kind  = 'bpkg'                # optional adapter pin (adopted bpkg trees)");
    println!("  ref   = 'v1.2'                # optional (first fetch only)");
    println!("  enable = ['git', 'npm']       # manager-native selection");
    println!("  theme  = 'agnoster'           # optional (OSH_THEME/…; absent = unmanaged)");
    println!();
    println!("Examples:");
    println!("  niu plugin add oh-my-bash        # catalog shorthand");
    println!("  niu plugin add rcrowley/bash-preexec   # wild GitHub plugin");
    println!("  niu plugin enable bash-preexec/bash-preexec.sh   # pick the file");
    println!("  niu plugin add bpkg --path <dir> # adopt a bpkg-installed tree");
    println!("  niu plugin sync                  # reconcile spec <-> machine");
    println!("  niu plugin sync --adopt          # declare installed sources into");
    println!("                           # the spec (imperative -> declarative;");
    println!("                           # snapshots the live enable/theme)");
    println!("  niu plugin sync --bootstrap      # quiet startup form (rc line)");
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

/// Plain-text usage for `niu setup` (niubash#195): `--help` must render
/// readably everywhere — pipes, redirects, and CI logs included — so it
/// never emits the ANSI logo art, on a tty or off one.
fn show_setup_usage() {
    println!(
        "niu {} — run the interactive setup wizard",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage:");
    println!("  niu setup                       Re-run the interactive setup wizard");
    println!("  niu setup --preset <name>       Apply a preset non-interactively");
    println!("                                  (e.g. recommended, minimal)");
    println!("  niu setup --help                Show this help (plain text)");
    println!();
    println!("The wizard writes ~/.niubashrc and the plugin spec");
    println!("(~/.niubash/plugins.toml). Existing rc files are backed up.");
}

#[cfg(windows)]
fn install_windows_terminal_profile(rest: &[String]) -> anyhow::Result<()> {
    let mut set_default = false;
    let mut quiet = false;

    for arg in rest {
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
fn install_windows_terminal_profile(_rest: &[String]) -> anyhow::Result<()> {
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

fn print_completion_probe(rest: &[String]) -> anyhow::Result<()> {
    let Some(line) = rest.first() else {
        anyhow::bail!("--completion-probe requires an input line");
    };
    let cursor_pos = if let Some(raw) = rest.get(1) {
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
