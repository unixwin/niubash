// Embedded completion definitions for common external commands (issue #305).
//
//! git, tar, curl, ssh, docker, kubectl, and npm are not WinuxCmd applets, so
//! they live outside the generated `winuxcmd` asset corpus. The data is a
//! hand-curated static subset of each tool's documented subcommands and
//! common flags — no external completion script is ever executed. The tables
//! use the same `CommandDef` schema as the embedded applet definitions.
//!
//! Load-order contract (mirrors `winuxcmd_assets`, see
//! `ExternalCompletionPlugin`): these definitions are seeded at construction
//! time as part of layer 0 — bundle definitions replace them per command, and
//! user completion dirs override everything.
#[allow(clippy::module_name_repetitions)]
pub static COMMON_COMPLETION_TOMLS: &[(&str, &str)] = &[
    (
        "curl",
        include_str!("../../assets/completions/common/curl.toml"),
    ),
    (
        "docker",
        include_str!("../../assets/completions/common/docker.toml"),
    ),
    (
        "git",
        include_str!("../../assets/completions/common/git.toml"),
    ),
    (
        "kubectl",
        include_str!("../../assets/completions/common/kubectl.toml"),
    ),
    (
        "npm",
        include_str!("../../assets/completions/common/npm.toml"),
    ),
    (
        "ssh",
        include_str!("../../assets/completions/common/ssh.toml"),
    ),
    (
        "tar",
        include_str!("../../assets/completions/common/tar.toml"),
    ),
];

/// Parse every embedded definition. Files that fail to parse are
/// skipped with a warning; a broken asset is a build/CI problem, not a
/// runtime one (same contract as
/// [`crate::completion::winuxcmd_assets::definitions`]).
pub fn definitions() -> impl Iterator<Item = crate::completion::external::CommandDef> {
    COMMON_COMPLETION_TOMLS
        .iter()
        .filter_map(|(command, toml)| match toml::from_str(toml) {
            Ok(def) => Some(def),
            Err(e) => {
                log::warn!("embedded common completion {command} failed to parse: {e}");
                None
            }
        })
}
