//! winuxsh entry point
//!
//! Usage:
//!   winuxsh                  → interactive REPL
//!   winuxsh -c "command"     → execute one command, print exit code, exit
//!   winuxsh script.sh        → execute a script file
//!   winuxsh --help | -h      → usage
//!   winuxsh --version        → version (winuxsh / rubash / winuxcmd)
//!   winuxsh --zsh-compat-report      → scan zsh config and print report
//!   winuxsh --zsh-compat-report-json → scan zsh config and print JSON report
//!   winuxsh --zsh-compat-import-plan → print a reviewable .winshrc.toml patch
//!   winuxsh --zsh-compat-import-apply → write the import patch with a backup
//!   winuxsh --zsh-compat-import-status → inspect import block and backups
//!   winuxsh --zsh-compat-import-rollback-plan → print restore command
//!   winuxsh --zsh-compat-doctor → summarize zsh compatibility health
//!   winuxsh --zsh-native-packs → list built-in native zsh plugin packs
//!   winuxsh --zsh-native-packs-json → list built-in native zsh plugin packs as JSON
//!   winuxsh --zsh-profile-plan <profile> → print a native zsh profile TOML plan
//!   winuxsh --completion-probe "line" [cursor] → print REPL completions

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // Initialize logging (only error level by default)
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Error)
        .parse_env("RUST_LOG")
        .init();

    // Install Ctrl+C handler (best-effort)
    winuxsh_runtime::ctrl_c::install();

    let args: Vec<String> = std::env::args().collect();

    if let Err(e) = run(&args) {
        if is_broken_pipe_error(&e) {
            return ExitCode::from(1);
        }
        eprintln!("winuxsh: {}", e);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run(args: &[String]) -> anyhow::Result<()> {
    if args.len() < 2 {
        return if winuxsh_runtime::terminal::stdio_is_interactive() {
            run_repl()
        } else {
            run_stdin_script()
        };
    }

    let first = &args[1];
    match first.as_str() {
        "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        "--version" | "-V" => {
            print_version();
            Ok(())
        }
        "--zsh-compat-report" => {
            print_zsh_compat_report(false)?;
            Ok(())
        }
        "--zsh-compat-report-json" => {
            print_zsh_compat_report(true)?;
            Ok(())
        }
        "--zsh-compat-import-plan" => {
            print_zsh_compat_import_plan()?;
            Ok(())
        }
        "--zsh-compat-import-apply" => {
            apply_zsh_compat_import_plan()?;
            Ok(())
        }
        "--zsh-compat-import-status" => {
            print_zsh_compat_import_status()?;
            Ok(())
        }
        "--zsh-compat-import-rollback-plan" => {
            print_zsh_compat_import_rollback_plan()?;
            Ok(())
        }
        "--zsh-compat-doctor" => {
            print_zsh_compat_doctor()?;
            Ok(())
        }
        "--zsh-native-packs" => {
            print_zsh_native_packs(false)?;
            Ok(())
        }
        "--zsh-native-packs-json" => {
            print_zsh_native_packs(true)?;
            Ok(())
        }
        "--zsh-profile-plan" => {
            print_zsh_profile_plan(args)?;
            Ok(())
        }
        "--completion-probe" => {
            print_completion_probe(args)?;
            Ok(())
        }
        "-c" => {
            if args.len() < 3 {
                anyhow::bail!("-c requires an argument");
            }
            let mut shell = winuxsh_runtime::Shell::new()?;
            shell.executor.inherit_process_stdin();
            if let Some(command_name) = args.get(3) {
                shell.executor.set_env("__RUBASH_SCRIPT_NAME", command_name);
                shell.executor.set_positional_params(args[4..].to_vec());
            }
            let code = shell.execute_script(&args[2])?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        _ => {
            // Treat as a script file to execute
            let script = PathBuf::from(first);
            if !script.exists() {
                anyhow::bail!("unknown argument '{}' (not a script file)", first);
            }
            let mut shell = winuxsh_runtime::Shell::new()?;
            shell.executor.set_env("__RUBASH_SCRIPT_NAME", first);
            shell.executor.inherit_process_stdin();
            shell.executor.set_positional_params(args[2..].to_vec());
            let content = std::fs::read_to_string(&script)?;
            let code = shell.execute_script(&content)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
    }
}

fn run_repl() -> anyhow::Result<()> {
    let mut shell = winuxsh_runtime::Shell::new()?;
    winuxsh_runtime::repl::run_repl(&mut shell)
}

fn run_stdin_script() -> anyhow::Result<()> {
    let mut script = String::new();
    std::io::stdin().read_to_string(&mut script)?;
    if script.trim().is_empty() {
        return Ok(());
    }

    let mut shell = winuxsh_runtime::Shell::new()?;
    let code = shell.execute_script(&script)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn print_usage() {
    println!(
        "Winuxsh {} \u{2014} a bash-compatible shell that feels at home on Windows.",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage:  winuxsh [option]");
    println!("        winuxsh -c <cmd>         Run a command then exit");
    println!("        winuxsh <script> [args]   Run a script file");
    println!();
    println!("Options:");
    println!("  -h, --help                Show this help");
    println!("  -V, --version             Version and component info");
    println!("  -c <command>              Execute a command ad-hoc");
    println!();
    println!("  --zsh-compat-report       Scan ~/.zshrc, show safe-import report");
    println!("  --zsh-compat-report-json  Same, as JSON");
    println!("  --zsh-compat-import-plan  Generate a .winshrc.toml import patch");
    println!("  --zsh-compat-import-apply Apply the patch (with backup)");
    println!("  --zsh-compat-import-status Inspect import block and backup");
    println!("  --zsh-compat-import-rollback-plan  Show restore command");
    println!("  --zsh-compat-doctor       Overall zsh health summary");
    println!();
    println!("  --zsh-native-packs        List built-in zsh plugin replacements");
    println!("  --zsh-native-packs-json   Same, as JSON");
    println!("  --zsh-profile-plan <profile>  Print TOML for a profile");
    println!();
    println!("  --completion-probe <line> [cursor]  Debug: print completion candidates");
    println!();
    println!("Configuration: ~/.winshrc.toml for settings, ~/.winshrc for REPL shell code");
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
    let shell = winuxsh_runtime::Shell::new()?;
    for suggestion in shell.completion_probe(line, cursor_pos) {
        println!("{}", suggestion);
    }
    Ok(())
}

fn print_zsh_compat_import_plan() -> anyhow::Result<()> {
    let config = winuxsh_runtime::config::load();
    let options = winuxsh_runtime::zsh_compat::ZshImportOptions::for_report(&config.zsh);
    let report = winuxsh_runtime::zsh_compat::scan(&options);
    println!(
        "{}",
        winuxsh_runtime::zsh_compat::import_plan_toml(&options, &report)
    );
    Ok(())
}

fn apply_zsh_compat_import_plan() -> anyhow::Result<()> {
    let config = winuxsh_runtime::config::load();
    let options = winuxsh_runtime::zsh_compat::ZshImportOptions::for_report(&config.zsh);
    let report = winuxsh_runtime::zsh_compat::scan(&options);
    let plan = winuxsh_runtime::zsh_compat::import_plan_toml(&options, &report);
    let config_path = winuxsh_runtime::config::default_config_path();
    let summary = winuxsh_runtime::zsh_compat::apply_import_plan_to_config(&config_path, &plan)?;

    println!(
        "Wrote zsh compatibility import block to {}",
        summary.config_path.display()
    );
    if summary.replaced_existing_block {
        println!("Replaced the previous winuxsh-managed zsh import block");
    } else {
        println!("Added a new winuxsh-managed zsh import block");
    }
    if let Some(backup_path) = summary.backup_path {
        println!("Backup: {}", backup_path.display());
    }
    Ok(())
}

fn print_zsh_compat_import_status() -> anyhow::Result<()> {
    let config = winuxsh_runtime::config::load();
    let options = winuxsh_runtime::zsh_compat::ZshImportOptions::for_report(&config.zsh);
    let report = winuxsh_runtime::zsh_compat::scan(&options);
    let plan = winuxsh_runtime::zsh_compat::import_plan_toml(&options, &report);
    let config_path = winuxsh_runtime::config::default_config_path();
    let status = winuxsh_runtime::zsh_compat::inspect_import_config_status(&config_path, &plan)?;

    println!("Config: {}", status.config_path.display());
    println!("Exists: {}", yes_no(status.config_exists));
    println!(
        "Managed block: {}",
        zsh_import_block_state_label(status.block_state)
    );
    if status.toml_valid {
        println!("TOML: valid");
    } else {
        println!(
            "TOML: invalid ({})",
            status.toml_error.as_deref().unwrap_or("unknown error")
        );
    }
    println!(
        "Next apply: {}",
        zsh_import_apply_readiness_label(status.apply_readiness)
    );
    if let Some(error) = status.apply_error {
        println!("Apply detail: {}", error);
    }
    println!("Backups: {}", status.backup_paths.len());
    if let Some(path) = status.backup_paths.last() {
        println!("Latest backup: {}", path.display());
    }
    Ok(())
}

fn print_zsh_compat_import_rollback_plan() -> anyhow::Result<()> {
    let config_path = winuxsh_runtime::config::default_config_path();
    let plan = winuxsh_runtime::zsh_compat::inspect_import_rollback_plan(&config_path)?;

    println!("Config: {}", plan.config_path.display());
    println!("Backups: {}", plan.backup_paths.len());
    if let Some(path) = plan.latest_backup_path {
        println!("Latest backup: {}", path.display());
    } else {
        println!("Latest backup: none");
    }
    if let Some(command) = plan.restore_command {
        println!("Restore command:");
        println!("{}", command);
    } else {
        println!("Restore command: unavailable (no backups found)");
    }
    Ok(())
}

fn print_zsh_compat_doctor() -> anyhow::Result<()> {
    let config = winuxsh_runtime::config::load();
    let options = winuxsh_runtime::zsh_compat::ZshImportOptions::for_report(&config.zsh);
    let report = winuxsh_runtime::zsh_compat::scan(&options);
    let plan = winuxsh_runtime::zsh_compat::import_plan_toml(&options, &report);
    let config_path = winuxsh_runtime::config::default_config_path();
    let status = winuxsh_runtime::zsh_compat::inspect_import_config_status(&config_path, &plan)?;
    let rollback = winuxsh_runtime::zsh_compat::inspect_import_rollback_plan(&config_path)?;

    println!(
        "{}",
        winuxsh_runtime::zsh_compat::zsh_compat_doctor_text(&report, &status, &rollback)
    );
    Ok(())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn zsh_import_block_state_label(
    state: winuxsh_runtime::zsh_compat::ZshImportBlockState,
) -> &'static str {
    match state {
        winuxsh_runtime::zsh_compat::ZshImportBlockState::Missing => "missing",
        winuxsh_runtime::zsh_compat::ZshImportBlockState::Present => "present",
        winuxsh_runtime::zsh_compat::ZshImportBlockState::Malformed => "malformed",
    }
}

fn zsh_import_apply_readiness_label(
    readiness: winuxsh_runtime::zsh_compat::ZshImportApplyReadiness,
) -> &'static str {
    match readiness {
        winuxsh_runtime::zsh_compat::ZshImportApplyReadiness::AddNewBlock => "add new block",
        winuxsh_runtime::zsh_compat::ZshImportApplyReadiness::ReplaceExistingBlock => {
            "replace existing block"
        }
        winuxsh_runtime::zsh_compat::ZshImportApplyReadiness::Blocked => "blocked",
    }
}

fn print_zsh_compat_report(json: bool) -> anyhow::Result<()> {
    let config = winuxsh_runtime::config::load();
    let options = winuxsh_runtime::zsh_compat::ZshImportOptions::for_report(&config.zsh);
    let report = winuxsh_runtime::zsh_compat::scan(&options);
    if json {
        println!("{}", report.to_json_pretty()?);
    } else {
        println!("{}", report.to_human());
    }
    Ok(())
}

fn print_zsh_native_packs(json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", winuxsh_runtime::zsh_compat::native_zsh_packs_json()?);
    } else {
        println!("{}", winuxsh_runtime::zsh_compat::native_zsh_packs_text());
    }
    Ok(())
}

fn print_zsh_profile_plan(args: &[String]) -> anyhow::Result<()> {
    let Some(profile) = args.get(2) else {
        anyhow::bail!("--zsh-profile-plan requires a profile: agent or zsh-lite");
    };
    println!(
        "{}",
        winuxsh_runtime::zsh_compat::zsh_profile_plan_toml_for_name(profile)?
    );
    Ok(())
}

fn print_version() {
    println!(
        "Winuxsh {} \u{2014} bash-compatible shell for Windows",
        env!("CARGO_PKG_VERSION")
    );
    println!("  rubash   git {}", rubash_revision());
    if let Some(v) = winuxsh_runtime::winuxcmd::version() {
        println!("  winuxcmd {}", v);
    }
}

fn rubash_revision() -> &'static str {
    "df626e2905ce1337849db6aff5405f14db168a9e"
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
