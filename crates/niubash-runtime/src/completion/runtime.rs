use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::completion::{CompletionContext, CompletionPlugin, CompletionResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCompletionCommand {
    pub command: String,
    pub args: Vec<String>,
    pub origin: String,
}

pub struct RuntimeCompletionPlugin {
    sources: HashMap<String, RuntimeCompletionCommand>,
    timeout: Duration,
}

impl RuntimeCompletionPlugin {
    pub fn new<I>(sources: I, timeout: Duration) -> Self
    where
        I: IntoIterator<Item = RuntimeCompletionCommand>,
    {
        let mut map = HashMap::new();
        for source in sources {
            if is_safe_name(&source.command)
                && source.args.iter().all(|arg| is_safe_completion_arg(arg))
            {
                map.insert(source.command.clone(), source);
            }
        }

        Self {
            sources: map,
            timeout: timeout.max(Duration::from_millis(1)),
        }
    }

    fn complete_for_source(
        &self,
        source: &RuntimeCompletionCommand,
        context: &CompletionContext,
    ) -> Option<CompletionResult> {
        if context.is_command_position() {
            return None;
        }

        let words = command_words_before_cursor(context)?;
        if words.first()? != &source.command {
            return None;
        }

        let mut args = source.args.clone();
        args.extend(words.iter().cloned());

        let output = run_runtime_completion(source, &args, context, self.timeout).ok()?;
        let current_word = context.get_current_word().unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let mut completions: Vec<String> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| context.behavior.matches(line, &current_word))
            .filter(|line| seen.insert((*line).to_string()))
            .map(str::to_string)
            .collect();
        completions.sort();

        if completions.is_empty() {
            None
        } else {
            Some(CompletionResult::new(completions))
        }
    }
}

impl CompletionPlugin for RuntimeCompletionPlugin {
    fn name(&self) -> &str {
        "runtime-completion"
    }

    fn complete(&self, context: &CompletionContext) -> Option<CompletionResult> {
        let command = context.get_command_name()?;
        let source = self.sources.get(&command)?;
        self.complete_for_source(source, context)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn run_runtime_completion(
    source: &RuntimeCompletionCommand,
    args: &[String],
    context: &CompletionContext,
    timeout: Duration,
) -> Result<String, String> {
    if !is_safe_name(&source.command) {
        return Err(format!(
            "unsafe runtime completion command: {}",
            source.command
        ));
    }
    if args.iter().any(|arg| !is_safe_completion_arg(arg)) {
        return Err(format!(
            "unsafe runtime completion args for {}: {}",
            source.command,
            args.join(" ")
        ));
    }

    let stdout_path = runtime_completion_temp_path(&source.command, "stdout");
    let stderr_path = runtime_completion_temp_path(&source.command, "stderr");
    let stdout = std::fs::File::create(&stdout_path)
        .map_err(|err| format!("failed to create stdout capture: {}", err))?;
    let stderr = std::fs::File::create(&stderr_path)
        .map_err(|err| format!("failed to create stderr capture: {}", err))?;

    let command_path =
        resolve_command_path(&source.command).unwrap_or_else(|| PathBuf::from(&source.command));

    let mut child = Command::new(command_path)
        .args(args)
        .env("COMP_LINE", context.input.clone())
        .env("COMP_POINT", context.cursor_pos.to_string())
        .env("COMP_CWORD", comp_cword(context).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|err| format!("failed to run runtime completion command: {}", err))?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&stdout_path);
                    let _ = std::fs::remove_file(&stderr_path);
                    return Err(format!(
                        "runtime completion command timed out after {:?}",
                        timeout
                    ));
                }
                sleep(Duration::from_millis(10));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&stdout_path);
                let _ = std::fs::remove_file(&stderr_path);
                return Err(format!(
                    "failed while waiting for runtime completion command: {}",
                    err
                ));
            }
        }
    };

    let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);

    if !status.success() {
        return Err(format!(
            "runtime completion command exited with {}: {}",
            status,
            stderr.trim()
        ));
    }

    Ok(stdout)
}

fn command_words_before_cursor(context: &CompletionContext) -> Option<Vec<String>> {
    let pos = context.cursor_pos.min(context.input.len());
    let pos = floor_char_boundary(&context.input, pos);
    let before_cursor = &context.input[..pos];
    let start = before_cursor
        .rfind(|ch: char| ch == ';' || ch == '|' || ch == '&' || ch == '\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let segment = before_cursor[start..].trim_start();
    let words = split_shell_words(segment);
    (!words.is_empty()).then_some(words)
}

fn comp_cword(context: &CompletionContext) -> usize {
    let Some(words) = command_words_before_cursor(context) else {
        return 0;
    };
    if context
        .input
        .get(..context.cursor_pos.min(context.input.len()))
        .and_then(|input| input.chars().last())
        .map_or(false, char::is_whitespace)
    {
        words.len()
    } else {
        words.len().saturating_sub(1)
    }
}

fn split_shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut single = false;
    let mut double = false;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            ch if ch.is_whitespace() && !single && !double => {
                if !current.is_empty() {
                    words.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_safe_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn is_safe_completion_arg(arg: &str) -> bool {
    !arg.chars().any(|ch| matches!(ch, '\0' | '\n' | '\r'))
}

fn runtime_completion_temp_path(command: &str, stream: &str) -> PathBuf {
    let safe_command = sanitize_cache_component(command);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "niubash-runtime-completion-{}-{}-{}-{}.tmp",
        safe_command,
        std::process::id(),
        nanos,
        stream
    ))
}

fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.is_file() {
        return Some(command_path);
    }

    let path = std::env::var_os("PATH")?;
    let has_extension = PathBuf::from(command)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some();
    let extensions: &[&str] = if has_extension {
        &[""]
    } else if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };

    for dir in std::env::split_paths(&path) {
        for ext in extensions {
            let candidate = dir.join(format!("{}{}", command, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn sanitize_cache_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn floor_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}
