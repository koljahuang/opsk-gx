//! GitHub source code integration for code-level RCA.
//!
//! Extracts file references from Python tracebacks and fetches source code
//! from GitHub repositories to enable AI root-cause analysis down to exact lines.

use std::sync::LazyLock;

use regex::Regex;
use reqwest::Client;

static TRACEBACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"File "([^"]+)", line (\d+)(?:, in (\w+))?"#).unwrap());

/// A reference to a source file extracted from a traceback.
#[derive(Debug, Clone)]
pub struct FileRef {
    pub file_path: String,
    pub line_number: usize,
    pub function_name: Option<String>,
}

/// Extract file references from a Python traceback string.
///
/// Matches patterns like: `File "main.py", line 119, in calculate_discount`
/// and also: `File "/app/main.py", line 187, in process_order`
///
/// Returns deduplicated references ordered by first appearance.
pub fn extract_file_refs(traceback: &str) -> Vec<FileRef> {
    let mut refs = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for cap in TRACEBACK_RE.captures_iter(traceback) {
        let raw_path = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let line_num: usize = cap.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let func_name = cap.get(3).map(|m| m.as_str().to_string());

        // Normalize path: strip leading /app/ or ./ prefix
        let file_path = raw_path
            .trim_start_matches("/app/")
            .trim_start_matches("./")
            .to_string();

        // Skip stdlib / site-packages
        if file_path.contains("site-packages") || file_path.starts_with("/usr/") || file_path.starts_with("/lib/") {
            continue;
        }

        let key = format!("{}:{}", file_path, line_num);
        if seen.insert(key) {
            refs.push(FileRef {
                file_path,
                line_number: line_num,
                function_name: func_name,
            });
        }
    }

    refs
}

/// Fetch a source file from GitHub via the Contents API.
///
/// Uses `application/vnd.github.v3.raw` to get raw file content directly.
/// `repo` should be in "owner/repo" format, `path` is the file path within the repo.
pub async fn fetch_source_file(repo: &str, path: &str, token: &str) -> Result<String, String> {
    let url = format!(
        "https://api.github.com/repos/{}/contents/{}",
        repo,
        path.trim_start_matches('/')
    );

    let client = Client::new();
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github.v3.raw")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "opsk-rca")
        .send()
        .await
        .map_err(|e| format!("GitHub API request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned HTTP {} for {}", resp.status(), path));
    }

    resp.text()
        .await
        .map_err(|e| format!("Failed to read GitHub response: {}", e))
}

/// Fetch source code for all file refs and return a formatted context string.
///
/// For each referenced file, fetches the full source and extracts a window of
/// ±10 lines around the referenced line number.
pub async fn fetch_source_context(repo: &str, token: &str, file_refs: &[FileRef]) -> String {
    if file_refs.is_empty() {
        return String::new();
    }

    let mut sections = Vec::new();
    let mut fetched_files: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();

    for fref in file_refs {
        // Resolve to likely app path (the demo app lives under app/)
        let paths_to_try = vec![format!("app/{}", fref.file_path), fref.file_path.clone()];

        let source = if let Some(cached) = fetched_files.get(&fref.file_path) {
            cached.clone()
        } else {
            let mut result = None;
            for path in &paths_to_try {
                match fetch_source_file(repo, path, token).await {
                    Ok(content) => {
                        result = Some(content);
                        break;
                    }
                    Err(_) => continue,
                }
            }
            fetched_files.insert(fref.file_path.clone(), result.clone());
            result
        };

        if let Some(ref content) = source {
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len();

            // Extract window around the referenced line
            let target = fref.line_number.saturating_sub(1); // 0-indexed
            let start = target.saturating_sub(10);
            let end = (target + 11).min(total_lines);

            let mut snippet = format!(
                "### {} (line {}, {} total lines)",
                fref.file_path, fref.line_number, total_lines
            );
            if let Some(ref func) = fref.function_name {
                snippet.push_str(&format!(" — in `{}`", func));
            }
            snippet.push('\n');
            snippet.push_str("```python\n");

            for (i, line) in lines[start..end].iter().enumerate() {
                let line_num = start + i + 1;
                let marker = if line_num == fref.line_number { ">>>" } else { "   " };
                snippet.push_str(&format!("{} {:>4} | {}\n", marker, line_num, line));
            }
            snippet.push_str("```\n");
            sections.push(snippet);
        }
    }

    if sections.is_empty() {
        "(Could not fetch source code from GitHub)".to_string()
    } else {
        sections.join("\n")
    }
}

/// Fetch a file from GitHub and extract a specific line range.
/// If start/end are 0, returns the entire file.
pub async fn fetch_source_range(
    repo: &str,
    token: &str,
    file_path: &str,
    start_line: usize,
    end_line: usize,
) -> Result<String, String> {
    // Strip repo name prefix if AI included it (e.g. "rca-app-in-k8s/main.py" → "main.py")
    let repo_name = repo.rsplit('/').next().unwrap_or("");
    let cleaned = file_path
        .trim_start_matches('/')
        .strip_prefix(repo_name)
        .map(|s| s.trim_start_matches('/'))
        .unwrap_or(file_path);

    let paths_to_try = [cleaned.to_string(), format!("app/{}", cleaned), file_path.to_string()];

    let mut last_err = String::new();
    let mut content = None;
    for path in &paths_to_try {
        match fetch_source_file(repo, path, token).await {
            Ok(c) => {
                content = Some(c);
                break;
            }
            Err(e) => {
                last_err = e;
            }
        }
    }
    let content = content.ok_or(last_err)?;
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();

    if start_line == 0 && end_line == 0 {
        return Ok(format!("// {} ({} lines)\n{}", file_path, total, content));
    }

    let start = start_line.saturating_sub(1).min(total);
    let end = end_line.min(total);
    if start >= end {
        return Err(format!(
            "Invalid range {}-{} for file with {} lines",
            start_line, end_line, total
        ));
    }

    let mut out = format!("// {} lines {}-{} (of {})\n", file_path, start_line, end_line, total);
    for (i, line) in lines[start..end].iter().enumerate() {
        let num = start + i + 1;
        out.push_str(&format!("{:>4} | {}\n", num, line));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_refs_basic() {
        let traceback = r#"Traceback (most recent call last):
  File "main.py", line 187, in process_order
    discounted = calculate_discount(base_amount * quantity, tier)
  File "main.py", line 119, in calculate_discount
    final_price = amount * 100 / (100 - discount_pct)
ZeroDivisionError: division by zero"#;

        let refs = extract_file_refs(traceback);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].file_path, "main.py");
        assert_eq!(refs[0].line_number, 187);
        assert_eq!(refs[0].function_name.as_deref(), Some("process_order"));
        assert_eq!(refs[1].file_path, "main.py");
        assert_eq!(refs[1].line_number, 119);
        assert_eq!(refs[1].function_name.as_deref(), Some("calculate_discount"));
    }

    #[test]
    fn test_extract_file_refs_with_app_prefix() {
        let traceback = r#"File "/app/main.py", line 42, in validate_sku"#;
        let refs = extract_file_refs(traceback);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file_path, "main.py");
        assert_eq!(refs[0].line_number, 42);
    }

    #[test]
    fn test_extract_file_refs_skips_stdlib() {
        let traceback = r#"File "/usr/lib/python3.11/http/server.py", line 100, in handle
  File "main.py", line 50, in process"#;
        let refs = extract_file_refs(traceback);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file_path, "main.py");
    }

    #[test]
    fn test_extract_file_refs_empty() {
        let refs = extract_file_refs("no traceback here");
        assert!(refs.is_empty());
    }
}
