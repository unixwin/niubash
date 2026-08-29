//! niubash entry point
//!
//! Usage:
//!   niubash                  → interactive REPL
//!   niu -c "command"     → execute one command, print exit code, exit
//!   niubash -C "command"     → execute one REPL-style command, then exit
//!   niubash script.sh        → execute a script file
//!   niubash --help | -h      → usage
//!   niubash --version        → version (niubash / rubash / winuxcmd)
//!   niubash setup            → re-run the interactive prompt/plugin wizard
//!   niu plugin list [--json] → list official Niubash plugins
//!   niu plugin info <name> [--json] → inspect one official plugin
//!   niu plugin search [query] [--json] → discover official plugins
//!   niu plugin themes [--json] → list user and bundle themes
//!   niu plugin bundle status [--json] → inspect official bundle install state
//!   niu plugin doctor [--json] → diagnose plugin configuration health
//!   niu plugin review <name> [--json] → review plugin permissions
//!   niu plugin update oh-my-winuxsh --from <path> → install a bundle release
//!   niu plugin update oh-my-winuxsh --github-release latest → download/install bundle
//!   niu plugin rollback oh-my-winuxsh → roll back to the previous bundle
//!   niubash --completion-probe "line" [cursor] → print REPL completions
//!   niubash --install-wt-profile → add/update the Windows Terminal profile
//!   niubash --self-update → download and run the latest installer
//!   self-update / update-niubash → REPL commands for Niubash self-update

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use rubash::invocation::ShellInvocation;

mod self_update;
const OFFICIAL_PLUGIN_BUNDLE_REPO: &str = "unixwin/oh-my-winuxsh";
const PLUGIN_BUNDLE_DOWNLOAD_CACHE: &str = "niubash-plugin-bundles";
const NIU_MAIN_STACK_SIZE: usize = 32 * 1024 * 1024;

fn main() -> ExitCode {
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
    if first.starts_with('-')
        && !matches!(
            first.as_str(),
            "-h" | "--help" | "-V" | "--version" | "-C" | "--repl-command"
        )
        && ShellInvocation::parse(&args[1..]).is_ok()
    {
        return run_shell_invocation(&args[1..]);
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
        "setup" | "configure" => niubash_runtime::setup_wizard::rerun_wizard(),
        "plugin" => run_plugin_command(args),
        "-C" | "--repl-command" => run_repl_command(args),
        "-c" => {
            if args.len() < 3 {
                anyhow::bail!("-c requires an argument");
            }
            let mut shell = niubash_runtime::Shell::new()?;
            shell.executor.inherit_process_stdin();
            shell.enable_process_stdin_pipeline_bridge();
            shell.executor.set_env("BASH_EXECUTION_STRING", &args[2]);
            if let Some(command_name) = args.get(3) {
                shell.executor.set_env("__RUBASH_SCRIPT_NAME", command_name);
                shell.executor.set_positional_params(args[4..].to_vec());
            }
            let code = shell.execute_script(&args[2])?;
            let code = shell.finish_with_exit_trap(code)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        _ => {
            // Treat as a script file to execute
            let script = script_arg_to_host_path(first);
            if !script.exists() {
                anyhow::bail!("unknown argument '{}' (not a script file)", first);
            }
            let mut shell = niubash_runtime::Shell::new()?;
            shell.executor.set_env("__RUBASH_SCRIPT_NAME", first);
            shell.executor.inherit_process_stdin();
            shell.enable_process_stdin_pipeline_bridge();
            shell.executor.set_positional_params(args[2..].to_vec());
            let content = std::fs::read_to_string(&script)?;
            let code = shell.execute_script(&content)?;
            let code = shell.finish_with_exit_trap(code)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
    }
}

fn run_shell_invocation(args: &[String]) -> anyhow::Result<()> {
    let invocation =
        ShellInvocation::parse(args).map_err(|error| anyhow::anyhow!("niu: {}", error))?;
    let mut shell = if invocation.read_stdin {
        niubash_runtime::Shell::new_for_stdin_script()?
    } else {
        niubash_runtime::Shell::new()?
    };
    invocation
        .apply_to_executor(&mut shell.executor)
        .map_err(|error| anyhow::anyhow!("niu: {}", error))?;
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();

    if let Some(command) = invocation.command {
        shell.executor.set_env("BASH_EXECUTION_STRING", &command);
        let code = shell.execute_script(&command)?;
        let code = shell.finish_with_exit_trap(code)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if let Some(script_name) = invocation.script {
        shell.executor.set_env("__RUBASH_SCRIPT_NAME", &script_name);
        let content = std::fs::read_to_string(script_arg_to_host_path(&script_name))?;
        let code = shell.execute_script(&content)?;
        let code = shell.finish_with_exit_trap(code)?;
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    let mut content = String::new();
    std::io::stdin().read_to_string(&mut content)?;
    let code = shell.execute_script(&content)?;
    let code = shell.finish_with_exit_trap(code)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
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
    niubash_runtime::repl::run_repl(&mut shell)
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
    shell.executor.inherit_process_stdin();
    shell.enable_process_stdin_pipeline_bridge();
    if let Some(command_name) = args.get(3) {
        shell.executor.set_env("__RUBASH_SCRIPT_NAME", command_name);
        shell.executor.set_positional_params(args[4..].to_vec());
    }
    shell.run_startup_rc();
    shell.run_precmd_hooks();
    let code = shell.execute_interactive_line(&args[2])?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn run_stdin_script() -> anyhow::Result<()> {
    let mut shell = niubash_runtime::Shell::new_for_stdin_script()?;
    shell.executor.inherit_process_stdin();
    let mut line = String::new();
    let mut pending = Vec::new();

    loop {
        line.clear();
        match read_unbuffered_line(&mut line)? {
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

        let code = shell.execute_script(&script)?;
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

fn print_usage() {
    println!(
        "Niubash {} \u{2014} a bash-compatible shell that feels at home on Windows.",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage:  niubash [option]");
    println!("        niu -c <cmd>         Run a command then exit");
    println!("        niubash -C <cmd>         Run one REPL-style command then exit");
    println!("        niubash setup           Re-run prompt/plugin setup");
    println!("        niubash <script> [args]   Run a script file");
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
    println!("  plugin list [--json]      List official Niubash plugins");
    println!("  plugin info <name> [--json]  Inspect one official Niubash plugin");
    println!("  plugin search [query] [--json]  Discover official plugins");
    println!("  plugin themes [--json]    List user and bundle themes");
    println!("  plugin bundle status [--json]  Inspect official bundle install state");
    println!("  plugin update oh-my-winuxsh --from <path>");
    println!("      [--checksum <sha>|--checksum-file <path>] [--json]");
    println!("  plugin update oh-my-winuxsh --github-release latest|vX.Y.Z [--json]");
    println!("                            Install bundle release");
    println!("  plugin rollback oh-my-winuxsh [--json]  Roll back bundle release");
    println!();
    println!();
    println!();
    println!("  --completion-probe <line> [cursor]  Debug: print completion candidates");
    println!();
    println!("Configuration: ~/.niubashrc for interactive startup; ~/.winshrc remains a compatibility fallback");
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
            let json = parse_plugin_json_flag(&args[3..])?;
            if json {
                println!("{}", niubash_runtime::plugins::plugin_packs_json()?);
            } else {
                println!("{}", niubash_runtime::plugins::plugin_packs_text());
            }
            Ok(())
        }
        "search" => run_plugin_search_command(&args[3..]),
        "themes" => run_plugin_themes_command(&args[3..]),
        "info" => {
            let Some(name) = args.get(3) else {
                anyhow::bail!("plugin info requires a plugin name");
            };
            let json = parse_plugin_json_flag(&args[4..])?;
            if json {
                match niubash_runtime::plugins::plugin_pack_json(name)? {
                    Some(output) => println!("{}", output),
                    None => anyhow::bail!("unknown plugin '{}'", name),
                }
            } else {
                match niubash_runtime::plugins::plugin_pack_text(name) {
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
        unknown => anyhow::bail!("unknown plugin subcommand '{}'", unknown),
    }
}

fn run_plugin_doctor_command(args: &[String]) -> anyhow::Result<()> {
    let json = parse_plugin_json_flag(args)?;
    let config = niubash_runtime::config::load();
    let report = niubash_runtime::plugins::plugin_doctor_report(&config.plugins);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", niubash_runtime::plugins::plugin_doctor_text(&report));
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
    let (query, json) = parse_plugin_search_args(args)?;
    if json {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_search_json(query.as_deref())?
        );
    } else {
        println!(
            "{}",
            niubash_runtime::plugins::plugin_search_text(query.as_deref())
        );
    }
    Ok(())
}

fn run_plugin_themes_command(args: &[String]) -> anyhow::Result<()> {
    let json = parse_plugin_json_flag(args)?;
    if json {
        println!("{}", niubash_runtime::plugins::plugin_theme_catalog_json()?);
    } else {
        println!("{}", niubash_runtime::plugins::plugin_theme_catalog_text());
    }
    Ok(())
}

fn run_plugin_bundle_command(args: &[String]) -> anyhow::Result<()> {
    let Some(subcommand) = args.get(0) else {
        anyhow::bail!("plugin bundle requires a subcommand: status");
    };

    match subcommand.as_str() {
        "status" => {
            let json = parse_plugin_json_flag(&args[1..])?;
            if json {
                println!("{}", niubash_runtime::plugins::plugin_bundle_status_json()?);
            } else {
                println!("{}", niubash_runtime::plugins::plugin_bundle_status_text());
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
        println!("Updated bundle '{}' to {}", summary.bundle, summary.version);
        println!("Installed path: {}", summary.installed_path.display());
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
            "Rolled back bundle '{}' to {}",
            summary.bundle, summary.version
        );
        println!("Active path: {}", summary.active_path.display());
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

fn parse_plugin_search_args(args: &[String]) -> anyhow::Result<(Option<String>, bool)> {
    let mut query = None;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
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
    Ok((query, json))
}

fn print_plugin_usage() {
    println!("Usage:  niu plugin <command>");
    println!();
    println!("Commands:");
    println!("  list [--json]             List official Niubash plugins");
    println!("  info <name> [--json]      Inspect one official Niubash plugin");
    println!("  search [query] [--json]   Discover official plugins");
    println!("  themes [--json]           List user and bundle themes");
    println!("  bundle status [--json]    Inspect official bundle install state");
    println!("  doctor [--json]           Diagnose plugin configuration health");
    println!("  review <name> [--json]    Review plugin permissions before enabling");
    println!("  update oh-my-winuxsh --from <path>");
    println!("      [--checksum <sha>|--checksum-file <path>] [--json]");
    println!("                            Install a local bundle directory or zip");
    println!("  update oh-my-winuxsh --github-release latest|vX.Y.Z [--json]");
    println!("                            Download, verify, and install GitHub release");
    println!("  rollback oh-my-winuxsh [--json]");
    println!("                            Roll back to the previous bundle");
    println!("  install <name>           Install official plugin from active bundle");
    println!("  uninstall <name>         Uninstall official plugin from active bundle");
}

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
    println!(
        "Niubash {} \u{2014} bash-compatible shell for Windows",
        env!("CARGO_PKG_VERSION")
    );
    println!("  rubash   git {}", rubash_revision());
    if let Some(v) = niubash_runtime::winuxcmd::version() {
        println!("  winuxcmd {}", v);
    }
}

fn rubash_revision() -> &'static str {
    option_env!("NIU_RUBASH_REV").unwrap_or("master")
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
}
