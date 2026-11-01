//! Host-owned completion sources for GNU bash's context-specific
//! completion entry points: usernames (`complete-username`, M-~) and
//! hostnames (`complete-hostname`, M-@).
//!
//! GNU sources: `rl_username_completion_function` (lib/readline/complete.c:
//! 2377, backed by `getpwent`) and bashline.c:2626
//! `hostname_completion_function`, backed by `snarf_hosts_from_file`
//! (bashline.c:800). Both keep the caller's leading marker (`~` or `@`) in
//! the returned candidates and match a case-sensitive prefix.

use crate::completion::CompletionResult;

/// Candidates for `complete-username` (`rl_username_completion_function`).
pub fn complete_username(word: &str) -> Option<CompletionResult> {
    username_matches(&default_usernames(), word)
}

/// Candidates for `complete-hostname` (`hostname_completion_function`).
pub fn complete_hostname(word: &str) -> Option<CompletionResult> {
    hostname_matches(&default_hostnames(), word)
}

/// Filter USERS by the username prefix in WORD, preserving a leading `~`.
pub fn username_matches(users: &[String], word: &str) -> Option<CompletionResult> {
    let (prefix, query) = split_marker(word, '~');
    let mut matches: Vec<String> = users
        .iter()
        .filter(|user| user.starts_with(query))
        .map(|user| format!("{prefix}{user}"))
        .collect();
    matches.sort();
    matches.dedup();
    (!matches.is_empty()).then(|| CompletionResult::new(matches))
}

/// Filter HOSTS by the hostname prefix in WORD, preserving a leading `@`.
pub fn hostname_matches(hosts: &[String], word: &str) -> Option<CompletionResult> {
    let (prefix, query) = split_marker(word, '@');
    let mut matches: Vec<String> = hosts
        .iter()
        .filter(|host| host.starts_with(query))
        .map(|host| format!("{prefix}{host}"))
        .collect();
    matches.sort();
    matches.dedup();
    (!matches.is_empty()).then(|| CompletionResult::new(matches))
}

/// Split the GNU completion marker (`~` for users, `@` for hosts) from the
/// prefix the user typed. GNU strips at most the first character.
fn split_marker(word: &str, marker: char) -> (&'static str, &str) {
    match word.strip_prefix(marker) {
        Some(rest) if marker == '~' => ("~", rest),
        Some(rest) => ("@", rest),
        None => ("", word),
    }
}

/// Parse the username field of an `/etc/passwd` body.
///
/// Only the first colon-separated field (the login name) is used; the
/// password field and every later field are ignored. Comment, blank and
/// malformed lines are skipped.
pub fn passwd_usernames(contents: &str) -> Vec<String> {
    let mut users = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(':');
        let Some(name) = fields.next().filter(|name| !name.is_empty()) else {
            continue;
        };
        if fields.next().is_none() {
            continue;
        }
        users.push(name.to_string());
    }
    users
}

/// Parse the names in an `/etc/hosts` body, following bashline.c
/// `snarf_hosts_from_file`: skip comments and blank lines, skip a leading
/// numeric internet address, stop at an inline `#`, and collect every
/// remaining whitespace-separated alias. `$include` lines are ignored (the
/// host reads `/etc/hosts` only, not the preprocessor form).
pub fn hosts_file_hostnames(contents: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in contents.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with("$include ") {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        let names = if first.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            // The first field was the internet address, not a name.
            fields.collect::<Vec<_>>()
        } else {
            std::iter::once(first).chain(fields).collect::<Vec<_>>()
        };
        for name in names {
            if name.starts_with('#') {
                break;
            }
            hosts.push(name.to_string());
        }
    }
    hosts
}

/// The platform's username list.
///
/// Unix reads `/etc/passwd` (the same account database GNU's `getpwent`
/// path opens for the local entries), falling back to `USER` when the file
/// is missing or empty.
#[cfg(unix)]
pub fn default_usernames() -> Vec<String> {
    let mut users = std::fs::read_to_string("/etc/passwd")
        .map(|contents| passwd_usernames(&contents))
        .unwrap_or_default();
    if users.is_empty() {
        if let Ok(user) = std::env::var("USER") {
            if !user.is_empty() {
                users.push(user);
            }
        }
    }
    users.sort();
    users.dedup();
    users
}

/// Windows has no `/etc/passwd` (GNU itself returns no matches there). The
/// host fallback offers `USERNAME` and the final component of
/// `USERPROFILE`, which is the account name Windows expands `~` to.
#[cfg(windows)]
pub fn default_usernames() -> Vec<String> {
    let mut users = Vec::new();
    if let Ok(user) = std::env::var("USERNAME") {
        if !user.is_empty() {
            users.push(user);
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if let Some(user) = std::path::Path::new(&profile)
            .file_name()
            .and_then(|name| name.to_str())
        {
            if !user.is_empty() {
                users.push(user.to_string());
            }
        }
    }
    users.sort();
    users.dedup();
    users
}

#[cfg(not(any(unix, windows)))]
pub fn default_usernames() -> Vec<String> {
    Vec::new()
}

/// The platform's hostname list.
///
/// Unix reads `/etc/hosts` and appends the kernel hostname
/// (`gethostname`), mirroring bashline.c's `get_hostname_list` plus the
/// local machine's own name.
#[cfg(unix)]
pub fn default_hostnames() -> Vec<String> {
    let mut hosts = std::fs::read_to_string("/etc/hosts")
        .map(|contents| hosts_file_hostnames(&contents))
        .unwrap_or_default();
    if let Some(local) = local_hostname() {
        hosts.push(local);
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Windows has no portable hostname database without DNS/NetBIOS
/// enumeration. The documented fallback offers `COMPUTERNAME` and
/// `localhost`; it does not invent names that the OS cannot enumerate.
#[cfg(windows)]
pub fn default_hostnames() -> Vec<String> {
    let mut hosts = vec!["localhost".to_string()];
    if let Ok(host) = std::env::var("COMPUTERNAME") {
        if !host.is_empty() {
            hosts.push(host);
        }
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

#[cfg(not(any(unix, windows)))]
pub fn default_hostnames() -> Vec<String> {
    Vec::new()
}

#[cfg(unix)]
fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is valid for its full length and gethostname only
    // writes within it (a truncated name is still NUL-terminated on Unix).
    let status = unsafe {
        libc::gethostname(
            buffer.as_mut_ptr().cast::<libc::c_char>(),
            buffer.len() as libc::size_t,
        )
    };
    if status != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec())
        .ok()
        .filter(|host| !host.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn passwd_parser_reads_only_the_login_field() {
        let contents = "\
# local accounts
root:x:0:0:root:/root:/bin/bash

alice:$6$rounds=1000$hash:1000:1000:Alice:/home/alice:/bin/sh
nobody:*:65534:65534::/:/usr/sbin/nologin
malformed-line-without-colons
";
        assert_eq!(
            passwd_usernames(contents),
            names(&["root", "alice", "nobody"]),
            "the password and later fields never leak into a username"
        );
    }

    #[test]
    fn username_matching_preserves_the_tilde_and_filters_by_prefix() {
        let users = names(&["root", "alice", "bob"]);
        assert_eq!(
            username_matches(&users, "~a").unwrap().completions,
            names(&["~alice"])
        );
        // GNU also completes a bare name when the word has no `~`.
        assert_eq!(
            username_matches(&users, "b").unwrap().completions,
            names(&["bob"])
        );
        assert_eq!(
            username_matches(&users, "~").unwrap().completions,
            names(&["~alice", "~bob", "~root"])
        );
        assert!(username_matches(&users, "~zzz").is_none());
    }

    #[test]
    fn hosts_parser_skips_addresses_comments_and_blanks() {
        let contents = "\
# /etc/hosts
127.0.0.1 localhost localhost.localdomain
   10.0.0.5 buildbox builder   # inline comment
$include /etc/hosts.local
";
        assert_eq!(
            hosts_file_hostnames(contents),
            names(&["localhost", "localhost.localdomain", "buildbox", "builder"]),
            "only the names after the address are candidates"
        );
    }

    #[test]
    fn hostname_matching_preserves_the_at_marker_and_filters_by_prefix() {
        let hosts = names(&["localhost", "buildbox", "builder"]);
        assert_eq!(
            hostname_matches(&hosts, "@bu").unwrap().completions,
            names(&["@buildbox", "@builder"])
        );
        assert_eq!(
            hostname_matches(&hosts, "loc").unwrap().completions,
            names(&["localhost"])
        );
        assert_eq!(
            hostname_matches(&hosts, "@").unwrap().completions,
            names(&["@buildbox", "@builder", "@localhost"])
        );
        assert!(hostname_matches(&hosts, "@gone").is_none());
    }
}
