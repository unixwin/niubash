//! `niu doctor`: one-command health check for a Niubash installation.
//!
//! Reuses the same probes the setup wizard uses (winuxcmd discovery,
//! command links, nerd font) and prints a compact report with fix hints.
//! Critical checks decide the trailing count; advisory rows (font, shims,
//! language) never fail the run.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

const OK: &str = "\x1b[1;92m✅\x1b[0m";
const WARN: &str = "\x1b[1;93m⚠️\x1b[0m";
const INFO: &str = "\x1b[1;96mℹ️\x1b[0m";

/// Text of the n/a row for a Windows-only component skipped on another
/// platform (niubash#194). A shared constant so the tests assert the very
/// string the report prints.
const WINDOWS_COMPONENT_NA: &str = "n/a on this platform — Windows component, skipped";

/// Which platform family the doctor is running on (niubash#194).
///
/// The Windows command layer (winuxcmd core, command links, bash/sh shims)
/// does not exist on Linux/macOS; probing it there always reports `not
/// found` and drags the critical tally down falsely. `current()` resolves
/// at compile time via `cfg!(windows)`, but the checks branch on the enum
/// value so the skip logic is unit-testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    Windows,
    Unix,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }

    /// Label shown in the doctor header so the report names its platform.
    fn label(self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::Unix => "unix",
        }
    }

    /// Whether the Windows-only command layer (winuxcmd core, command
    /// links, bash/sh shims) applies on this platform and must be probed.
    fn checks_windows_command_layer(self) -> bool {
        matches!(self, Platform::Windows)
    }
}

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
    let platform = Platform::current();

    if color {
        writeln!(
            out,
            "\x1b[1mniubash doctor\x1b[0m — {} ({})",
            env!("CARGO_PKG_VERSION"),
            platform.label()
        )?;
    } else {
        writeln!(
            out,
            "niubash doctor — {} ({})",
            env!("CARGO_PKG_VERSION"),
            platform.label()
        )?;
    }
    writeln!(out)?;

    // ── Critical: the Unix command core (Windows-only layer, #194) ────────
    // winuxcmd is the native-Windows Unix command layer; on Linux/macOS it
    // can never be found, so the row is reported as n/a and stays out of
    // the critical tally instead of failing the run.
    if platform.checks_windows_command_layer() {
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
    } else {
        writeln!(out, "  {info} winuxcmd core       {WINDOWS_COMPONENT_NA}")?;
    }

    // ── Critical: command links on PATH (Windows-only layer, #194) ────────
    if platform.checks_windows_command_layer() {
        critical_total += 1;
        if crate::winuxcmd::command_links_ready() {
            let count = crate::winuxcmd::list_commands().len();
            writeln!(
                out,
                "  {ok} command links       {count} commands (ls, cat, grep, …)"
            )?;
            critical_passed += 1;
        } else {
            // Windows-only recovery text: the wpm command-layer verb exists
            // only there, and the `wpm` string must not surface in
            // non-Windows builds — compile-time gate, not a runtime check.
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
    } else {
        writeln!(out, "  {info} command links       {WINDOWS_COMPONENT_NA}")?;
    }

    // ── Critical: interactive startup rc ───────────────────────────────────
    critical_total += 1;
    let home = crate::path_utils::shell_home_dir().unwrap_or_else(|| PathBuf::from("."));
    let rc = home.join(".niubashrc");
    if rc.is_file() {
        writeln!(out, "  {ok} startup rc          {}", display(&rc))?;
        critical_passed += 1;
    } else {
        writeln!(
            out,
            "  {warn} startup rc          none — run `niu setup` to create ~/.niubashrc"
        )?;
    }

    // ── Advisory: plugin loader / legacy managed blocks (niubash#359) ─────
    // Read-only: the generated loader is the activation channel and the rc
    // is never touched, so a legacy managed block in the rc (the old
    // write-era state) is reported with its exact migration verb — the
    // removal stays the user's own edit, forever.
    let loader_text = crate::plugins::loader::read_loader_text();
    let rc_text_read = std::fs::read_to_string(&rc).ok();
    let rc_legacy_blocks: Option<Vec<_>> = rc_text_read
        .as_deref()
        .and_then(|text| crate::plugins::loader::scan_legacy_blocks(text).ok())
        .filter(|blocks| !blocks.is_empty());
    // CR F2/F2-prime: the doctor reports the guard's OWN evidence, and the
    // "stays OFF" claim is keyed on the guard's verdict (guard_trips), not
    // on the block scan — a held-off loader is never diagnosed as clean,
    // and an inert (comment-only) legacy pair is never claimed to hold the
    // loader off.
    let guard_trips: Vec<(usize, String)> = rc_text_read
        .as_deref()
        .map(crate::plugins::loader::guard_trip_lines)
        .unwrap_or_default();
    let held_off = !guard_trips.is_empty();
    let legacy_seen = rc_legacy_blocks.is_some() || held_off;
    // niubash#586: the doctor judges the loader by the SAME validate()
    // the startup path gates on — a truncated or foreign loader is a
    // warn (it will not be sourced), never an ok, and a file that exists
    // but cannot be read as text is not reported as "none".
    let loader_invalid: Option<String> = loader_text
        .as_deref()
        .and_then(|text| crate::plugins::loader::validate(text).err())
        .map(|err| err.to_string());
    let loader_unreadable =
        loader_text.is_none() && crate::plugins::loader::loader_path().is_file();
    match (&loader_text, legacy_seen) {
        (None, false) => {
            if loader_unreadable {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — exists but could not be read as text; \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else {
                writeln!(
                    out,
                    "  {info} plugin loader      none — plugin sources activate through it once \
                     a verb manages them"
                )?;
            }
        }
        (Some(_), false) => {
            if let Some(reason) = &loader_invalid {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — failed validation ({reason}); \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else {
                writeln!(
                    out,
                    "  {ok} plugin loader      {} (sourced before the rc; user overrides win)",
                    display(&crate::plugins::loader::loader_path())
                )?;
            }
        }
        (loader, true) => {
            let mut lines: Vec<String> = rc_legacy_blocks
                .unwrap_or_default()
                .iter()
                .map(|b| format!("{} (lines {}-{})", b.id, b.start_line, b.end_line))
                .collect();
            // Guard trips the block scan does not attribute are named by
            // line, so the state is always explainable.
            for (line, text) in &guard_trips {
                lines.push(format!("guard trip at line {line}: {text}"));
            }
            // The hold verdict comes from the guard itself (F2-prime).
            let state = if held_off {
                "the generated loader stays OFF while they execute"
            } else {
                "inert (comments only — nothing executes, the loader is unaffected)"
            };
            writeln!(
                out,
                "  {warn} legacy rc blocks   {} — {state}; remove them BY HAND after \
                 reviewing: `niu plugin migrate-rc`",
                lines.join(", ")
            )?;
            if let Some(reason) = &loader_invalid {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — failed validation ({reason}); \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else if loader_unreadable {
                writeln!(
                    out,
                    "  {warn} plugin loader      {} — exists but could not be read as text; \
                     it is not sourced at startup; regenerate with `niu plugin sync`",
                    display(&crate::plugins::loader::loader_path())
                )?;
            } else if loader.is_none() {
                writeln!(
                    out,
                    "  {info} plugin loader      none yet — `niu plugin migrate-rc` publishes \
                     the candidate generation from the current manifest"
                )?;
            }
        }
    }
    if crate::plugins::assets::rc_has_migrated_debris() {
        writeln!(
            out,
            "  {warn} rc moved-out debris stale moved-out remnants of the retired drift guard \
             found in the rc (an older niu's write-era damage, niubash#324/#647) — niu never \
             repairs your rc; review the moved-out sections by hand"
        )?;
    }

    // ── Advisory: PATH-shadowed niu (wt82-L01 V1 class) ──────────────────
    // The rc bootstrap and every bare `niu` typing resolve `niu` through
    // PATH. When PATH finds a *different* niu than the one running right
    // now, startup reconcile runs under that binary's semantics and old
    // vintages print `unknown plugin subcommand` noise on every start.
    match find_niu_on_path() {
        Some(path_niu)
            if !same_executable(&path_niu, &std::env::current_exe().unwrap_or_default())
                && !same_packaged_origin(&path_niu) =>
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
    // never reported as missing. The bundled layout is a Windows-release
    // concept (#194): on other platforms the winuxcmd tree is absent, so
    // the row is labeled n/a rather than reading as "nothing installed".
    let bundled = crate::setup_wizard::bundled_components();
    writeln!(out, "  {info}{}", bundled_tools_row(&bundled, platform))?;

    let terminal = match platform {
        Platform::Windows => {
            if std::env::var_os("WT_SESSION").is_some() {
                "Windows Terminal".to_string()
            } else {
                "console host".to_string()
            }
        }
        // WT_SESSION is a Windows Terminal variable; on unix fall back to
        // TERM so the row stays informative instead of always "console host".
        Platform::Unix => std::env::var("TERM")
            .map(|term| format!("unix terminal ({term})"))
            .unwrap_or_else(|_| "unix terminal".to_string()),
    };
    writeln!(out, "  {info} terminal            {terminal}")?;

    // ── Advisory: elevation channel (niubash#429, decision A) ────────────
    // Elevation is delegated, never built in: the Rubash `sudo` builtin
    // stays disabled unless NIU_ENABLE_RUBASH_SUDO=1, and an external
    // provider owns UAC — Microsoft sudo (in-box from Windows build
    // 26045) or gsudo. The doctor names the provider this machine
    // actually has so a user without either (the common Windows 10 case)
    // learns why `sudo` does not elevate here instead of hitting a
    // silently disabled builtin. Windows-only layer (#194): on other
    // platforms the row is n/a and the probes never run.
    let provider = if platform.checks_windows_command_layer() {
        elevation_provider(
            windows_build_number(),
            find_exe_on_path("sudo").is_some(),
            find_exe_on_path("gsudo").is_some(),
        )
    } else {
        ElevationProvider::None
    };
    let (tone, row) = elevation_channel_row(provider, platform);
    let mark = match tone {
        RowTone::Ok => ok,
        RowTone::Warn => warn,
        RowTone::Info => info,
    };
    writeln!(out, "  {mark}{row}")?;

    // ── Advisory: TAB completion shape (2026-10-12 discovery surface) ─────
    // The completion style was invisible config before 1.4.1 — the wizard
    // never asked and the env variable was hidden knowledge. The doctor
    // states the effective value (file/env/default precedence applied) and
    // names every switch, so the five shapes are discoverable.
    let (style, source) = crate::config::resolve_completion_style();
    let origin = match source {
        crate::config::SettingSource::Env => " (NIU_COMPLETION_STYLE override)",
        crate::config::SettingSource::Inputrc => " (inputrc)",
        crate::config::SettingSource::Default => " (default)",
    };
    writeln!(
        out,
        "  {info} completion style    {}{origin} — change with: \
         `export NIU_COMPLETION_STYLE=ide|column|list|inline|gnu` \
         in the environment or ~/.niubashrc",
        style.as_str()
    )?;

    // ── Advisory: TAB completion interaction (niubash#631) ──────────────
    // The second completion axis rides beside the style row: how TAB
    // drives the line while candidates are shown, with the same
    // precedence display (env > config > default).
    let (interaction, interaction_source) = crate::config::resolve_completion_interaction();
    let interaction_origin = match interaction_source {
        crate::config::SettingSource::Env => " (NIU_COMPLETION_INTERACTION override)",
        crate::config::SettingSource::Inputrc => " (inputrc)",
        crate::config::SettingSource::Default => " (default)",
    };
    writeln!(
        out,
        "  {info} completion interact {}{interaction_origin} — change with: \
         `export NIU_COMPLETION_INTERACTION=insert|cycle` \
         in the environment or ~/.niubashrc",
        interaction.as_str()
    )?;

    // ── Advisory: key bindings (niubash#630) ─────────────────────────────
    // The doctor is the standing face of the unified keymap registry:
    // the effective table itself is the help topic's job, but the
    // dropped records (with one-line fixes) live here permanently, so
    // a bind that never landed is discoverable without waiting for a
    // startup reminder. The snapshot is the last interactive build's;
    // doctor never sources rc files to rebuild it.
    match crate::keymap_registry::load_state_from(&crate::keymap_registry::state_path()) {
        Some(state) => {
            writeln!(
                out,
                "  {info} key bindings      {} effective ({} mode, registry generation {}), \
                 {} dropped — full table and fixes: `{}`",
                state.entries.len(),
                state.editor_mode,
                state.registry_generation,
                state.dropped.len(),
                crate::keymap_registry::keymap_help_command()
            )?;
            for bind in &state.dropped {
                writeln!(
                    out,
                    "  {info}   dropped '{}' ({}) — {}; fix: {}",
                    bind.keyseq,
                    bind.target_name.as_deref().unwrap_or(""),
                    bind.reason.why(),
                    bind.session_fix()
                )?;
            }
        }
        None => {
            writeln!(
                out,
                "  {info} key bindings      no interactive session has built a keymap yet \
                 — `{}` explains the binding layers",
                crate::keymap_registry::keymap_help_command()
            )?;
        }
    }

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
    // Sources whose tree is gone must be *visible*: the guarded loaders
    // fell back to the niubash defaults at startup and the user deserves
    // to know.
    let sources = crate::plugins::sources::list_sources();
    // niubash#178: an unparsable registry silently reads as "none installed"
    // everywhere, so the doctor must call it out explicitly — naming the bad
    // line (via the parse error) and the fix — instead of letting the empty
    // view stand.
    if let Some(corruption) = crate::plugins::sources::registry_corruption() {
        writeln!(
            out,
            "  {warn} plugin registry     UNPARSABLE ({}) — every registry \
             record in it is UNLOADED and sync/install are refused until you \
             fix or remove the file; the original is preserved at {}",
            corruption.error.lines().next().unwrap_or("parse error"),
            crate::plugins::sources::corrupt_registry_sidecar_path().display(),
        )?;
    }
    if sources.is_empty() {
        writeln!(
            out,
            "  {info} plugin sources      none installed — browse with `niu plugin discover`"
        )?;
    } else {
        let ready = sources.iter().filter(|s| !s.missing).count();
        let missing = sources.iter().filter(|s| s.missing).count();
        writeln!(
            out,
            "  {info} plugin sources      {ready} ready, {missing} missing \
             (`niu plugin list` for assets)"
        )?;
        for status in sources.iter().filter(|s| s.missing) {
            writeln!(
                out,
                "  {warn} source missing      '{}' tree missing — its assets stay unloaded; \
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
    // pastes their own mirror URL (`niu plugin mirror set <url>`). niu has
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
                     point git at a mirror of your choice: `niu plugin mirror set <url>`"
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
/// what the release actually ships, or an explicit "none". On non-Windows
/// platforms (#194) the winuxcmd opt/ layout the bundle lives in does not
/// exist, so an empty bundle reads as n/a, not as an empty install.
fn bundled_tools_row(bundled: &[&str], platform: Platform) -> String {
    if !bundled.is_empty() {
        format!(
            " bundled tools       {} (shipped with the release, not system installs)",
            bundled.join(" ")
        )
    } else if platform.checks_windows_command_layer() {
        " bundled tools       none — this install ships no pre-installed packages".to_string()
    } else {
        " bundled tools       n/a — bundled tools ship in the Windows release layout".to_string()
    }
}

/// Severity of a doctor row that carries text produced by a helper (the
/// report loop owns the colored/plain markers; helpers stay pure so the
/// row wording is unit-testable without capturing stdout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowTone {
    Ok,
    Warn,
    Info,
}

/// Which external provider answers elevation on this machine
/// (niubash#429, decision A). Niubash itself never elevates: the Rubash
/// `sudo` builtin is opt-in only and UAC belongs to one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElevationProvider {
    /// Microsoft sudo: in-box `sudo.exe`, ships from Windows build 26045
    /// (Windows 11 24H2).
    MsSudo,
    /// gsudo on PATH (the WPM package).
    Gsudo,
    /// Neither provider detected.
    None,
}

/// First Windows build carrying Microsoft sudo in-box (Windows 11 24H2).
const MS_SUDO_MIN_BUILD: u32 = 26045;

/// Classify the elevation channel from the raw probes. Microsoft sudo
/// wins when both providers exist — it is the in-box channel, and gsudo
/// stays reachable by its own name either way. The build floor is part
/// of the test: a stray `sudo.exe` on an older build is not Microsoft
/// sudo and must not be reported as one.
fn elevation_provider(
    build: Option<u32>,
    ms_sudo_on_path: bool,
    gsudo_on_path: bool,
) -> ElevationProvider {
    if ms_sudo_on_path && matches!(build, Some(b) if b >= MS_SUDO_MIN_BUILD) {
        ElevationProvider::MsSudo
    } else if gsudo_on_path {
        ElevationProvider::Gsudo
    } else {
        ElevationProvider::None
    }
}

/// The `elevation channel` advisory row (without the leading two-space
/// indent; the caller prepends the tone marker). Non-Windows platforms
/// reuse the shared n/a wording (#194): the MS-sudo/gsudo layer is a
/// Windows component, and the row must never read as "no provider
/// found" there.
fn elevation_channel_row(provider: ElevationProvider, platform: Platform) -> (RowTone, String) {
    if !platform.checks_windows_command_layer() {
        return (
            RowTone::Info,
            format!(" elevation channel   {WINDOWS_COMPONENT_NA}"),
        );
    }
    match provider {
        ElevationProvider::MsSudo => (
            RowTone::Ok,
            " elevation channel   Microsoft sudo detected (build >= 26045, sudo.exe on PATH) \
             — elevation is delegated to Windows; niu's builtin sudo stays off \
             (opt-in: NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
        ElevationProvider::Gsudo => (
            RowTone::Ok,
            " elevation channel   gsudo detected on PATH — elevation is delegated to \
             gsudo; niu's builtin sudo stays off (opt-in: NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
        ElevationProvider::None => (
            RowTone::Warn,
            " elevation channel   none found — install Microsoft sudo (Windows 11 24H2, \
             build >= 26045) or gsudo (`wpm install gsudo`) for elevated commands; \
             niu's builtin sudo is opt-in only (NIU_ENABLE_RUBASH_SUDO=1)"
                .to_string(),
        ),
    }
}

/// Windows OS build number, read from the registry
/// (`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\CurrentBuildNumber`).
/// `None` when unreadable — callers treat an unknown build as below any
/// floor rather than guessing.
#[cfg(windows)]
fn windows_build_number() -> Option<u32> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
    };

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";
    let mut key: HKEY = std::ptr::null_mut();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            to_wide(KEY).as_ptr(),
            0,
            KEY_READ,
            &mut key,
        )
    };
    if opened != ERROR_SUCCESS {
        return None;
    }
    let mut buf = vec![0u16; 64];
    let mut len = (buf.len() * std::mem::size_of::<u16>()) as u32;
    let mut ty = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            to_wide("CurrentBuildNumber").as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr() as *mut u8,
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS || ty != REG_SZ {
        return None;
    }
    let chars = (len as usize / 2).min(buf.len());
    let text = String::from_utf16_lossy(&buf[..chars]);
    text.trim_end_matches('\0').trim().parse().ok()
}

#[cfg(not(windows))]
fn windows_build_number() -> Option<u32> {
    None
}

/// What `command -v niu` resolves to on this machine. `None` = nothing
/// on PATH answers to `niu`.
fn find_niu_on_path() -> Option<PathBuf> {
    find_exe_on_path("niu")
}

/// The first PATH entry that carries an executable `name` (`.exe`/
/// `.bat`/`.cmd`/`.com`, plus the extension-less form some POSIX-side
/// PATHs expose). `None` = nothing on PATH answers to `name`.
fn find_exe_on_path(name: &str) -> Option<PathBuf> {
    const EXTS: &[&str] = &[".exe", ".bat", ".cmd", ".com", ""];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| {
            EXTS.iter()
                .map(|ext| dir.join(format!("{name}{ext}")))
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

/// niubash#495: under MSIX the PATH copy is the AppExecutionAlias stub —
/// `%LOCALAPPDATA%\Microsoft\WindowsApps\niu.exe`, a reparse point that
/// never canonicalizes to the package's real `niu.exe` under
/// `C:\Program Files\WindowsApps`. Both names start this exact package, so
/// the shadow warning must not fire on them.
fn same_packaged_origin(path_niu: &Path) -> bool {
    crate::msix::is_packaged() && crate::msix::is_app_execution_alias_path(path_niu)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// niubash#230 follow-up: the bundled-tools row names exactly what the
    /// release ships and labels it as bundled, never as a system install.
    #[test]
    fn bundled_tools_row_labels_the_release_source() {
        let row = bundled_tools_row(&["gawk", "niugit", "ripgrep", "fd"], Platform::Windows);
        assert!(row.contains("gawk niugit ripgrep fd"), "{row}");
        assert!(row.contains("shipped with the release"), "{row}");
        assert!(row.contains("not system installs"), "{row}");
    }

    #[test]
    fn bundled_tools_row_is_honest_when_empty_on_windows() {
        let row = bundled_tools_row(&[], Platform::Windows);
        assert!(row.contains("none"), "{row}");
        assert!(!row.contains("shipped"), "{row}");
    }

    // ── niubash#194: platform gating of the Windows command layer ──────────

    /// The Windows command layer (winuxcmd core, command links, shims) must
    /// be probed on Windows and skipped on unix — the skip decision is the
    /// same code path `run_doctor` branches on, exercised here with an
    /// injected platform so it holds regardless of the host OS.
    #[test]
    fn windows_command_layer_is_checked_only_on_windows() {
        assert!(Platform::Windows.checks_windows_command_layer());
        assert!(!Platform::Unix.checks_windows_command_layer());
        // The runtime default must agree with the compile-time target.
        assert_eq!(
            Platform::current().checks_windows_command_layer(),
            cfg!(windows)
        );
    }

    /// On unix the skipped rows must read as n/a (not as a broken install),
    /// and the bundled-tools row must not imply an empty install. Asserts
    /// the exact shared constant the report prints.
    #[test]
    fn unix_skipped_rows_read_as_na_not_failure() {
        assert!(WINDOWS_COMPONENT_NA.contains("n/a"));
        assert!(WINDOWS_COMPONENT_NA.contains("Windows component"));
        assert!(!WINDOWS_COMPONENT_NA.contains("reinstall"));
        assert!(!WINDOWS_COMPONENT_NA.contains("not found"));

        // And the bundled row on unix is explicitly n/a about the layout.
        let bundled = bundled_tools_row(&[], Platform::Unix);
        assert!(bundled.contains("n/a"), "{bundled}");
        assert!(bundled.contains("Windows release layout"), "{bundled}");
        assert!(!bundled.contains("reinstall"), "{bundled}");
    }

    /// The header names the platform so users can tell which checks apply.
    #[test]
    fn platform_labels_are_distinct() {
        assert_ne!(Platform::Windows.label(), Platform::Unix.label());
        assert_eq!(Platform::Windows.label(), "windows");
        assert_eq!(Platform::Unix.label(), "unix");
    }

    // ── niubash#429 (decision A): the elevation-channel advisory ──────────

    /// Microsoft sudo counts only at or above the 24H2 build floor, and
    /// only with `sudo.exe` actually on PATH; an unknown build never
    /// counts as new enough.
    #[test]
    fn ms_sudo_needs_the_build_floor_and_the_binary() {
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), true, false),
            ElevationProvider::MsSudo
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD - 1), true, false),
            ElevationProvider::None
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), false, false),
            ElevationProvider::None
        );
        assert_eq!(
            elevation_provider(None, true, false),
            ElevationProvider::None
        );
    }

    /// gsudo is the fallback channel; Microsoft sudo (the in-box one)
    /// wins when both are present.
    #[test]
    fn gsudo_is_the_fallback_channel() {
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD - 1), true, true),
            ElevationProvider::Gsudo
        );
        assert_eq!(
            elevation_provider(None, false, true),
            ElevationProvider::Gsudo
        );
        assert_eq!(
            elevation_provider(Some(MS_SUDO_MIN_BUILD), true, true),
            ElevationProvider::MsSudo
        );
        assert_eq!(
            elevation_provider(None, false, false),
            ElevationProvider::None
        );
    }

    /// On Windows the row names the detected provider, and the
    /// no-provider row is a warning that names both install routes and
    /// the opt-in builtin variable.
    #[test]
    fn elevation_row_reports_the_windows_channel() {
        let (tone, row) = elevation_channel_row(ElevationProvider::MsSudo, Platform::Windows);
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("Microsoft sudo"), "{row}");

        let (tone, row) = elevation_channel_row(ElevationProvider::Gsudo, Platform::Windows);
        assert_eq!(tone, RowTone::Ok);
        assert!(row.contains("gsudo"), "{row}");

        let (tone, row) = elevation_channel_row(ElevationProvider::None, Platform::Windows);
        assert_eq!(tone, RowTone::Warn);
        assert!(row.contains("none found"), "{row}");
        assert!(row.contains("26045"), "{row}");
        assert!(row.contains("gsudo"), "{row}");
        assert!(row.contains("NIU_ENABLE_RUBASH_SUDO"), "{row}");
    }

    /// Non-Windows platforms never report a missing provider: the layer
    /// is a Windows component and the row reads n/a whatever the probes
    /// would have said.
    #[test]
    fn elevation_row_is_na_off_windows() {
        for provider in [
            ElevationProvider::MsSudo,
            ElevationProvider::Gsudo,
            ElevationProvider::None,
        ] {
            let (tone, row) = elevation_channel_row(provider, Platform::Unix);
            assert_eq!(tone, RowTone::Info);
            assert!(row.contains(WINDOWS_COMPONENT_NA), "{row}");
            assert!(!row.contains("none found"), "{row}");
        }
    }

    /// The build probe is compile-time honest: off Windows it reports
    /// unknown rather than inventing a build.
    #[test]
    fn windows_build_number_is_unknown_off_windows() {
        if !cfg!(windows) {
            assert_eq!(windows_build_number(), None);
        }
    }
}
