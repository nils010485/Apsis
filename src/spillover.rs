use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;

use crate::error::Result;

/// Default inline threshold: 50 KiB.
pub const DEFAULT_MAX_INLINE_BYTES: usize = 50 * 1024;
pub const PREVIEW_CHAR_LIMIT: usize = 1000;

/// Result of applying the inline-size threshold to a serialized tool payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpilloverResult {
    Inline(String),
    Spilled {
        file_path: PathBuf,
        total_bytes: usize,
        preview: String,
    },
}

#[derive(Serialize)]
struct SpilledEnvelope<'a> {
    spilled: bool,
    file_path: String,
    total_bytes: usize,
    preview: &'a str,
}

/// Resolve the maximum inline byte threshold from `APSIS_MAX_INLINE_BYTES` or default (50 KiB).
pub fn max_inline_bytes() -> usize {
    std::env::var("APSIS_MAX_INLINE_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_INLINE_BYTES)
}

/// Apply the spillover threshold, writing full content to `/tmp/apsis-<preset>-<timestamp>.<ext>`
/// if it exceeds `threshold_bytes`.
pub fn apply_spillover_with_dir(
    preset: &str,
    content: &str,
    ext: &str,
    threshold_bytes: usize,
    output_dir: &Path,
) -> Result<SpilloverResult> {
    let total_bytes = content.len();
    if total_bytes <= threshold_bytes {
        return Ok(SpilloverResult::Inline(content.to_string()));
    }

    let safe_preset: String = preset
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe_ext = ext.trim_start_matches('.');
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.6fZ");
    let filename = format!("apsis-{safe_preset}-{timestamp}.{safe_ext}");
    let file_path = output_dir.join(filename);

    fs::write(&file_path, content.as_bytes())?;

    let preview: String = content.chars().take(PREVIEW_CHAR_LIMIT).collect();
    Ok(SpilloverResult::Spilled {
        file_path,
        total_bytes,
        preview,
    })
}

/// Apply spillover using `/tmp` and `APSIS_MAX_INLINE_BYTES` (default 50 KiB), formatting
/// the result as text ready for an MCP `ContentBlock::text` response.
pub fn format_mcp_output(preset: &str, content: &str, ext: &str) -> Result<String> {
    let dir = PathBuf::from("/tmp");
    let outcome = apply_spillover_with_dir(preset, content, ext, max_inline_bytes(), &dir)?;
    match outcome {
        SpilloverResult::Inline(text) => Ok(text),
        SpilloverResult::Spilled {
            file_path,
            total_bytes,
            preview,
        } => {
            let envelope = SpilledEnvelope {
                spilled: true,
                file_path: file_path.display().to_string(),
                total_bytes,
                preview: &preview,
            };
            Ok(serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| {
                format!(
                    "Output written to {} ({total_bytes} bytes)\n\n{preview}",
                    file_path.display()
                )
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_when_within_threshold() {
        let dir = std::env::temp_dir();
        let res = apply_spillover_with_dir("dev-pg", "hello", "json", 10, &dir).unwrap();
        assert_eq!(res, SpilloverResult::Inline("hello".to_string()));
    }

    #[test]
    fn spills_when_exceeding_threshold_and_truncates_utf8_safely() {
        let dir = std::env::temp_dir();
        let data = "é".repeat(1200);
        let res = apply_spillover_with_dir("dev/pg", &data, "txt", 100, &dir).unwrap();
        match res {
            SpilloverResult::Spilled {
                file_path,
                total_bytes,
                preview,
            } => {
                assert_eq!(total_bytes, data.len());
                assert_eq!(preview.chars().count(), PREVIEW_CHAR_LIMIT);
                assert!(
                    file_path
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("apsis-dev_pg-")
                );
                assert!(file_path.extension().unwrap() == "txt");
                let written = fs::read_to_string(&file_path).unwrap();
                assert_eq!(written, data);
                let _ = fs::remove_file(&file_path);
            }
            SpilloverResult::Inline(_) => panic!("expected spillover"),
        }
    }
}
