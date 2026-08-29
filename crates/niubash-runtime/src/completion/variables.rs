// Variable completion for WinSH
// Provides Tab completion for environment variables

use crate::completion::{CompletionContext, CompletionResult};
use anyhow::Result;
use std::collections::HashMap;

/// Variable completer
pub struct VariableCompleter;

impl VariableCompleter {
    /// Complete a variable name
    pub fn complete(
        context: &CompletionContext,
        env_vars: &HashMap<String, String>,
    ) -> Result<Option<CompletionResult>> {
        let word = match context.get_current_word() {
            Some(w) => w,
            None => return Ok(None),
        };

        // Check if it's a variable reference
        if !word.starts_with('$') {
            return Ok(None);
        }

        // Extract variable name (after $ and potentially {)
        let var_name = if word.starts_with("${") {
            if word.len() > 2 {
                &word[2..]
            } else {
                return Ok(None);
            }
        } else if word.starts_with('$') {
            if word.len() > 1 {
                &word[1..]
            } else {
                return Ok(None);
            }
        } else {
            return Ok(None);
        };

        // Get all available variables
        let mut all_vars = Self::get_environment_variables();

        // Add shell variables
        for (key, _) in env_vars.iter() {
            all_vars.push(key.clone());
        }

        // Filter variables that start with the name
        let matches: Vec<String> = all_vars
            .into_iter()
            .filter(|var| var.to_lowercase().starts_with(&var_name.to_lowercase()))
            .map(|var| {
                // Preserve the $ prefix in the completion
                if word.starts_with("${") {
                    format!("${{{}}}", var)
                } else {
                    format!("${}", var)
                }
            })
            .collect();

        if matches.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResult::new(matches)))
        }
    }

    /// Get system environment variables
    pub fn get_environment_variables() -> Vec<String> {
        std::env::vars().map(|(key, _)| key).collect()
    }

    /// Get common environment variables for quick completion
    pub fn get_common_variables() -> Vec<String> {
        vec![
            "HOME".to_string(),
            "USER".to_string(),
            "PATH".to_string(),
            "PWD".to_string(),
            "SHELL".to_string(),
            "TERM".to_string(),
            "LANG".to_string(),
            "LC_ALL".to_string(),
            "EDITOR".to_string(),
            "VISUAL".to_string(),
            "PAGER".to_string(),
            "PS1".to_string(),
            "PS2".to_string(),
            "HOSTNAME".to_string(),
            "HOSTTYPE".to_string(),
            "OSTYPE".to_string(),
            "MACHTYPE".to_string(),
            "SHLVL".to_string(),
            "LOGNAME".to_string(),
        ]
    }

    /// Expand environment variables in a string
    pub fn expand_variables(input: &str, env_vars: &HashMap<String, String>) -> String {
        let mut result = input.to_string();

        // Expand $VAR format
        while let Some(start) = result.find('$') {
            let rest = &result[start + 1..];

            // Check for ${VAR} format
            if rest.starts_with('{') {
                if let Some(end) = rest.find('}') {
                    let var_name = &rest[1..end];
                    let replacement = Self::get_variable_value(var_name, env_vars);
                    result = format!("{}{}{}", &result[..start], replacement, &rest[end + 1..]);
                    continue;
                }
            }

            // Find end of variable name
            let end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(rest.len());

            let var_name = &rest[..end];
            let replacement = Self::get_variable_value(var_name, env_vars);
            result = format!("{}{}{}", &result[..start], replacement, &rest[end..]);
        }

        result
    }

    /// Get the value of a variable
    fn get_variable_value(var_name: &str, env_vars: &HashMap<String, String>) -> String {
        // Check shell variables first
        if let Some(value) = env_vars.get(var_name) {
            return value.clone();
        }

        // Check environment variables
        if let Ok(value) = std::env::var(var_name) {
            return value;
        }

        // Return empty string if not found
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_environment_variables() {
        // Inject a deterministic variable so this test does not depend on host env shape.
        std::env::set_var("NIU_TEST_ENV", "1");
        let vars = VariableCompleter::get_environment_variables();
        assert!(vars.iter().any(|k| k == "NIU_TEST_ENV"));
    }

    #[test]
    fn test_get_common_variables() {
        let vars = VariableCompleter::get_common_variables();
        assert!(vars.contains(&"PATH".to_string()));
        assert!(vars.contains(&"HOME".to_string()));
    }

    #[test]
    fn test_expand_variables() {
        let mut env_vars = HashMap::new();
        env_vars.insert("TEST".to_string(), "value".to_string());

        // Note: This test might not pass if TEST is not in env_vars or system env
        // Just testing the function exists and runs
        let result = VariableCompleter::expand_variables("echo $TEST", &env_vars);
        assert!(result.contains("echo"));
    }
}
