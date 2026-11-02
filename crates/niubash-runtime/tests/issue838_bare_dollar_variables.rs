//! niubash#838 regression pins: a bare `$` prefix completes every
//! variable.
//!
//! Before the fix, `VariableCompleter::complete` returned `Ok(None)`
//! for 'echo $<TAB>' and 'echo ${<TAB>' because the stripped empty
//! var_name bailed before listing; the fallback then treated `$` as
//! a path word and the user got a silent zero. GNU lists every
//! variable on a bare `$` prefix (and `complete_names`, the
//! `complete-variable` entry point, already handled the bare case).

use std::collections::HashMap;
use std::path::PathBuf;

use niubash_runtime::completion::variables::VariableCompleter;
use niubash_runtime::completion::CompletionContext;

fn complete(line: &str, env_vars: &HashMap<String, String>) -> Vec<String> {
    let context = CompletionContext::new(PathBuf::from("."), line.to_string(), line.len());
    VariableCompleter::complete(&context, env_vars)
        .expect("variable completion must not error")
        .map(|result| result.completions)
        .unwrap_or_default()
}

fn fixture_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("ISSUE838_PROBE_VAR".to_string(), "probe".to_string());
    env
}

#[test]
fn issue838_bare_dollar_lists_variables() {
    let env = fixture_env();
    let values = complete("echo $", &env);
    assert!(
        values.iter().any(|v| v == "$ISSUE838_PROBE_VAR"),
        "bare $ must list session variables, got {values:?}"
    );
    // Process environment variables ride along (PATH exists in
    // every test environment).
    assert!(
        values.iter().any(|v| v == "$PATH"),
        "bare $ must list environment variables, got {values:?}"
    );
}

#[test]
fn issue838_bare_braced_dollar_lists_variables() {
    let env = fixture_env();
    let values = complete("echo ${", &env);
    assert!(
        values.iter().any(|v| v == "${ISSUE838_PROBE_VAR}"),
        "bare ${{ must list session variables in braced form, got {values:?}"
    );
}

#[test]
fn issue838_prefixed_dollar_still_filters() {
    let env = fixture_env();
    let values = complete("echo $ISSUE838_PROBE_V", &env);
    assert_eq!(values, vec!["$ISSUE838_PROBE_VAR".to_string()]);
    // A non-variable word still declines.
    let values = complete("echo plain", &env);
    assert!(values.is_empty(), "non-$ word must decline, got {values:?}");
}
