use std::io;
use std::path::Path;
use std::process::Command;

#[derive(Debug)]
pub enum GitError {
    Io(io::Error),
    Git(String),
    Utf8(std::string::FromUtf8Error),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Io(e) => write!(f, "IO error: {}", e),
            GitError::Git(msg) => write!(f, "Git error: {}", msg),
            GitError::Utf8(e) => write!(f, "UTF-8 error: {}", e),
        }
    }
}

impl std::error::Error for GitError {}

impl From<io::Error> for GitError {
    fn from(e: io::Error) -> Self {
        GitError::Io(e)
    }
}

impl From<std::string::FromUtf8Error> for GitError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        GitError::Utf8(e)
    }
}

/// Read ignore patterns from .ghccignore file
pub(crate) fn read_ignore_patterns_from_path(path: &Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }

    match std::fs::read_to_string(path) {
        Ok(content) => content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect(),
        Err(e) => {
            eprintln!(
                "Warning: Could not read {}: {}. No files will be ignored.",
                path.display(),
                e
            );
            Vec::new()
        }
    }
}

/// Get the root directory of the current git repository
fn get_git_root() -> Option<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8(output.stdout).ok()?;
    Some(std::path::PathBuf::from(path.trim()))
}

/// Read ignore patterns from .ghccignore in repo root
fn read_ignore_patterns() -> Vec<String> {
    let path = get_git_root()
        .map(|root| root.join(".ghccignore"))
        .unwrap_or_else(|| std::path::PathBuf::from(".ghccignore"));
    read_ignore_patterns_from_path(&path)
}

/// Get the diff of staged changes
pub fn get_diff() -> Result<String, GitError> {
    let ignore_patterns = read_ignore_patterns();

    let mut args = vec!["diff", "--cached", "--"];

    // Add pathspecs: start with "." to include all, then exclude patterns
    args.push(".");

    // Build exclusion pathspecs
    let exclusions: Vec<String> = ignore_patterns.iter().map(|p| format!(":!{}", p)).collect();

    let exclusion_refs: Vec<&str> = exclusions.iter().map(|s| s.as_str()).collect();
    args.extend(exclusion_refs);

    let output = Command::new("git").args(&args).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::Git(stderr.to_string()));
    }

    let diff_lossy = String::from_utf8_lossy(&output.stdout);
    if matches!(diff_lossy, std::borrow::Cow::Owned(_)) {
        eprintln!("Warning: git diff output contained non-UTF8 bytes; replacing invalid sequences");
    }
    Ok(diff_lossy.into_owned())
}

/// Get the diff stat summary of staged changes (file names + lines changed)
pub fn get_diff_stat() -> Result<String, GitError> {
    let ignore_patterns = read_ignore_patterns();

    let mut args = vec!["diff", "--cached", "--stat", "--"];

    // Add pathspecs: start with "." to include all, then exclude patterns
    args.push(".");

    // Build exclusion pathspecs
    let exclusions: Vec<String> = ignore_patterns.iter().map(|p| format!(":!{}", p)).collect();

    let exclusion_refs: Vec<&str> = exclusions.iter().map(|s| s.as_str()).collect();
    args.extend(exclusion_refs);

    let output = Command::new("git").args(&args).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::Git(stderr.to_string()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check if there are any staged changes
pub fn has_staged_changes() -> Result<bool, GitError> {
    let diff = get_diff()?;
    Ok(!diff.is_empty())
}

/// Check if this is the initial commit (no commits yet; HEAD does not exist)
pub fn is_initial_commit() -> Result<bool, GitError> {
    let output = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .output()?;

    if output.status.success() {
        // If HEAD exists, at least one commit exists. Any new commit is not an initial commit.
        Ok(false)
    } else {
        // If HEAD doesn't exist yet (fresh repo, no commits), treat as initial commit.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("unknown revision")
            || stderr.contains("bad revision")
            || stderr.contains("ambiguous argument")
            || stderr.contains("Needed a single revision")
        {
            Ok(true)
        } else {
            Err(GitError::Git(stderr.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn test_read_ignore_patterns_parses_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "package-lock.json").unwrap();
        writeln!(file, "*.lock").unwrap();
        writeln!(file, "dist/").unwrap();

        let patterns = read_ignore_patterns_from_path(file.path());

        assert_eq!(patterns.len(), 3);
        assert!(patterns.contains(&"package-lock.json".to_string()));
        assert!(patterns.contains(&"*.lock".to_string()));
        assert!(patterns.contains(&"dist/".to_string()));
    }

    #[test]
    fn test_read_ignore_patterns_skips_comments() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "# This is a comment").unwrap();
        writeln!(file, "package-lock.json").unwrap();
        writeln!(file, "# Another comment").unwrap();
        writeln!(file, "*.lock").unwrap();

        let patterns = read_ignore_patterns_from_path(file.path());

        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"package-lock.json".to_string()));
        assert!(patterns.contains(&"*.lock".to_string()));
        assert!(!patterns.iter().any(|p| p.starts_with('#')));
    }

    #[test]
    fn test_read_ignore_patterns_skips_empty_lines() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "package-lock.json").unwrap();
        writeln!(file).unwrap(); // empty line
        writeln!(file, "   ").unwrap(); // whitespace-only line
        writeln!(file, "*.lock").unwrap();

        let patterns = read_ignore_patterns_from_path(file.path());

        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"package-lock.json".to_string()));
        assert!(patterns.contains(&"*.lock".to_string()));
    }

    #[test]
    fn test_read_ignore_patterns_returns_empty_when_missing() {
        let path = Path::new("/nonexistent/.ghccignore");
        let patterns = read_ignore_patterns_from_path(path);

        assert!(patterns.is_empty());
    }

    #[test]
    fn test_read_ignore_patterns_trims_whitespace() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "  package-lock.json  ").unwrap();
        writeln!(file, "\t*.lock\t").unwrap();

        let patterns = read_ignore_patterns_from_path(file.path());

        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"package-lock.json".to_string()));
        assert!(patterns.contains(&"*.lock".to_string()));
    }

    /// Helper to run is_initial_commit() in a specific directory
    fn is_initial_commit_in_dir(dir: &Path) -> Result<bool, GitError> {
        let original_dir = std::env::current_dir().map_err(GitError::Io)?;
        std::env::set_current_dir(dir).map_err(GitError::Io)?;

        let result = is_initial_commit();

        // Best-effort restore; if this fails, propagate to keep tests honest.
        std::env::set_current_dir(&original_dir).map_err(GitError::Io)?;

        result
    }

    #[test]
    fn test_is_initial_commit_fresh_repo_no_commits() {
        let dir = TempDir::new().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // No commits yet - should be treated as initial commit
        assert!(is_initial_commit_in_dir(dir.path()).unwrap());
    }

    #[test]
    fn test_is_initial_commit_one_commit() {
        let dir = TempDir::new().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Configure git user for commit
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create and commit a file
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "first"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // One commit exists - any new commit is not the initial commit
        assert!(!is_initial_commit_in_dir(dir.path()).unwrap());
    }

    #[test]
    fn test_is_initial_commit_two_commits() {
        let dir = TempDir::new().unwrap();

        // Initialize git repo
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Configure git user for commit
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create and commit first file
        std::fs::write(dir.path().join("file1.txt"), "content1").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "first"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Create and commit second file
        std::fs::write(dir.path().join("file2.txt"), "content2").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "second"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Two commits - should NOT be treated as initial commit
        assert!(!is_initial_commit_in_dir(dir.path()).unwrap());
    }
}
