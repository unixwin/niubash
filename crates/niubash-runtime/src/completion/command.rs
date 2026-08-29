// Command completion for WinSH
// Provides Tab completion for executable commands

use std::env;
use std::sync::Mutex;

use crate::completion::{CompletionContext, CompletionResult};
use anyhow::Result;

/// Cache of PATH commands to avoid scanning on every keystroke.
static PATH_CMD_CACHE: Mutex<Option<(Vec<String>, Vec<String>)>> = Mutex::new(None);

/// Command completer
pub struct CommandCompleter;

impl CommandCompleter {
    /// Get all available commands (built-in + common + PATH)
    pub fn get_all_commands() -> Vec<String> {
        let mut commands = Self::get_builtin_commands();
        commands.extend(Self::get_common_commands());
        commands.extend(Self::get_path_commands_cached());
        commands.sort();
        commands.dedup();
        commands
    }

    /// Get shell builtins owned by rubash.
    pub fn get_builtin_commands() -> Vec<String> {
        [
            ".",
            ":",
            "[",
            "alias",
            "bg",
            "bind",
            "break",
            "builtin",
            "caller",
            "cd",
            "command",
            "compgen",
            "complete",
            "compopt",
            "continue",
            "declare",
            "dirs",
            "disown",
            "echo",
            "enable",
            "eval",
            "exec",
            "exit",
            "export",
            "false",
            "fc",
            "fg",
            "getopts",
            "hash",
            "help",
            "history",
            "jobs",
            "kill",
            "let",
            "local",
            "logout",
            "mapfile",
            "popd",
            "printf",
            "pushd",
            "pwd",
            "read",
            "readarray",
            "readonly",
            "return",
            "set",
            "setopt",
            "shift",
            "shopt",
            "source",
            "suspend",
            "test",
            "times",
            "trap",
            "true",
            "type",
            "typeset",
            "ulimit",
            "umask",
            "unalias",
            "unset",
            "unsetopt",
            "wait",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    /// Get commonly-used commands (shown on empty Tab)
    pub fn get_common_commands() -> Vec<String> {
        vec![
            "ls".to_string(),
            "cd".to_string(),
            "pwd".to_string(),
            "echo".to_string(),
            "exit".to_string(),
            "clear".to_string(),
            "cat".to_string(),
            "grep".to_string(),
            "find".to_string(),
            "cp".to_string(),
            "mv".to_string(),
            "rm".to_string(),
            "mkdir".to_string(),
            "jobs".to_string(),
            "fg".to_string(),
            "bg".to_string(),
            "set".to_string(),
            "setopt".to_string(),
            "unset".to_string(),
            "unsetopt".to_string(),
            "export".to_string(),
            "env".to_string(),
            "help".to_string(),
            "history".to_string(),
            "alias".to_string(),
            "unalias".to_string(),
            "source".to_string(),
            "array".to_string(),
            "plugin".to_string(),
            "theme".to_string(),
            "oh-my-niu".to_string(),
            "git".to_string(),
            "code".to_string(),
            "code-insiders".to_string(),
            "explorer".to_string(),
            "start".to_string(),
            "wt".to_string(),
            "codex".to_string(),
            "pwsh".to_string(),
            "powershell".to_string(),
            "cmd".to_string(),
            "where".to_string(),
            "which".to_string(),
            "touch".to_string(),
            "head".to_string(),
            "tail".to_string(),
            "sort".to_string(),
            "uniq".to_string(),
            "wc".to_string(),
            "tee".to_string(),
            "diff".to_string(),
            "less".to_string(),
            "more".to_string(),
            "xargs".to_string(),
            "basename".to_string(),
            "dirname".to_string(),
            "realpath".to_string(),
            "sleep".to_string(),
            "timeout".to_string(),
            "watch".to_string(),
            "nano".to_string(),
            "vim".to_string(),
            "nvim".to_string(),
            "node".to_string(),
            "npm".to_string(),
            "npx".to_string(),
            "python".to_string(),
            "pip".to_string(),
            "cargo".to_string(),
            "rustc".to_string(),
            "rustup".to_string(),
            "gh".to_string(),
            "docker".to_string(),
            "docker-compose".to_string(),
            "make".to_string(),
            "cmake".to_string(),
            "ninja".to_string(),
            "ssh".to_string(),
            "scp".to_string(),
            "curl".to_string(),
            "wget".to_string(),
            "ping".to_string(),
            "ipconfig".to_string(),
            "netstat".to_string(),
            "nslookup".to_string(),
            "tracert".to_string(),
            "choco".to_string(),
            "scoop".to_string(),
            "winget".to_string(),
            "reg".to_string(),
            "tasklist".to_string(),
            "taskkill".to_string(),
            "systeminfo".to_string(),
        ]
    }

    /// Get cached PATH commands (scans once, caches forever)
    fn get_path_commands_cached() -> Vec<String> {
        if let Ok(cache) = PATH_CMD_CACHE.lock() {
            if let Some((ref cmds, _)) = *cache {
                return cmds.clone();
            }
        }
        let cmds = Self::get_path_commands();
        if let Ok(mut cache) = PATH_CMD_CACHE.lock() {
            *cache = Some((cmds.clone(), Vec::new()));
        }
        cmds
    }

    /// Get commands from PATH environment variable
    pub fn get_path_commands() -> Vec<String> {
        let mut commands = Vec::new();

        if let Ok(path_env) = env::var("PATH") {
            for path in env::split_paths(&path_env) {
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        if let Ok(file_type) = entry.file_type() {
                            if file_type.is_file() {
                                let file_name = entry.file_name().to_string_lossy().to_string();

                                // Recognise Windows executables and shell scripts.
                                // Extensionless files in PATH are skipped because
                                // Windows PATH dirs commonly contain LICENSE, README,
                                // etc. Extensionless tools like dsh are covered by
                                // oh-my-niu or the common-commands list.
                                let is_executable = file_name.ends_with(".exe")
                                    || file_name.ends_with(".bat")
                                    || file_name.ends_with(".cmd")
                                    || file_name.ends_with(".ps1")
                                    || file_name.ends_with(".sh")
                                    || file_name.ends_with(".bash")
                                    || file_name.ends_with(".zsh")
                                    || file_name.ends_with(".niubash");

                                if is_executable {
                                    // Remove extension for cleaner completion
                                    let name_without_ext = if let Some(pos) = file_name.rfind('.') {
                                        file_name[..pos].to_string()
                                    } else {
                                        file_name.clone()
                                    };
                                    commands.push(name_without_ext);
                                }
                            }
                        }
                    }
                }
            }
        }

        commands
    }

    /// Complete a command name
    pub fn complete(context: &CompletionContext) -> Result<Option<CompletionResult>> {
        if !context.is_command_position() {
            return Ok(None);
        }

        let word = context.get_current_word().unwrap_or_default();

        // Empty input at command position → show common commands
        if word.is_empty() {
            let common = Self::get_common_commands();
            let mut matches: Vec<String> = common
                .into_iter()
                .filter(|cmd| context.behavior.matches(cmd, ""))
                .collect();
            if let Some(limit) = context.behavior.max_command_results {
                matches.truncate(limit);
            }
            if matches.is_empty() {
                return Ok(None);
            }
            return Ok(Some(CompletionResult::new(matches)));
        }

        // Normal prefix matching: built-in + cached PATH commands
        let all_commands = Self::get_all_commands();

        let mut matches: Vec<String> = all_commands
            .into_iter()
            .filter(|cmd| context.behavior.matches(cmd, &word))
            .collect();
        if let Some(limit) = context.behavior.max_command_results {
            matches.truncate(limit);
        }

        if matches.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResult::new(matches)))
        }
    }

    /// Check if a command exists in PATH
    pub fn command_exists(command: &str) -> bool {
        Self::get_builtin_commands().contains(&command.to_string())
            || Self::get_path_commands().contains(&command.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_builtin_commands() {
        let commands = CommandCompleter::get_builtin_commands();
        assert!(commands.contains(&"cd".to_string()));
        assert!(commands.contains(&"setopt".to_string()));
        assert!(commands.contains(&"unsetopt".to_string()));
        assert!(!commands.contains(&"ls".to_string()));
        assert!(!commands.contains(&"cp".to_string()));
        assert!(!commands.contains(&"self-update".to_string()));
        assert!(!commands.contains(&"update-niubash".to_string()));
    }

    #[test]
    fn test_command_exists() {
        assert!(CommandCompleter::command_exists("cd"));
        assert!(!CommandCompleter::command_exists(
            "__niubash_missing_command_for_completion_test__"
        ));

        // Verify PATH command lookup without hardcoding platform-specific binaries.
        if let Some(any_path_cmd) = CommandCompleter::get_path_commands().first() {
            assert!(CommandCompleter::command_exists(any_path_cmd));
        }
    }

    #[test]
    fn command_completion_uses_substring_matching_when_configured() {
        let ctx = CompletionContext::with_behavior(
            std::path::PathBuf::from("."),
            "ep".to_string(),
            2,
            crate::completion::CompletionBehavior {
                match_mode: crate::completion::CompletionMatchMode::Substring,
                ..crate::completion::CompletionBehavior::default()
            },
        );
        let result = CommandCompleter::complete(&ctx).unwrap().unwrap();
        assert!(result.completions.contains(&"grep".to_string()));
    }

    #[test]
    fn command_completion_includes_common_commands_for_partial_input() {
        let ctx = CompletionContext::new(std::path::PathBuf::from("."), "oh-".to_string(), 3);
        let result = CommandCompleter::complete(&ctx).unwrap().unwrap();
        assert!(result.completions.contains(&"oh-my-niu".to_string()));
    }

    #[test]
    fn command_completion_respects_case_sensitivity_and_result_cap() {
        let ctx = CompletionContext::with_behavior(
            std::path::PathBuf::from("."),
            "GRE".to_string(),
            3,
            crate::completion::CompletionBehavior {
                case_sensitive: true,
                max_command_results: Some(1),
                ..crate::completion::CompletionBehavior::default()
            },
        );
        let completions = CommandCompleter::complete(&ctx)
            .unwrap()
            .map(|result| result.completions)
            .unwrap_or_default();
        assert!(!completions.contains(&"grep".to_string()));

        let limited = CompletionContext::with_behavior(
            std::path::PathBuf::from("."),
            "".to_string(),
            0,
            crate::completion::CompletionBehavior {
                max_command_results: Some(1),
                ..crate::completion::CompletionBehavior::default()
            },
        );
        let result = CommandCompleter::complete(&limited).unwrap().unwrap();
        assert_eq!(result.completions.len(), 1);
    }
}
