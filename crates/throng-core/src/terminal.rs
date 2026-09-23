//! Terminal rules that do not depend on how a PTY is spawned (Principles III and IV).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a Terminal Panel was configured with. Persisted inside the layout, so a restored panel can
/// cold-start the same shell when no live session is left to reattach to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPanelConfig {
    /// The shell's id in the detected catalogue (e.g. `bash`, `zsh`, `pwsh`). `None` = the user's
    /// default shell at spawn time.
    #[serde(default)]
    pub shell: Option<String>,
    /// Extra arguments appended after the shell's own.
    #[serde(default)]
    pub args: Vec<String>,
    /// Where the shell starts. `None` = the project root (Principle IV).
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Written to the shell once, on a cold start only, after its first output.
    #[serde(default)]
    pub startup_command: Option<String>,
    /// Reopen in the directory it was last working in. `None` follows the
    /// `terminal.defaultRememberDirectory` preference, which is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_directory: Option<bool>,
    /// The directory it was last seen working in, kept while it remembers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_directory: Option<PathBuf>,
}

/// Where a terminal starts on a cold start: the directory it last
/// worked in when it remembers, else the one it was asked to start in — each only while it is a
/// folder inside the project — else the project root. Nothing stored is trusted, and a folder that
/// has gone gives way to the root silently.
pub fn start_directory(
    config: &TerminalPanelConfig,
    root: &Path,
    remember_default: bool,
    rules: &crate::paths::PathRules,
    is_dir: impl Fn(&Path) -> bool,
) -> PathBuf {
    let remember = config.remember_directory.unwrap_or(remember_default);
    let remembered = config.last_directory.as_ref().filter(|_| remember);
    remembered
        .into_iter()
        .chain(config.cwd.as_ref())
        .find(|dir| rules.is_within(root, dir) && is_dir(dir))
        .cloned()
        .unwrap_or_else(|| root.to_path_buf())
}

/// Shell arguments as the user typed them: split on spaces, with `'…'` and `"…"` quoting and `\`
/// escaping.
#[must_use]
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"') | None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                started = true;
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// Arguments back into one line, quoted where [`split_args`] needs it to read them the same.
#[must_use]
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty() && !a.chars().any(|c| c.is_whitespace() || "'\"\\".contains(c)) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\"'\"'"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A shell the user can pick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellInfo {
    pub id: String,
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// How a terminal ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    /// The exit code, or `None` when it could not be read (killed by a signal, host lost).
    pub code: Option<i32>,
    /// Ended because the user closed or killed it, rather than on its own.
    pub user_killed: bool,
}

/// Whether an exit deserves a notice. A deliberate kill never does. Otherwise the exit code decides,
/// not who asked: typing `exit` is deliberate and exits 0, so gating on "did throng kill it" would
/// report the user's own action back to them and train them to dismiss notices unread.
#[must_use]
pub fn should_surface_exit(status: ExitStatus) -> bool {
    !status.user_killed && status.code != Some(0)
}

/// Who an exit notice is about. Absent parts are omitted, never shown blank.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalIdentity {
    pub project: Option<String>,
    pub tab: Option<String>,
    pub panel: Option<String>,
    pub shell: Option<String>,
}

/// "Terminal exited (code 1) — Web › Tab 1 › Panel 2 (bash)".
#[must_use]
pub fn exit_notice(code: Option<i32>, identity: &TerminalIdentity) -> String {
    let code_text = code.map_or_else(|| "—".to_owned(), |c| c.to_string());
    let head = format!("Terminal exited (code {code_text})");
    let place: Vec<&str> = [&identity.project, &identity.tab, &identity.panel]
        .into_iter()
        .filter_map(|part| part.as_deref().map(str::trim).filter(|s| !s.is_empty()))
        .collect();
    let mut suffix = place.join(" › ");
    if let Some(shell) = identity.shell.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !suffix.is_empty() {
            suffix.push(' ');
        }
        suffix.push('(');
        suffix.push_str(shell);
        suffix.push(')');
    }
    if suffix.is_empty() { head } else { format!("{head} — {suffix}") }
}

/// What happens to a terminal when its project or the application closes (Principle III).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerCloseAction {
    /// A command is running: leave it running and reattach on reopen.
    KeepRunning,
    /// An idle shell: close it now and cold-start a new one on reopen.
    Close,
}

/// A terminal is busy iff a process other than its shell holds the foreground. When that cannot be
/// determined the host must report busy — never silently treat a possibly-running command as idle.
#[must_use]
pub fn owner_close_action(busy: bool) -> OwnerCloseAction {
    if busy { OwnerCloseAction::KeepRunning } else { OwnerCloseAction::Close }
}

/// The user's choice when closing throng with busy terminals: exactly three.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseChoice {
    /// Close and leave busy terminals running in the background.
    LeaveRunning,
    /// Close and terminate every terminal.
    TerminateAll,
    /// Do not close; review the open terminals.
    Cancel,
}

/// Environment variables removed before a shell starts: throng's own markers must not leak into the
/// user's shells, where they would make a nested throng think it is a child of this one.
#[must_use]
pub fn is_private_env_var(name: &str) -> bool {
    name.starts_with("THRONG_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_starts_where_it_last_worked_while_that_is_a_folder_in_the_project() {
        let rules = crate::paths::PathRules::LINUX;
        let root = Path::new("/p");
        let exists = |p: &Path| p != Path::new("/p/gone");
        let mut config =
            TerminalPanelConfig { last_directory: Some("/p/src".into()), ..TerminalPanelConfig::default() };
        assert_eq!(start_directory(&config, root, true, &rules, exists), PathBuf::from("/p/src"));
        assert_eq!(
            start_directory(&config, root, false, &rules, exists),
            PathBuf::from("/p"),
            "preference off"
        );
        config.remember_directory = Some(true);
        assert_eq!(
            start_directory(&config, root, false, &rules, exists),
            PathBuf::from("/p/src"),
            "the panel wins"
        );
        config.last_directory = Some("/p/gone".into());
        config.cwd = Some("/p/docs".into());
        assert_eq!(
            start_directory(&config, root, true, &rules, exists),
            PathBuf::from("/p/docs"),
            "a gone folder gives way"
        );
        config.cwd = Some("/elsewhere".into());
        assert_eq!(
            start_directory(&config, root, true, &rules, exists),
            PathBuf::from("/p"),
            "never outside the project"
        );
    }

    #[test]
    fn shell_arguments_split_and_join_the_same_way() {
        assert_eq!(
            split_args(r#"-l --rcfile "my rc" 'a b' c\ d"#),
            vec!["-l", "--rcfile", "my rc", "a b", "c d"]
        );
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert_eq!(split_args("''"), vec![String::new()]);
        let args = vec!["-c".to_owned(), "it's here".to_owned(), String::new(), "x".to_owned()];
        assert_eq!(split_args(&join_args(&args)), args);
    }

    #[test]
    fn exits_surface_by_code_unless_user_killed() {
        assert!(!should_surface_exit(ExitStatus { code: Some(0), user_killed: false }));
        assert!(should_surface_exit(ExitStatus { code: Some(1), user_killed: false }));
        assert!(should_surface_exit(ExitStatus { code: None, user_killed: false }));
        assert!(!should_surface_exit(ExitStatus { code: Some(137), user_killed: true }));
    }

    #[test]
    fn exit_notice_names_what_it_can() {
        let full = TerminalIdentity {
            project: Some("Web".into()),
            tab: Some("Tab 1".into()),
            panel: Some("Panel 2".into()),
            shell: Some("bash".into()),
        };
        assert_eq!(exit_notice(Some(1), &full), "Terminal exited (code 1) — Web › Tab 1 › Panel 2 (bash)");
        assert_eq!(exit_notice(None, &TerminalIdentity::default()), "Terminal exited (code —)");
        let shell_only =
            TerminalIdentity { shell: Some("zsh".into()), panel: Some("  ".into()), ..Default::default() };
        assert_eq!(exit_notice(Some(2), &shell_only), "Terminal exited (code 2) — (zsh)");
    }

    #[test]
    fn busy_terminals_keep_running() {
        assert_eq!(owner_close_action(true), OwnerCloseAction::KeepRunning);
        assert_eq!(owner_close_action(false), OwnerCloseAction::Close);
    }

    #[test]
    fn private_env_is_throng_prefixed() {
        assert!(is_private_env_var("THRONG_HOME"));
        assert!(!is_private_env_var("PATH"));
    }
}
