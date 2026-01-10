// ghcc only supports Linux and macOS
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("ghcc only supports Linux and macOS");

mod auth;
mod copilot;
mod git;

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

fn print_usage() {
    eprintln!("ghcc - Generate conventional commit messages with GitHub Copilot");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  ghcc [options]      Generate commit message for staged changes");
    eprintln!("  ghcc login          Authenticate with GitHub Copilot");
    eprintln!("  ghcc status         Show authentication status");
    eprintln!("  ghcc models         List and select Copilot models");
    eprintln!("  ghcc hook install   Install git prepare-commit-msg hook");
    eprintln!("  ghcc hook uninstall Remove git prepare-commit-msg hook");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -d, --detailed      Include bullet-point body (default: single-line)");
    eprintln!("  -a, --auto          Let AI decide format (experimental)");
    eprintln!("  -h, --help          Show this help message");
    eprintln!("  -V, --version       Show version");
}

fn cmd_login() -> Result<(), Box<dyn std::error::Error>> {
    let copilot_auth = auth::login()?;
    auth::save_auth(&copilot_auth)?;

    let path = auth::auth_file_path()?;
    eprintln!("Authentication successful!");
    eprintln!("Token saved to: {}", path.display());
    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    match auth::read_auth() {
        Ok(copilot_auth) => {
            let expired = auth::is_expired(&copilot_auth);
            let expires_at = copilot_auth.expires / 1000;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            eprintln!("Authenticated: yes");
            if expired {
                eprintln!("Token status: expired (will refresh on next use)");
            } else {
                let remaining = expires_at.saturating_sub(now);
                let mins = remaining / 60;
                eprintln!("Token status: valid ({} minutes remaining)", mins);
            }
            if let Some(model) = copilot_auth.model {
                eprintln!("Selected model: {}", model);
            } else {
                eprintln!("Selected model: {} (default)", copilot::DEFAULT_MODEL);
            }
            Ok(())
        }
        Err(auth::AuthError::NotFound) => {
            eprintln!("Authenticated: no");
            eprintln!("Run 'ghcc login' to authenticate");
            Ok(())
        }
        Err(auth::AuthError::Parse(msg)) => {
            eprintln!("Authenticated: unknown (auth file is invalid)");
            eprintln!("{}", msg);
            eprintln!("Run 'ghcc login' to refresh credentials");
            Ok(())
        }
        Err(err) => Err(Box::new(err)),
    }
}

fn cmd_models() -> Result<(), Box<dyn std::error::Error>> {
    let mut auth = auth::get_valid_auth()?;
    let models = copilot::list_models(&auth)?;

    if models.is_empty() {
        eprintln!("No enabled models found.");
        return Ok(());
    }

    eprintln!();
    eprintln!("Available models:");
    eprintln!();

    for (i, model) in models.iter().enumerate() {
        let current = auth.model.as_deref().unwrap_or(copilot::DEFAULT_MODEL);
        let marker = if model.id == current { "*" } else { " " };
        eprintln!(
            "{:>2}. {} {:<20} {} ({})",
            i + 1,
            marker,
            model.id,
            model.name,
            model.vendor
        );
    }
    eprintln!();

    let current = auth.model.as_deref().unwrap_or(copilot::DEFAULT_MODEL);
    eprintln!("Current: {}", current);
    eprintln!();
    eprint!("Enter number to select (q to cancel): ");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let input = input.trim();
    if input.eq_ignore_ascii_case("q") || input.is_empty() {
        return Ok(());
    }

    if let Ok(index) = input.parse::<usize>() {
        if index > 0 && index <= models.len() {
            let selected = &models[index - 1];
            auth.model = Some(selected.id.clone());
            auth.max_prompt_tokens = selected
                .capabilities
                .as_ref()
                .and_then(|c| c.limits.as_ref())
                .and_then(|l| l.max_prompt_tokens);
            auth::save_auth(&auth)?;
            eprintln!("Model set to: {}", selected.id);
        } else {
            eprintln!("Invalid selection.");
        }
    } else {
        eprintln!("Invalid input.");
    }

    Ok(())
}

fn hook_script(style_flag: &str) -> String {
    format!(
        r#"#!/bin/sh
[ -z "$2" ] && exec ghcc --hook "$1" {}
exit 0
"#,
        style_flag
    )
}

fn cmd_hook_install(style: copilot::CommitStyle) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let style_flag = match style {
        copilot::CommitStyle::SingleLine => "",
        copilot::CommitStyle::Detailed => "-d",
        copilot::CommitStyle::Auto => "-a",
    };

    // Get git directory
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()?;

    if !output.status.success() {
        return Err("Not a git repository".into());
    }

    let git_dir = String::from_utf8(output.stdout)?.trim().to_string();
    let hook_path = std::path::PathBuf::from(&git_dir)
        .join("hooks")
        .join("prepare-commit-msg");

    // Check if hook already exists (but allow overwriting our own hook)
    if hook_path.exists() {
        let existing = fs::read_to_string(&hook_path)?;
        if !existing.contains("ghcc") {
            return Err(format!(
                "Hook already exists at {}. Remove it first or manually add ghcc.",
                hook_path.display()
            )
            .into());
        }
    }

    // Create hooks directory if needed
    if let Some(parent) = hook_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write hook
    fs::write(&hook_path, hook_script(style_flag))?;

    // Make executable
    let mut perms = fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms)?;

    let style_desc = match style {
        copilot::CommitStyle::SingleLine => "single-line",
        copilot::CommitStyle::Detailed => "detailed (-d)",
        copilot::CommitStyle::Auto => "auto (-a)",
    };
    eprintln!("Hook installed at {} ({})", hook_path.display(), style_desc);
    Ok(())
}

fn cmd_hook_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::process::Command;

    // Get git directory
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()?;

    if !output.status.success() {
        return Err("Not a git repository".into());
    }

    let git_dir = String::from_utf8(output.stdout)?.trim().to_string();
    let hook_path = std::path::PathBuf::from(&git_dir)
        .join("hooks")
        .join("prepare-commit-msg");

    if !hook_path.exists() {
        eprintln!("No hook found at {}", hook_path.display());
        return Ok(());
    }

    let existing = fs::read_to_string(&hook_path)?;
    if !existing.contains("ghcc") {
        return Err(format!(
            "Hook at {} was not installed by ghcc. Remove it manually.",
            hook_path.display()
        )
        .into());
    }

    fs::remove_file(&hook_path)?;
    eprintln!("Hook removed from {}", hook_path.display());
    Ok(())
}

/// Called by git hook: ghcc --hook <msg-file> [style-flags]
fn cmd_hook(msg_file: &str, style: copilot::CommitStyle) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;

    if !git::has_staged_changes()? {
        // No staged changes, leave message file untouched
        return Ok(());
    }

    // Short-circuit for initial commit
    if git::is_initial_commit()? {
        let content = "Initial commit\n\n# To abort: delete all lines or exit with :cq (vim)\n";
        fs::write(msg_file, content)?;
        return Ok(());
    }

    let auth = auth::get_valid_auth()?;

    let diff = git::get_diff()?;
    let diff_stat = git::get_diff_stat()?;
    let message = copilot::generate_commit_message(&auth, &diff, &diff_stat, style)?;

    // Write to message file with abort hint
    let content = format!(
        "{}\n\n# To abort: delete all lines or exit with :cq (vim)\n",
        message
    );
    fs::write(msg_file, content)?;

    Ok(())
}

fn cmd_generate(style: copilot::CommitStyle) -> Result<(), Box<dyn std::error::Error>> {
    let auth = auth::get_valid_auth()?;

    if !git::has_staged_changes()? {
        eprintln!("No staged changes found. Please stage your changes using 'git add'.");
        return Ok(());
    }

    // Short-circuit for initial commit
    if git::is_initial_commit()? {
        println!("Initial commit");
        return Ok(());
    }

    let diff = git::get_diff()?;
    let diff_stat = git::get_diff_stat()?;
    let message = copilot::generate_commit_message(&auth, &diff, &diff_stat, style)?;

    // Output to stdout for piping: `git commit -m "$(ghcc)"`
    println!("{}", message);

    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    // Determine commit style from flags
    let style = if args.iter().any(|a| a == "-d" || a == "--detailed") {
        copilot::CommitStyle::Detailed
    } else if args.iter().any(|a| a == "-a" || a == "--auto") {
        copilot::CommitStyle::Auto
    } else {
        copilot::CommitStyle::SingleLine
    };

    // Check for --hook <msg-file> (internal, called by git hook)
    if let Some(pos) = args.iter().position(|a| a == "--hook") {
        if let Some(msg_file) = args.get(pos + 1) {
            return match cmd_hook(msg_file, style) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    ExitCode::from(1)
                }
            };
        } else {
            eprintln!("Usage: ghcc --hook <msg-file> [-d|-a]");
            return ExitCode::from(1);
        }
    }

    // Get command (first non-flag argument after program name)
    let cmd = args.iter().skip(1).find(|a| !a.starts_with('-'));

    let result = match cmd.map(|s| s.as_str()) {
        Some("login") => cmd_login(),
        Some("status") => cmd_status(),
        Some("models") => cmd_models(),
        Some("hook") => {
            // Get subcommand for hook
            let subcmd = args.iter().skip(2).find(|a| !a.starts_with('-'));
            match subcmd.map(|s| s.as_str()) {
                Some("install") => cmd_hook_install(style),
                Some("uninstall") => cmd_hook_uninstall(),
                _ => {
                    eprintln!("Usage: ghcc hook <install|uninstall> [-d|-a]");
                    return ExitCode::from(1);
                }
            }
        }
        Some("help") => {
            print_usage();
            Ok(())
        }
        None if args.iter().any(|a| a == "--help" || a == "-h") => {
            print_usage();
            Ok(())
        }
        None if args.iter().any(|a| a == "--version" || a == "-V") => {
            eprintln!("ghcc {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            print_usage();
            return ExitCode::from(1);
        }
        None => cmd_generate(style),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_script_single_line() {
        let script = hook_script("");

        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("ghcc --hook \"$1\""));
        assert!(script.contains("exit 0"));
        // Should not have extra flags
        assert!(!script.contains("-d"));
        assert!(!script.contains("-a"));
    }

    #[test]
    fn test_hook_script_detailed() {
        let script = hook_script("-d");

        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("ghcc --hook \"$1\" -d"));
    }

    #[test]
    fn test_hook_script_auto() {
        let script = hook_script("-a");

        assert!(script.starts_with("#!/bin/sh"));
        assert!(script.contains("ghcc --hook \"$1\" -a"));
    }

    #[test]
    fn test_hook_script_checks_for_commit_type() {
        // The script should only run when $2 is empty (no commit type provided)
        // This means it runs for normal commits but not for merge, squash, etc.
        let script = hook_script("");

        assert!(script.contains("[ -z \"$2\" ]"));
    }
}
