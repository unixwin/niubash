//! winuxfetch: small Neofetch-style system summary for Winuxsh users.
//!
//! The Windows ASCII logo is adapted from Neofetch.
//! See THIRD_PARTY_NOTICES.md for the Neofetch MIT license notice.

use std::collections::HashMap;
use std::env;
use std::process::Command;

const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36;1m";
const GREEN: &str = "\x1b[32;1m";
const BOLD: &str = "\x1b[1m";

const WINDOWS_LOGO: &[&str] = &[
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
    "################  ################",
];

#[derive(Debug, Default)]
struct FetchOptions {
    no_logo: bool,
    no_color: bool,
    help: bool,
    license: bool,
}

#[derive(Debug, Default)]
struct SystemInfo {
    os: Option<String>,
    kernel: Option<String>,
    host: Option<String>,
    uptime: Option<String>,
    cpu: Option<String>,
    memory: Option<String>,
}

fn main() {
    let options = parse_args(env::args().skip(1));

    if options.help {
        print_help();
        return;
    }
    if options.license {
        print_license_notice();
        return;
    }

    let no_color = options.no_color || env::var_os("NO_COLOR").is_some();
    let info = collect_system_info();
    let lines = info_lines(&info, no_color);

    if options.no_logo {
        for line in lines {
            println!("{line}");
        }
        return;
    }

    print_with_logo(&lines, no_color);
}

fn parse_args(args: impl Iterator<Item = String>) -> FetchOptions {
    let mut options = FetchOptions::default();
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => options.help = true,
            "--license" => options.license = true,
            "--no-logo" | "--off" => options.no_logo = true,
            "--no-color" | "--plain" => options.no_color = true,
            _ => {}
        }
    }
    options
}

fn print_help() {
    println!(
        "winuxfetch {} - Neofetch-style system info for Winuxsh",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Usage: winuxfetch [--no-logo] [--no-color] [--license]");
    println!();
    println!("Options:");
    println!("  --no-logo, --off   Print info without ASCII art");
    println!("  --no-color, --plain Disable ANSI colors");
    println!("  --license          Show third-party attribution");
    println!("  -h, --help         Show this help");
}

fn print_license_notice() {
    println!("winuxfetch includes Windows ASCII art adapted from Neofetch.");
    println!("Neofetch is MIT licensed:");
    println!("Copyright (c) 2015-2021 Dylan Araps");
    println!("See THIRD_PARTY_NOTICES.md in the Winuxsh source distribution.");
}

fn collect_system_info() -> SystemInfo {
    let mut info = powershell_system_info().unwrap_or_default();

    if info.os.is_none() {
        info.os = Some(default_os_label());
    }
    if info.kernel.is_none() {
        info.kernel = Some(env::consts::ARCH.to_string());
    }
    if info.host.is_none() {
        info.host = env::var("COMPUTERNAME")
            .ok()
            .or_else(|| env::var("HOSTNAME").ok());
    }
    if info.cpu.is_none() {
        info.cpu = Some(env::consts::ARCH.to_string());
    }

    info
}

fn powershell_system_info() -> Option<SystemInfo> {
    if !cfg!(windows) {
        return None;
    }

    let script = r#"
$os = Get-CimInstance Win32_OperatingSystem
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$cs = Get-CimInstance Win32_ComputerSystem
$uptime = (Get-Date) - $os.LastBootUpTime
$total = [int64]$os.TotalVisibleMemorySize
$free = [int64]$os.FreePhysicalMemory
$used = $total - $free
Write-Output ("OS=" + $os.Caption)
Write-Output ("KERNEL=" + $os.Version)
Write-Output ("HOST=" + (($cs.Manufacturer, $cs.Model | Where-Object { $_ }) -join " "))
Write-Output ("UPTIME={0}d {1}h {2}m" -f [int]$uptime.TotalDays, $uptime.Hours, $uptime.Minutes)
Write-Output ("CPU=" + $cpu.Name)
Write-Output ("MEMORY={0} MiB / {1} MiB" -f [int]($used / 1024), [int]($total / 1024))
"#;

    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pairs = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_uppercase(), value.trim().to_string()))
        .filter(|(_, value)| !value.is_empty())
        .collect::<HashMap<_, _>>();

    Some(SystemInfo {
        os: pairs.get("OS").cloned(),
        kernel: pairs.get("KERNEL").cloned(),
        host: pairs.get("HOST").cloned(),
        uptime: pairs.get("UPTIME").cloned(),
        cpu: pairs.get("CPU").cloned(),
        memory: pairs.get("MEMORY").cloned(),
    })
}

fn info_lines(info: &SystemInfo, no_color: bool) -> Vec<String> {
    let title = format!(
        "{}@{}",
        env::var("USERNAME")
            .or_else(|_| env::var("USER"))
            .unwrap_or_else(|_| "user".to_string()),
        env::var("COMPUTERNAME")
            .or_else(|_| env::var("HOSTNAME"))
            .unwrap_or_else(|_| "host".to_string())
    );
    let underline = "-".repeat(title.chars().count());
    let shell = format!("winuxsh {}", env!("CARGO_PKG_VERSION"));
    let terminal = terminal_name();

    let mut lines = Vec::new();
    lines.push(accent(&title, no_color));
    lines.push(underline);
    push_info(&mut lines, "OS", info.os.as_deref());
    push_info(&mut lines, "Host", info.host.as_deref());
    push_info(&mut lines, "Kernel", info.kernel.as_deref());
    push_info(&mut lines, "Uptime", info.uptime.as_deref());
    push_info(&mut lines, "Shell", Some(&shell));
    push_info(&mut lines, "Terminal", Some(&terminal));
    push_info(&mut lines, "CPU", info.cpu.as_deref());
    push_info(&mut lines, "Memory", info.memory.as_deref());
    lines
}

fn push_info(lines: &mut Vec<String>, label: &str, value: Option<&str>) {
    if let Some(value) = value {
        if !value.trim().is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
}

fn print_with_logo(lines: &[String], no_color: bool) {
    let logo_width = WINDOWS_LOGO
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let rows = WINDOWS_LOGO.len().max(lines.len());

    for index in 0..rows {
        let logo = WINDOWS_LOGO.get(index).copied().unwrap_or("");
        let info = lines.get(index).map(String::as_str).unwrap_or("");
        if no_color {
            println!("{:<width$}  {}", logo, info, width = logo_width);
        } else {
            println!(
                "{CYAN}{:<width$}{RESET}  {}",
                logo,
                info,
                width = logo_width
            );
        }
    }
}

fn terminal_name() -> String {
    if env::var_os("WT_SESSION").is_some() {
        return "Windows Terminal".to_string();
    }
    if let Ok(value) = env::var("TERM_PROGRAM") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    if let Ok(value) = env::var("TERM") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    env::var("COMSPEC").unwrap_or_else(|_| "terminal".to_string())
}

fn default_os_label() -> String {
    if cfg!(windows) {
        "Windows".to_string()
    } else {
        format!("{} {}", env::consts::OS, env::consts::ARCH)
    }
}

fn accent(value: &str, no_color: bool) -> String {
    if no_color {
        value.to_string()
    } else {
        format!("{BOLD}{GREEN}{value}{RESET}")
    }
}
