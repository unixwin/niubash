//! `niu doctor`: one-command health check for a Niubash installation.
//!
//! Reuses the same probes the setup wizard trusts (winuxcmd discovery,
//! command links, nerd font) and prints a compact report with fix hints.
//! Critical checks decide the trailing count; advisory rows (font, shims,
//! language) never fail the run.

use std::io::{self, Write};
use std::path::PathBuf;

const OK: &str = "\x1b[1;92m✅\x1b[0m";
const WARN: &str = "\x1b[1;93m⚠️\x1b[0m";
const INFO: &str = "\x1b[1;96mℹ️\x1b[0m";

/// Run all checks and print the report. Colors are dropped when stdout is
/// not a terminal so piped output stays plain.
///
/// `skill_files` is the launcher-embedded agent skill bundle (empty in
/// builds that embed nothing): the doctor reports install state per named
/// agent skill dir (niubash#188), advisory only — a missing skill install
/// never fails the run.
pub fn run_doctor(skill_files: &[crate::skill::SkillFile]) -> anyhow::Result<()> {
    let color = crate::terminal::stdout_is_terminal();
    let (ok, warn, info) = if color {
        (OK, WARN, INFO)
    } else {
        ("OK", "ADVICE", "INFO")
    };
    let mut out = io::stdout();
    let mut critical_passed = 0usize;
    let mut critical_total = 0usize;

    if color {
        writeln!(
            out,
            "\x1b[1mniubash doctor\x1b[0m — {}",
            env!("CARGO_PKG_VERSION")
        )?;
    } else {
        writeln!(out, "niubash doctor — {}", env!("CARGO_PKG_VERSION"))?;
    }
    writeln!(out)?;

    // ── Critical: the Unix command core ────────────────────────────────────
    critical_total += 1;
    match crate::winuxcmd::find_winuxcmd() {
        Some(exe) => {
            let version = crate::winuxcmd::version()
                .map(|v| format!(" (winuxcmd {v})"))
                .unwrap_or_default();
            writeln!(out, "  {ok} winuxcmd core       {}{version}", display(&exe))?;
            critical_passed += 1;

            // The bash/sh forwarder shims ship beside the winuxcmd tree.
            let shim = exe
                .parent()
                .map(|dir| dir.join("bash.exe"))
                .filter(|p| p.is_file());
            match shim {
                Some(_) => writeln!(
                    out,
                    "  {info} bash/sh shims       present — `bash` and `sh` forward to niu"
                )?,
                None => writeln!(
                    out,
                    "  {info} bash/sh shims       not beside winuxcmd (optional)"
                )?,
            }
        }
        None => writeln!(
            out,
            "  {warn} winuxcmd core       not found — reinstall Niubash or check PATH"
        )?,
    }

    // ── Critical: command links on PATH ────────────────────────────────────
    critical_total += 1;
    if crate::winuxcmd::command_links_ready() {
        let count = crate::winuxcmd::list_commands().len();
        writeln!(
            out,
            "  {ok} command links       {count} commands (ls, cat, grep, …)"
        )?;
        critical_passed += 1;
    } else {
        // Windows-only recovery text: the wpm command-layer verb exists only
        // there, and the `wpm` string must not surface in non-Windows
        // builds — compile-time gate, not a runtime check.
        #[cfg(windows)]
        writeln!(
            out,
            "  {warn} command links       missing (ls/cat/grep) — restart niu or run `wpm links rebuild`"
        )?;
        #[cfg(not(windows))]
        writeln!(
            out,
            "  {warn} command links       missing (ls/cat/grep) — restart niu"
        )?;
    }

    // ── Critical: interactive startup rc ───────────────────────────────────
    critical_total += 1;
    let home = crate::path_utils::shell_home_dir().unwrap_or_else(|| PathBuf::from("."));
    let rc = home.join(".niubashrc");
    if rc.is_file() {
        writeln!(out, "  {ok} startup rc          {}", display(&rc))?;
        critical_passed += 1;
    } else if home.join(".winuxshrc").is_file() {
        writeln!(
            out,
            "  {ok} startup rc          {} (legacy, migrates on next start)",
            display(&home.join(".winuxshrc"))
        )?;
        critical_passed += 1;
    } else {
        writeln!(
            out,
            "  {warn} startup rc          none — run `niu setup` to create ~/.niubashrc"
        )?;
    }

    // ── Advisory: PATH-shadowed niu (wt82-L01 V1 class) ──────────────────
    // The rc bootstrap and every bare `niu` typing resolve `niu` through
    // PATH. When PATH finds a *different* niu than the one running right
    // now, startup reconcile runs under that binary's semantics and old
    // vintages print `unknown plugin subcommand` noise on every start.
    match find_niu_on_path() {
        Some(path_niu)
            if !same_executable(&path_niu, &std::env::current_exe().unwrap_or_default()) =>
        {
            writeln!(
                out,
                "  {warn} niu on PATH         {} is NOT this niu ({}) — \
                 bare `niu` and the rc bootstrap run the PATH copy; \
                 update/reinstall it (`niu --self-update` from that copy) or \
                 reorder PATH so the current install wins",
                display(&path_niu),
                std::env::current_exe()
                    .map(|e| display(&e))
                    .unwrap_or_else(|_| "?".to_string()),
            )?;
        }
        _ => {}
    }

    // ── Advisory rows ───────────────────────────────────────────────────────
    if crate::fonts::nerd_font_installed() {
        writeln!(
            out,
            "  {ok} nerd font           detected — icon themes unlocked"
        )?;
    } else {
        writeln!(
            out,
            "  {info} nerd font           not found — icon themes need one: `niu font`"
        )?;
    }

    // ── Advisory: release-bundled tools (niubash#230 follow-up) ───────────
    // The release ships gawk/niugit/ripgrep/fd inside the winuxcmd opt/ tree
    // (preinstall manifest). The doctor reports them with their true source
    // so a bundled component is never mistaken for a system install — and
    // never reported as missing.
    let bundled = crate::setup_wizard::bundled_components();
    writeln!(out, "  {info}{}", bundled_tools_row(&bundled))?;

    let terminal = if std::env::var_os("WT_SESSION").is_some() {
        "Windows Terminal"
    } else {
        "console host"
    };
    writeln!(out, "  {info} terminal            {terminal}")?;

    // ── Advisory: timing scripts (niubash#201) ────────────────────────────
    // `date` is an external program: each `$(date +%s%N)` pays Windows
    // process creation (~10-30ms). The bash 5 builtin `$EPOCHREALTIME`
    // expands in microseconds and drives millisecond-timeline scripts
    // (frame schedulers, benchmarks) three orders of magnitude faster.
    writeln!(
        out,
        "  {info} timing              use $EPOCHREALTIME for timing scripts — \
         `$(date +%s%N)` spawns date.exe (~30ms/call), `$EPOCHREALTIME` is a \
         builtin (~µs, microsecond format like 1699999999.123456)"
    )?;

    // ── Advisory: external plugin sources (iron law 2: fallback explicit) ──
    // Degraded sources must be *visible*: the guarded loaders fell back to
    // the niubash defaults at startup and the user deserves to know.
    let sources = crate::plugins::sources::list_sources();
    if sources.is_empty() {
        writeln!(
            out,
            "  {info} plugin sources      none installed — browse with `niu plugin discover`"
        )?;
    } else {
        let ready = sources.iter().filter(|s| s.state == "ready").count();
        let untrusted = sources.iter().filter(|s| s.state == "untrusted").count();
        let degraded = sources.iter().filter(|s| s.state == "degraded").count();
        writeln!(
            out,
            "  {info} plugin sources      {ready} ready, {untrusted} untrusted, {degraded} degraded \
             (`niu plugin list` for assets)"
        )?;
        for status in sources.iter().filter(|s| s.state == "degraded") {
            writeln!(
                out,
                "  {warn} source degraded     '{}' tree missing — fallback active; \
                 repair with `niu plugin restore {}`",
                status.record.id, status.record.id
            )?;
        }
    }

    // ── Advisory: declarative spec reconciliation (§14.6.3) ──────────────
    match crate::plugins::spec::load_spec() {
        Err(err) => {
            writeln!(out, "  {warn} plugin spec         unreadable: {err}")?;
        }
        Ok(None) => {
            writeln!(
                out,
                "  {info} plugin spec         none — imperative mode \
                 (`niu plugin add <target>` creates ~/.niubash/plugins.toml)"
            )?;
        }
        Ok(Some(spec)) => {
            let registry = crate::plugins::sources::read_source_registry();
            let declared: Vec<String> = spec
                .sources
                .iter()
                .filter_map(crate::plugins::spec::resolve_entry_id)
                .collect();
            let missing: Vec<String> = declared
                .iter()
                .filter(|id| !registry.iter().any(|record| record.id == **id))
                .cloned()
                .collect();
            let missing: Vec<&str> = missing.iter().map(String::as_str).collect();
            let undeclared = registry
                .iter()
                .filter(|record| !declared.contains(&record.id))
                .count();
            let missing_note = if missing.is_empty() {
                String::new()
            } else {
                format!("; missing: {} (run `niu plugin sync`)", missing.join(", "))
            };
            let undeclared_note = if undeclared == 0 {
                String::new()
            } else {
                format!("; {undeclared} installed-but-undeclared (`niu plugin sync` to review)")
            };
            writeln!(
                out,
                "  {info} plugin spec         {} declared source(s){missing_note}{undeclared_note}",
                spec.sources.len()
            )?;
        }
    }

    // ── Advisory: git mirroring (§14.8, China-network comfort) ──
    // Report-only — the product never auto-switches a mirror; the user
    // pastes the URL they trust (`niu plugin mirror set <url>`). niu has
    // no HTTP transport to probe (download retraction 2026-10-04), so the
    // row states the active channel instead of probing GitHub.
    match crate::plugins::mirrors::load_mirror_config() {
        Err(err) => {
            writeln!(
                out,
                "  {warn} git mirror          {} unreadable ({err}) — git fetches go direct",
                crate::plugins::mirrors::mirrors_path().display()
            )?;
        }
        Ok(_) => {
            let mirror = crate::plugins::mirrors::resolve_active_mirror();
            if !mirror.is_none() {
                let channel = mirror
                    .git_instead_of_base
                    .clone()
                    .unwrap_or_else(|| "(no insteadOf base set)".to_string());
                writeln!(
                    out,
                    "  {info} git mirror          custom active ({channel}) — \
                     `niu plugin mirror set none` to disable"
                )?;
            } else {
                writeln!(
                    out,
                    "  {info} git mirror          direct (no mirror configured)"
                )?;
                writeln!(
                    out,
                    "  {info}                     if GitHub fetches stall (e.g. mainland China), \
                     set one you trust: `niu plugin mirror set <url>`"
                )?;
            }
        }
    }

    if crate::setup_wizard::wizard_lang_is_chinese() {
        writeln!(
            out,
            "  {info} language            zh (wizard follows it; override with NIU_LANG)"
        )?;
    }

    // ── Advisory: agent skill bundle (niubash#188) ────────────────────────
    // AI hosts (Claude/ZCode/Cursor) only know niubash's capability surface
    // if the bundle is installed into their skill dirs. Report per named
    // target; the generic `--target <dir>` installs are visible via
    // `niu skill status --target <dir>`.
    if !skill_files.is_empty() {
        let mut any_installed = false;
        let mut any_outdated = false;
        let mut installed_count = 0usize;
        let mut resolvable = 0usize;
        for (_, skills_root) in crate::skill::NAMED_TARGETS {
            let Some(dir) = crate::path_utils::shell_home_dir()
                .map(|home| home.join(skills_root).join(crate::skill::SKILL_DIR_NAME))
            else {
                continue;
            };
            resolvable += 1;
            let status = crate::skill::check_target(skill_files, &dir);
            if !status.installed {
                continue;
            }
            any_installed = true;
            installed_count += 1;
            if !status.up_to_date {
                any_outdated = true;
            }
        }
        if !any_installed {
            writeln!(
                out,
                "  {info} agent skill         not installed — `niu skill install` \
                 teaches AI hosts this shell's capability surface"
            )?;
        } else if any_outdated {
            writeln!(
                out,
                "  {warn} agent skill         outdated in an installed target — \
                 refresh with `niu skill install` (details: `niu skill status`)"
            )?;
        } else {
            writeln!(
                out,
                "  {ok} agent skill         current at {installed_count}/{resolvable} \
                 named targets (`niu skill status` for paths)"
            )?;
        }
    }

    writeln!(out)?;
    if critical_passed == critical_total {
        writeln!(
            out,
            "  {ok} {critical_passed}/{critical_total} critical checks passed"
        )?;
    } else {
        writeln!(
            out,
            "  {warn} {critical_passed}/{critical_total} critical checks passed — fix the rows above"
        )?;
    }
    out.flush()?;
    Ok(())
}

fn display(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The `bundled tools` advisory row (without the leading two-space indent;
/// the caller prepends the info marker). Honest either way: the names of
/// what the release actually ships, or an explicit "none".
fn bundled_tools_row(bundled: &[&str]) -> String {
    if bundled.is_empty() {
        " bundled tools       none — this install ships no pre-installed packages".to_string()
    } else {
        format!(
            " bundled tools       {} (shipped with the release, not system installs)",
            bundled.join(" ")
        )
    }
}

/// What `command -v niu` resolves to on this machine: the first PATH entry
/// that carries an executable `niu` (`.exe`/`.bat`/`.cmd`/`.com`, plus the
/// extension-less form some POSIX-side PATHs expose). `None` = nothing on
/// PATH answers to `niu`.
fn find_niu_on_path() -> Option<PathBuf> {
    const EXTS: &[&str] = &[".exe", ".bat", ".cmd", ".com", ""];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| {
            EXTS.iter()
                .map(|ext| dir.join(format!("niu{ext}")))
                .find(|candidate| candidate.is_file())
        })
}

/// Loose same-file test for the doctor row: canonicalize when possible
/// (resolves case, `\\?\` prefixes and symlinks), else fall back to a
/// case-insensitive textual compare (Windows paths are case-insensitive).
fn same_executable(a: &PathBuf, b: &PathBuf) -> bool {
    let canon = |p: &PathBuf| p.canonicalize().unwrap_or_else(|_| p.clone());
    let (a, b) = (canon(a), canon(b));
    a.to_string_lossy()
        .eq_ignore_ascii_case(&b.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// niubash#230 follow-up: the bundled-tools row names exactly what the
    /// release ships and labels it as bundled, never as a system install.
    #[test]
    fn bundled_tools_row_labels_the_release_source() {
        let row = bundled_tools_row(&["gawk", "niugit", "ripgrep", "fd"]);
        assert!(row.contains("gawk niugit ripgrep fd"), "{row}");
        assert!(row.contains("shipped with the release"), "{row}");
        assert!(row.contains("not system installs"), "{row}");
    }

    #[test]
    fn bundled_tools_row_is_honest_when_empty() {
        let row = bundled_tools_row(&[]);
        assert!(row.contains("none"), "{row}");
        assert!(!row.contains("shipped"), "{row}");
    }
}
