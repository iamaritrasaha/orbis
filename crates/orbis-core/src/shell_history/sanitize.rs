//! Command-signature sanitization for shell-history insights.
//!
//! The invariant is strict: a signature may contain the executable name and,
//! when provably safe, one leading benign subcommand word. Everything else —
//! paths, URLs, option values, environment assignments, credentials — is
//! dropped. Uncertain input degrades to the executable name alone or to
//! nothing at all. Signatures are never derived from argument content.

/// Option names whose values (or bare presence) are treated as sensitive.
const SENSITIVE_LONG_OPTIONS: [&str; 10] = [
    "--password",
    "--passwd",
    "--pass",
    "--token",
    "--api-key",
    "--apikey",
    "--secret",
    "--authorization",
    "--header",
    "--user",
];

/// Short options treated as sensitive wherever they appear.
const SENSITIVE_SHORT_OPTIONS: [char; 3] = ['p', 'H', 'u'];

/// First-level subcommands considered safe to display next to the executable.
const SAFE_SUBCOMMANDS: [&str; 44] = [
    "add",
    "build",
    "check",
    "clean",
    "clippy",
    "commit",
    "config",
    "diff",
    "doc",
    "doctor",
    "fetch",
    "fmt",
    "info",
    "init",
    "install",
    "lint",
    "list",
    "log",
    "login",
    "new",
    "outdated",
    "prune",
    "pull",
    "push",
    "remove",
    "run",
    "search",
    "serve",
    "show",
    "start",
    "status",
    "stop",
    "test",
    "uninstall",
    "update",
    "upgrade",
    "version",
    "audit",
    "bench",
    "listr",
    "repl",
    "shell",
    "tree",
    "why",
];

/// Shells and remote-execution commands whose arguments are never summarized.
const EXECUTABLE_ONLY: [&str; 14] = [
    "curl", "wget", "ssh", "scp", "sftp", "nc", "ncat", "socat", "openssl", "gpg", "vault", "aws",
    "gcloud", "az",
];

/// Reduces one raw history line to a safe normalized signature.
///
/// Returns `None` when no safe signature can be derived.
pub fn sanitize_command(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Composites, substitutions, and redirections change what actually ran.
    // Only the executable level is certain, so everything else is dropped.
    if line.contains(['|', ';', '&', '`'])
        || line.contains("$(")
        || line.contains('<')
        || line.contains('>')
    {
        return executable_only(line);
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut iterator = tokens.iter().map(|token| token.to_owned());
    let mut first = iterator.next().expect("tokens checked");
    // Leading environment assignments are stripped, never displayed.
    while is_assignment(first) {
        {
            let next = iterator.next()?;
            first = next
        }
    }
    let executable = basename(first);
    if executable.is_empty()
        || executable.starts_with('-')
        || executable.contains(['$', '*', '?', '~', '='])
    {
        return None;
    }
    if executable.eq_ignore_ascii_case("sudo") || executable.eq_ignore_ascii_case("doas") {
        return Some(sanitize_command(&tokens[1..].join(" ")).unwrap_or(executable));
    }
    if EXECUTABLE_ONLY.iter().any(|known| known.eq_ignore_ascii_case(&executable)) {
        return Some(executable);
    }
    // The next meaningful token may be a benign subcommand; flags, options,
    // and anything option-like are not.
    for token in iterator {
        if is_assignment(token) {
            continue;
        }
        if token.starts_with('-') {
            // A value-carrying sensitive option means the following word must
            // never be treated as a subcommand.
            if is_sensitive_option(token) {
                return Some(executable);
            }
            continue;
        }
        if is_safe_subcommand(token) {
            return Some(format!("{executable} {token}"));
        }
        return Some(executable);
    }
    Some(executable)
}

fn executable_only(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut first = tokens.first()?.to_owned();
    while is_assignment(first) {
        first = tokens.get(1)?.to_owned();
    }
    let executable = basename(first.trim_end_matches([';', '|', '&']));
    match executable.as_str() {
        "" => None,
        "sudo" | "doas" => {
            let rest = &tokens[1..];
            match if rest.is_empty() { None } else { executable_only(&rest.join(" ")) } {
                Some(signature) if !signature.starts_with('-') => Some(signature),
                _ => Some(executable),
            }
        }
        _ => Some(executable),
    }
}

fn basename(token: &str) -> String {
    token.rsplit('/').next().unwrap_or(token).to_owned()
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else { return false };
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_safe_subcommand(token: &str) -> bool {
    SAFE_SUBCOMMANDS.iter().any(|safe| safe.eq_ignore_ascii_case(token))
}

fn is_sensitive_option(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if SENSITIVE_SHORT_OPTIONS.iter().any(|short| lower == format!("-{short}")) {
        return true;
    }
    SENSITIVE_LONG_OPTIONS
        .iter()
        .any(|long| lower == *long || lower.starts_with(&format!("{long}=")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_executable_and_benign_subcommand() {
        assert_eq!(sanitize_command("git status"), Some("git status".into()));
        assert_eq!(sanitize_command("cargo test"), Some("cargo test".into()));
        assert_eq!(sanitize_command("cargo test --release"), Some("cargo test".into()));
        assert_eq!(sanitize_command("orbis update"), Some("orbis update".into()));
        assert_eq!(sanitize_command("flutter run"), Some("flutter run".into()));
        assert_eq!(sanitize_command("npm install"), Some("npm install".into()));
        // An option taking a separate value cannot be distinguished from a
        // positional argument, so the signature degrades to the executable.
        assert_eq!(sanitize_command("git -C /tmp status"), Some("git".into()));
    }

    #[test]
    fn unknown_subcommands_degrade_to_executable() {
        assert_eq!(sanitize_command("git weird-op --whatever"), Some("git".into()));
        assert_eq!(sanitize_command("mytool /etc/passwd"), Some("mytool".into()));
    }

    #[test]
    fn sensitive_and_remote_commands_are_executable_only() {
        assert_eq!(sanitize_command("curl https://secret-url.invalid/x"), Some("curl".into()));
        assert_eq!(sanitize_command("ssh user@private-host"), Some("ssh".into()));
        assert_eq!(sanitize_command("aws s3 cp a b"), Some("aws".into()));
    }

    #[test]
    fn secret_values_never_appear() {
        let cases = [
            "dbctl --password=hunter2 query",
            "dbctl --password hunter2 query",
            "upload --api-key=abc123 file",
            "curl -H 'Authorization: Bearer xyz' https://x.invalid",
            "curl -u admin:secretpw https://x.invalid",
            "export OPENAI_API_KEY=sk-abcdef",
            "TOKEN=abc123 deploy now",
            "wget --token=xyz https://host.invalid",
        ];
        for case in cases {
            let signature = sanitize_command(case).unwrap_or_default();
            assert!(!signature.contains("hunter2"), "leaked in {case}");
            assert!(!signature.contains("abc123"), "leaked in {case}");
            assert!(!signature.contains("sk-abcdef"), "leaked in {case}");
            assert!(!signature.contains("Bearer"), "leaked in {case}");
            assert!(!signature.contains("secretpw"), "leaked in {case}");
            assert!(!signature.contains("xyz"), "leaked in {case}");
            assert!(!signature.contains("x.invalid"), "leaked in {case}");
            assert!(!signature.contains("host"), "leaked in {case}");
            // The executable itself is safe; the surrounding secret values are
            // what must never survive.
        }
    }

    #[test]
    fn composites_reduce_to_executable_only() {
        assert_eq!(sanitize_command("ls | wc -l"), Some("ls".into()));
        assert_eq!(sanitize_command("a && b"), Some("a".into()));
        assert_eq!(sanitize_command("echo $(whoami)"), Some("echo".into()));
        assert_eq!(sanitize_command("cmd1; cmd2"), Some("cmd1".into()));
        assert_eq!(sanitize_command("grep x > out.txt"), Some("grep".into()));
        assert_eq!(sanitize_command("sudo apt update"), Some("apt update".into()));
        assert_eq!(sanitize_command("sudo -i"), Some("sudo".into()));
    }

    #[test]
    fn assignments_and_globs_and_empty_lines_produce_nothing() {
        assert_eq!(sanitize_command("FOO=bar BAZ=qux"), None);
        assert_eq!(sanitize_command(""), None);
        assert_eq!(sanitize_command("# comment"), None);
        assert_eq!(sanitize_command("   "), None);
    }

    #[test]
    fn paths_collapse_to_executable_basename() {
        assert_eq!(sanitize_command("/usr/bin/btop --utf8"), Some("btop".into()));
        assert_eq!(sanitize_command("./scripts/build.sh all"), Some("build.sh".into()));
    }
}
