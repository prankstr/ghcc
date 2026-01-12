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

    let mut args = vec!["diff", "--cached", "-U12", "--"];

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

/// Read diff content from a file or stdin (if path is "-")
pub fn read_diff_from_file(path: &str) -> Result<String, GitError> {
    use std::io::Read;

    if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| GitError::Git(format!("Failed to read stdin: {}", e)))?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| GitError::Git(format!("Failed to read {}: {}", path, e)))
    }
}

/// Derive a diff stat summary from diff content (similar to git diff --stat)
pub fn derive_diff_stat(diff: &str) -> String {
    let mut files: Vec<(String, usize, usize)> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ins = 0usize;
    let mut current_del = 0usize;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            // Save previous file stats
            if let Some(file) = current_file.take() {
                files.push((file, current_ins, current_del));
            }
            // Extract filename from "diff --git a/path b/path"
            if let Some(b_part) = line.split(" b/").nth(1) {
                current_file = Some(b_part.to_string());
                current_ins = 0;
                current_del = 0;
            }
        } else if current_file.is_some() {
            if line.starts_with('+') && !line.starts_with("+++") {
                current_ins += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                current_del += 1;
            }
        }
    }

    // Don't forget the last file
    if let Some(file) = current_file {
        files.push((file, current_ins, current_del));
    }

    if files.is_empty() {
        return String::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut total_ins = 0usize;
    let mut total_del = 0usize;

    // Find max filename length for alignment
    let max_name_len = files.iter().map(|(f, _, _)| f.len()).max().unwrap_or(0);

    for (file, ins, del) in &files {
        total_ins += ins;
        total_del += del;
        let total = ins + del;
        // Truncate the +/- bar to reasonable length
        let bar_len = std::cmp::min(total, 50);
        let ins_bar = std::cmp::min(*ins, bar_len);
        let del_bar = bar_len.saturating_sub(ins_bar).min(*del);
        let bar = format!("{}{}", "+".repeat(ins_bar), "-".repeat(del_bar));
        lines.push(format!(
            " {:width$} | {:>4} {}",
            file,
            total,
            bar,
            width = max_name_len
        ));
    }

    lines.push(format!(
        " {} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        total_ins,
        if total_ins == 1 { "" } else { "s" },
        total_del,
        if total_del == 1 { "" } else { "s" }
    ));

    lines.join("\n")
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

    #[test]
    fn test_derive_diff_stat_single_file() {
        let diff = r#"diff --git a/README.md b/README.md
index 1234567..abcdefg 100644
--- a/README.md
+++ b/README.md
@@ -10,7 +10,7 @@ A CLI tool for generating commit messages.
 ## Installation
 
-cargo instal ghcc
+cargo install ghcc
 
 ## Usage"#;

        let stat = derive_diff_stat(diff);
        assert!(stat.contains("README.md"));
        assert!(stat.contains("1 file"));
        assert!(stat.contains("1 insertion"));
        assert!(stat.contains("1 deletion"));
    }

    #[test]
    fn test_derive_diff_stat_multiple_files() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
index 1234567..abcdefg 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,5 @@
+use std::io;
+use std::fs;
 fn main() {
     println!("Hello");
 }
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..abcdefg 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,4 @@
-fn old_function() {
-    // removed
-}
+fn new_function() {}
"#;

        let stat = derive_diff_stat(diff);
        assert!(stat.contains("src/main.rs"));
        assert!(stat.contains("src/lib.rs"));
        assert!(stat.contains("2 files changed"));
    }

    #[test]
    fn test_derive_diff_stat_empty_diff() {
        let stat = derive_diff_stat("");
        assert!(stat.is_empty());
    }
}
