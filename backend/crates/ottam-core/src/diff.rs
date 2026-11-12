use crate::core_error::CoreError;
use crate::git::{run_git, run_git_with_allowed_exit_codes};
use ottam_protocol::{DiffFile, DiffHunk, DiffLine, DiffSnapshot, OmittedFile};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::Instant;

const MAX_UNTRACKED_DIFF_FILES: usize = 200;
const MAX_UNTRACKED_FILE_BYTES: u64 = 1_000_000;
const MAX_UNTRACKED_ENUMERATION: usize = 1_000;
const MAX_UNTRACKED_RETURNED_FILES: usize = 500;
const MAX_DIFF_FILES: usize = 500;
const MAX_DIFF_LINES_PER_FILE: usize = 5_000;
const MAX_OMITTED_FILE_RECORDS: usize = 500;

pub fn load_git_diff_snapshot(repo_path: &str) -> Result<DiffSnapshot, CoreError> {
    if repo_path.trim().is_empty() {
        return Err(
            CoreError::new("projectRootRequired", "project root is required").recoverable(true),
        );
    }

    let started = Instant::now();
    let status_output = run_git(repo_path, ["status", "--porcelain=v1", "--branch", "-uall"])?;
    let diff_output = run_tracked_diff(repo_path)?;
    let untracked_output = run_git(
        repo_path,
        ["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let (untracked_diff_output, omitted_files) = run_untracked_diffs(repo_path, &untracked_output)?;

    let snapshot = build_diff_snapshot(
        repo_path,
        &status_output,
        &(diff_output + &untracked_diff_output),
        omitted_files,
    )
    .map_err(|error| {
        CoreError::new("diffParseFailed", error)
            .with_path(repo_path)
            .recoverable(true)
    })?;
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis(),
        files = snapshot.files.len(),
        untracked_files = snapshot.untracked_files.len(),
        omitted_files = snapshot.omitted_files.len(),
        additions = snapshot.additions,
        deletions = snapshot.deletions,
        "loaded git diff snapshot"
    );
    Ok(snapshot)
}

pub fn build_diff_snapshot(
    repo_path: &str,
    status_output: &str,
    diff_output: &str,
    omitted_files: Vec<OmittedFile>,
) -> Result<DiffSnapshot, String> {
    let (branch, ahead, behind, mut untracked_files) = parse_status(status_output);
    let (mut files, additions, deletions) = parse_unified_diff(diff_output)?;
    let mut omitted_files = omitted_files;
    apply_tracked_diff_limits(&mut files, &mut omitted_files);
    apply_diff_response_limits(&mut untracked_files, &mut omitted_files);
    Ok(DiffSnapshot {
        repo_path: repo_path.to_string(),
        branch,
        ahead,
        behind,
        additions,
        deletions,
        files,
        untracked_files,
        omitted_files,
    })
}

pub fn parse_status(output: &str) -> (String, i64, i64, Vec<String>) {
    let mut branch = "unknown".to_string();
    let mut ahead = 0;
    let mut behind = 0;
    let mut untracked = Vec::new();

    for raw_line in split_lines(output) {
        let line = raw_line.trim_end_matches('\r');
        if let Some(branch_line) = line.strip_prefix("## ") {
            (branch, ahead, behind) = parse_branch_line(branch_line);
        } else if let Some(path) = line.strip_prefix("?? ") {
            untracked.push(path.trim().to_string());
        }
    }

    (branch, ahead, behind, untracked)
}

fn parse_branch_line(line: &str) -> (String, i64, i64) {
    let mut branch = line.to_string();
    let mut metadata = "";

    if let Some(marker_index) = line.find("...") {
        branch = line[..marker_index].to_string();
        if let Some(bracket_index) = line[marker_index + 3..].find('[') {
            metadata = &line[marker_index + 3 + bracket_index..];
        }
    } else if let Some(bracket_index) = line.find(" [") {
        branch = line[..bracket_index].to_string();
        metadata = &line[bracket_index..];
    }

    (
        branch.trim().to_string(),
        parse_count_metadata(metadata, "ahead"),
        parse_count_metadata(metadata, "behind"),
    )
}

fn parse_count_metadata(metadata: &str, key: &str) -> i64 {
    let Some(index) = metadata.find(key) else {
        return 0;
    };
    metadata[index + key.len()..]
        .trim_start()
        .split(|character: char| !character.is_ascii_digit())
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
}

pub fn parse_unified_diff(output: &str) -> Result<(Vec<DiffFile>, i64, i64), String> {
    let mut files = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<DiffHunk> = None;
    let mut old_line = 0;
    let mut new_line = 0;
    let mut additions = 0;
    let mut deletions = 0;

    fn flush_hunk(current_file: &mut Option<DiffFile>, current_hunk: &mut Option<DiffHunk>) {
        if let (Some(file), Some(hunk)) = (current_file.as_mut(), current_hunk.take()) {
            file.hunks.push(hunk);
        }
    }

    fn flush_file(
        files: &mut Vec<DiffFile>,
        current_file: &mut Option<DiffFile>,
        current_hunk: &mut Option<DiffHunk>,
    ) {
        flush_hunk(current_file, current_hunk);
        if let Some(file) = current_file.take() {
            files.push(file);
        }
    }

    for raw_line in split_lines(output) {
        let line = raw_line.trim_end_matches('\r');

        if line.starts_with("diff --git ") {
            flush_file(&mut files, &mut current_file, &mut current_hunk);
            current_file = Some(parse_diff_git_line(line));
            continue;
        }

        let Some(file) = current_file.as_mut() else {
            continue;
        };

        if line.starts_with("new file mode ") {
            file.status = "added".to_string();
            append_metadata_line(&mut current_hunk, line);
        } else if line.starts_with("deleted file mode ") {
            file.status = "deleted".to_string();
            append_metadata_line(&mut current_hunk, line);
        } else if let Some(old_path) = line.strip_prefix("rename from ") {
            file.status = "renamed".to_string();
            file.old_path =
                Some(decode_git_path_token(old_path).unwrap_or_else(|| old_path.to_string()));
            append_metadata_line(&mut current_hunk, line);
        } else if let Some(new_path) = line.strip_prefix("rename to ") {
            file.path = decode_git_path_token(new_path).unwrap_or_else(|| new_path.to_string());
            append_metadata_line(&mut current_hunk, line);
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        } else if line.starts_with("@@ ") {
            flush_hunk(&mut current_file, &mut current_hunk);
            let (hunk, parsed_old_line, parsed_new_line) = parse_hunk_header(line)?;
            current_hunk = Some(hunk);
            old_line = parsed_old_line;
            new_line = parsed_new_line;
        } else if current_hunk.is_none() {
            append_metadata_line(&mut current_hunk, line);
        } else if line.starts_with(r"\ No newline at end of file") {
            push_hunk_line(
                &mut current_hunk,
                DiffLine {
                    kind: "note".to_string(),
                    old_line: None,
                    new_line: None,
                    content: line.trim_start_matches(r"\ ").to_string(),
                },
            );
        } else if line.starts_with('+') && !line.starts_with("+++ ") {
            push_hunk_line(
                &mut current_hunk,
                DiffLine {
                    kind: "addition".to_string(),
                    old_line: None,
                    new_line: Some(new_line),
                    content: line.trim_start_matches('+').to_string(),
                },
            );
            file.additions += 1;
            additions += 1;
            new_line += 1;
        } else if line.starts_with('-') && !line.starts_with("--- ") {
            push_hunk_line(
                &mut current_hunk,
                DiffLine {
                    kind: "deletion".to_string(),
                    old_line: Some(old_line),
                    new_line: None,
                    content: line.trim_start_matches('-').to_string(),
                },
            );
            file.deletions += 1;
            deletions += 1;
            old_line += 1;
        } else {
            let content = line.strip_prefix(' ').unwrap_or(line).to_string();
            push_hunk_line(
                &mut current_hunk,
                DiffLine {
                    kind: "context".to_string(),
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    content,
                },
            );
            old_line += 1;
            new_line += 1;
        }
    }

    flush_file(&mut files, &mut current_file, &mut current_hunk);
    Ok((files, additions, deletions))
}

fn parse_diff_git_line(line: &str) -> DiffFile {
    let (old_path, path) = parse_diff_git_paths(line)
        .map(|(old_path, new_path)| (strip_diff_prefix(&old_path), strip_diff_prefix(&new_path)))
        .unwrap_or_else(|| (line.to_string(), line.to_string()));

    DiffFile {
        path,
        old_path: Some(old_path),
        status: "modified".to_string(),
        additions: 0,
        deletions: 0,
        hunks: Vec::new(),
    }
}

fn parse_diff_git_paths(line: &str) -> Option<(String, String)> {
    let input = line.strip_prefix("diff --git ")?;
    let tokens = split_diff_path_tokens(input)?;
    if tokens.len() == 2 {
        return Some((tokens[0].clone(), tokens[1].clone()));
    }
    parse_unquoted_diff_git_paths(input)
}

fn split_diff_path_tokens(mut input: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    while !input.is_empty() {
        input = input.trim_start_matches(' ');
        if input.is_empty() {
            break;
        }

        if input.starts_with('"') {
            let (token, rest) = consume_quoted_token(input)?;
            tokens.push(token);
            input = rest;
            continue;
        }

        if let Some(next_space) = input.find(' ') {
            tokens.push(input[..next_space].to_string());
            input = &input[next_space + 1..];
        } else {
            tokens.push(input.to_string());
            break;
        }
    }
    Some(tokens)
}

fn consume_quoted_token(input: &str) -> Option<(String, &str)> {
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }

    let mut token = Vec::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let decoded = String::from_utf8_lossy(&token).to_string();
                return Some((decoded, &input[index + 1..]));
            }
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    return None;
                }
                let escaped = bytes[index];
                if escaped.is_ascii_digit() && escaped < b'8' {
                    let mut value = 0u8;
                    let mut digits = 0;
                    while index < bytes.len()
                        && digits < 3
                        && bytes[index].is_ascii_digit()
                        && bytes[index] < b'8'
                    {
                        value = value
                            .saturating_mul(8)
                            .saturating_add(bytes[index].saturating_sub(b'0'));
                        index += 1;
                        digits += 1;
                    }
                    token.push(value);
                    continue;
                }
                token.push(match escaped {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b'n' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'v' => 0x0b,
                    b'\\' => b'\\',
                    b'"' => b'"',
                    other => other,
                });
            }
            byte => token.push(byte),
        }
        index += 1;
    }
    None
}

fn parse_unquoted_diff_git_paths(input: &str) -> Option<(String, String)> {
    if !input.starts_with("a/") {
        return None;
    }
    let separator_index = input.find(" b/")?;
    Some((
        input[..separator_index].to_string(),
        input[separator_index + 1..].to_string(),
    ))
}

fn strip_diff_prefix(path: &str) -> String {
    path.trim_matches('"')
        .trim_start_matches("a/")
        .trim_start_matches("b/")
        .to_string()
}

fn decode_git_path_token(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.starts_with('"') {
        return consume_quoted_token(trimmed).map(|(path, _)| strip_diff_prefix(&path));
    }
    Some(strip_diff_prefix(trimmed))
}

fn apply_tracked_diff_limits(files: &mut Vec<DiffFile>, omitted_files: &mut Vec<OmittedFile>) {
    let original_len = files.len();
    if original_len > MAX_DIFF_FILES {
        files.truncate(MAX_DIFF_FILES);
        push_omitted_file(
            omitted_files,
            format!("{} additional tracked files", original_len - MAX_DIFF_FILES),
            "tracked diff file limit exceeded",
            None,
        );
    }

    for file in files {
        let mut remaining = MAX_DIFF_LINES_PER_FILE;
        let mut truncated = false;
        for hunk in &mut file.hunks {
            if hunk.lines.len() <= remaining {
                remaining -= hunk.lines.len();
                continue;
            }
            hunk.lines.truncate(remaining);
            remaining = 0;
            truncated = true;
        }
        if truncated {
            push_omitted_file(
                omitted_files,
                file.path.clone(),
                "tracked diff line limit exceeded",
                None,
            );
        }
    }
}

fn apply_diff_response_limits(
    untracked_files: &mut Vec<String>,
    omitted_files: &mut Vec<OmittedFile>,
) {
    let original_untracked_len = untracked_files.len();
    if original_untracked_len > MAX_UNTRACKED_RETURNED_FILES {
        untracked_files.truncate(MAX_UNTRACKED_RETURNED_FILES);
        push_omitted_file(
            omitted_files,
            format!(
                "{} additional untracked files",
                original_untracked_len - MAX_UNTRACKED_RETURNED_FILES
            ),
            "untracked filename response limit exceeded",
            None,
        );
    }

    let original_omitted_len = omitted_files.len();
    if original_omitted_len > MAX_OMITTED_FILE_RECORDS {
        omitted_files.truncate(MAX_OMITTED_FILE_RECORDS);
        push_omitted_file(
            omitted_files,
            format!(
                "{} additional omitted files",
                original_omitted_len - MAX_OMITTED_FILE_RECORDS
            ),
            "omitted file response limit exceeded",
            None,
        );
    }
}

fn push_omitted_file(
    omitted_files: &mut Vec<OmittedFile>,
    path: impl Into<String>,
    reason: impl Into<String>,
    byte_count: Option<i64>,
) {
    omitted_files.push(OmittedFile {
        path: path.into(),
        reason: reason.into(),
        byte_count,
    });
}

fn parse_hunk_header(line: &str) -> Result<(DiffHunk, i64, i64), String> {
    let (old_part, rest) = line
        .strip_prefix("@@ -")
        .and_then(|value| value.split_once(" +"))
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let (new_part, _) = rest
        .split_once(" @@")
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;

    let (old_start, old_lines) = parse_hunk_range(old_part)?;
    let (new_start, new_lines) = parse_hunk_range(new_part)?;
    Ok((
        DiffHunk {
            kind: "diff".to_string(),
            header: line.to_string(),
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        },
        old_start,
        new_start,
    ))
}

fn parse_hunk_range(value: &str) -> Result<(i64, i64), String> {
    if let Some((start, count)) = value.split_once(',') {
        Ok((
            start
                .parse()
                .map_err(|_| format!("invalid hunk range: {value}"))?,
            count.parse().unwrap_or(0),
        ))
    } else {
        Ok((
            value
                .parse()
                .map_err(|_| format!("invalid hunk range: {value}"))?,
            1,
        ))
    }
}

fn append_metadata_line(current_hunk: &mut Option<DiffHunk>, line: &str) {
    let hunk = current_hunk.get_or_insert_with(|| DiffHunk {
        kind: "metadata".to_string(),
        header: "file metadata".to_string(),
        old_start: 0,
        old_lines: 0,
        new_start: 0,
        new_lines: 0,
        lines: Vec::new(),
    });
    hunk.lines.push(DiffLine {
        kind: "metadata".to_string(),
        old_line: None,
        new_line: None,
        content: line.to_string(),
    });
}

fn push_hunk_line(current_hunk: &mut Option<DiffHunk>, line: DiffLine) {
    if let Some(hunk) = current_hunk.as_mut() {
        hunk.lines.push(line);
    }
}

fn split_lines(output: &str) -> Vec<&str> {
    let output = output.strip_suffix('\n').unwrap_or(output);
    if output.is_empty() {
        Vec::new()
    } else {
        output.split('\n').collect()
    }
}

fn run_tracked_diff(repo_path: &str) -> Result<String, CoreError> {
    if has_git_head(repo_path) {
        return run_git(repo_path, ["diff", "--no-ext-diff", "HEAD", "--"]);
    }
    let staged = run_git(repo_path, ["diff", "--no-ext-diff", "--cached", "--"])?;
    let unstaged = run_git(repo_path, ["diff", "--no-ext-diff", "--"])?;
    Ok(staged + &unstaged)
}

fn has_git_head(repo_path: &str) -> bool {
    run_git(repo_path, ["rev-parse", "--verify", "HEAD"]).is_ok()
}

fn run_untracked_diffs(
    repo_path: &str,
    nul_separated_paths: &str,
) -> Result<(String, Vec<OmittedFile>), CoreError> {
    let mut output = String::new();
    let mut omitted_files = Vec::new();
    let mut rendered_count = 0;

    for (index, path) in nul_separated_paths
        .split('\0')
        .filter(|path| !path.is_empty())
        .enumerate()
    {
        if index >= MAX_UNTRACKED_ENUMERATION {
            push_omitted_file(
                &mut omitted_files,
                "additional untracked files",
                "untracked enumeration limit exceeded",
                None,
            );
            break;
        }

        if rendered_count >= MAX_UNTRACKED_DIFF_FILES {
            push_omitted_file(
                &mut omitted_files,
                path,
                "untracked file limit exceeded",
                None,
            );
            continue;
        }

        let (byte_count, omit_reason) = omitted_untracked_reason(repo_path, path);
        if let Some(reason) = omit_reason {
            push_omitted_file(&mut omitted_files, path, reason, byte_count);
            continue;
        }

        let diff = run_git_diff_no_index(repo_path, path)?;
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&diff);
        rendered_count += 1;
    }

    Ok((output, omitted_files))
}

fn omitted_untracked_reason(repo_path: &str, path: &str) -> (Option<i64>, Option<String>) {
    let full_path = Path::new(repo_path).join(path);
    let Ok(metadata) = fs::symlink_metadata(&full_path) else {
        return (None, Some("stat failed".to_string()));
    };
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return (None, Some("directory".to_string()));
    }
    if !file_type.is_file() {
        return (None, Some("special file".to_string()));
    }
    let size = metadata.len();
    if size > MAX_UNTRACKED_FILE_BYTES {
        return (Some(size as i64), Some("file is too large".to_string()));
    }
    if is_binary_file(&full_path).unwrap_or(false) {
        return (Some(size as i64), Some("binary file".to_string()));
    }
    (Some(size as i64), None)
}

fn is_binary_file(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 8000];
    let count = file.read(&mut buffer)?;
    Ok(buffer[..count].contains(&0))
}

fn run_git_diff_no_index(repo_path: &str, path: &str) -> Result<String, CoreError> {
    let started = Instant::now();
    let output = run_git_with_allowed_exit_codes(
        repo_path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-index",
            "--",
            "/dev/null",
            path,
        ],
        &[0, 1],
    )?;

    tracing::debug!(
        exit_status = ?output.status_code,
        stdout_bytes = output.stdout.len(),
        stderr_bytes = output.stderr.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "git diff --no-index completed"
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn parses_modified_file_with_one_hunk() {
        let diff = r#"diff --git a/file.txt b/file.txt
index 83db48f..f735c2d 100644
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
 one
-two
+deux
 three
"#;

        let (files, additions, deletions) = parse_unified_diff(diff).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "file.txt");
        assert_eq!(files[0].status, "modified");
        assert_eq!(additions, 1);
        assert_eq!(deletions, 1);
        assert_eq!(files[0].additions, 1);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[0].hunks[1].lines[1].kind, "deletion");
        assert_eq!(files[0].hunks[1].lines[2].kind, "addition");
    }

    #[test]
    fn parses_added_deleted_multiple_hunks_and_newline_notes() {
        let added = r#"diff --git a/added.txt b/added.txt
new file mode 100644
index 0000000..7898192
--- /dev/null
+++ b/added.txt
@@ -0,0 +1,2 @@
+hello
+world
"#;
        let (files, additions, deletions) = parse_unified_diff(added).unwrap();
        assert_eq!(files[0].status, "added");
        assert_eq!((additions, deletions), (2, 0));

        let deleted = r#"diff --git a/deleted.txt b/deleted.txt
deleted file mode 100644
index 7898192..0000000
--- a/deleted.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-hello
-world
"#;
        let (files, additions, deletions) = parse_unified_diff(deleted).unwrap();
        assert_eq!(files[0].status, "deleted");
        assert_eq!((additions, deletions), (0, 2));

        let multiple = r#"diff --git a/file.txt b/file.txt
index 1c9a171..e6c7c3b 100644
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
-one
+uno
 two
 three
@@ -8,3 +8,3 @@
 eight
-nine
+nueve
 ten
"#;
        let (files, additions, deletions) = parse_unified_diff(multiple).unwrap();
        assert_eq!(files[0].hunks.len(), 3);
        assert_eq!((additions, deletions), (2, 2));

        let no_newline = r#"diff --git a/file.txt b/file.txt
index 257cc56..571f0d3 100644
--- a/file.txt
+++ b/file.txt
@@ -1 +1 @@
-old
\ No newline at end of file
+new
\ No newline at end of file
"#;
        let (files, _, _) = parse_unified_diff(no_newline).unwrap();
        let note_count = files[0].hunks[1]
            .lines
            .iter()
            .filter(|line| line.kind == "note")
            .count();
        assert_eq!(note_count, 2);
    }

    #[test]
    fn parses_status_and_quoted_diff_paths() {
        let status = "## main...origin/main [ahead 1, behind 2]\n M tracked.txt\n?? scratch.txt\n";
        let snapshot = build_diff_snapshot("/repo", status, "", Vec::new()).unwrap();
        assert_eq!(snapshot.branch, "main");
        assert_eq!(snapshot.ahead, 1);
        assert_eq!(snapshot.behind, 2);
        assert_eq!(snapshot.untracked_files, vec!["scratch.txt"]);

        let diff = r#"diff --git "a/dir/file name.swift" "b/dir/file name.swift"
index 83db48f..f735c2d 100644
--- "a/dir/file name.swift"
+++ "b/dir/file name.swift"
@@ -1 +1 @@
-old
+new
"#;
        let (files, _, _) = parse_unified_diff(diff).unwrap();
        assert_eq!(files[0].path, "dir/file name.swift");
        assert_eq!(files[0].old_path.as_deref(), Some("dir/file name.swift"));
    }

    #[test]
    fn load_snapshot_includes_untracked_file_contents_and_empty_arrays() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        run_test_git(repo.path(), ["commit", "--allow-empty", "-m", "initial"]);
        fs::write(repo.path().join("new file.txt"), "hello\nworld\n").unwrap();

        let snapshot = load_git_diff_snapshot(repo.path().to_str().unwrap()).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "new file.txt");
        assert_eq!(snapshot.files[0].status, "added");
        assert_eq!(snapshot.files[0].additions, 2);
        assert_eq!(snapshot.additions, 2);

        fs::remove_file(repo.path().join("new file.txt")).unwrap();
        let empty_snapshot = load_git_diff_snapshot(repo.path().to_str().unwrap()).unwrap();
        let encoded = serde_json::to_string(&empty_snapshot).unwrap();
        assert!(encoded.contains(r#""files":[]"#));
        assert!(encoded.contains(r#""untrackedFiles":[]"#));
        assert!(encoded.contains(r#""omittedFiles":[]"#));
    }

    #[test]
    fn load_snapshot_works_without_commits_and_omits_binary_files() {
        let repo = TempDir::new().unwrap();
        run_test_git(repo.path(), ["init"]);
        fs::write(repo.path().join("first.txt"), "first\n").unwrap();
        fs::write(repo.path().join("binary.bin"), b"\0binary").unwrap();

        let snapshot = load_git_diff_snapshot(repo.path().to_str().unwrap()).unwrap();
        assert!(snapshot
            .files
            .iter()
            .any(|file| file.path == "first.txt" && file.status == "added"));
        assert!(snapshot
            .omitted_files
            .iter()
            .any(|file| file.path == "binary.bin" && file.reason == "binary file"));
    }

    fn init_repo(path: &std::path::Path) {
        run_test_git(path, ["init"]);
        run_test_git(path, ["config", "user.name", "Ottam Test"]);
        run_test_git(path, ["config", "user.email", "test@example.com"]);
    }

    fn run_test_git<const N: usize>(repo_path: &std::path::Path, args: [&str; N]) {
        let output = Command::new("/usr/bin/git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
