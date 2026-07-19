use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use super::{classify_snapshot, Provider, ProviderFetch, UsageSnapshot, UsageWindow};
use crate::secrets::Secrets;

const PROVIDER_LABEL: &str = "Claude Code";
const PROVIDER_ID: &str = "claude";

/// Only the trailing 7d window is needed. Cap first-read / cold-start tails so
/// multi-hundred-MB session logs do not get fully loaded every tick.
const MAX_TAIL_BYTES: u64 = 4 * 1024 * 1024;

pub struct ClaudeProvider;

#[async_trait]
impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn label(&self) -> &'static str {
        PROVIDER_LABEL
    }

    async fn fetch(&self, _secrets: &Secrets) -> ProviderFetch {
        match tokio::task::spawn_blocking(read_claude_snapshot).await {
            Ok(Ok(snap)) => classify_snapshot(snap),
            Ok(Err(msg)) => classify_snapshot(UsageSnapshot::unavailable(PROVIDER_LABEL, msg)),
            Err(e) => classify_snapshot(UsageSnapshot::unavailable(
                PROVIDER_LABEL,
                format!("join: {e}"),
            )),
        }
    }
}

#[derive(Clone, Copy)]
struct FileCursor {
    /// Byte offset of the next unread byte (equal to last known length when fully read).
    offset: u64,
    /// Last observed file length — shrinks trigger a rescan of that file's tail.
    len: u64,
}

struct UsageEvent {
    ts: DateTime<Utc>,
    tokens: u64,
    /// Normalized model family ("Opus" / "Sonnet" / "Haiku") when the log
    /// line names a known model; None for unrecognized model strings.
    family: Option<&'static str>,
}

struct ClaudeCache {
    files: HashMap<PathBuf, FileCursor>,
    events: Vec<UsageEvent>,
}

fn claude_cache() -> &'static Mutex<ClaudeCache> {
    static CACHE: OnceLock<Mutex<ClaudeCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(ClaudeCache {
            files: HashMap::new(),
            events: Vec::new(),
        })
    })
}

/// Token counts come from per-message `usage` blocks in
/// `~/.claude/projects/**/*.jsonl`. The Claude CLI does not publish rate
/// limits, so we surface raw trailing-window totals (popup-only, since
/// there is no % to plot). `history.jsonl` is just user prompt titles, no
/// usage data — we skip it.
///
/// Reads are incremental: each file keeps a byte cursor, and only appended
/// bytes are parsed on subsequent ticks. Events older than 7 days are pruned.
fn read_claude_snapshot() -> Result<UsageSnapshot, String> {
    let home = dirs::home_dir().ok_or("no home directory")?;
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.exists() {
        // Soft empty state — surface only in the popup so the bar segment
        // doesn't go red for users without a ~/.claude directory.
        if let Ok(mut cache) = claude_cache().lock() {
            cache.files.clear();
            cache.events.clear();
        }
        return Ok(UsageSnapshot {
            provider: PROVIDER_LABEL.to_string(),
            level: None,
            windows: Vec::new(),
            unavailable_reason: Some("no local claude data".into()),
            fetched_at: Utc::now(),
        });
    }

    let files = find_jsonl_files(&projects_dir, 3);
    if files.is_empty() {
        if let Ok(mut cache) = claude_cache().lock() {
            cache.files.clear();
            cache.events.clear();
        }
        return Ok(soft_empty("no local claude data"));
    }

    let now = Utc::now();
    let w5h = now - Duration::hours(5);
    let w7d = now - Duration::days(7);

    let mut cache = claude_cache()
        .lock()
        .map_err(|_| "claude cache lock poisoned".to_string())?;

    // Drop cursors for files that disappeared.
    let live: std::collections::HashSet<PathBuf> = files.iter().cloned().collect();
    cache.files.retain(|path, _| live.contains(path));

    // Truncation / rewrite would double-count if we only re-tailed that file while
    // keeping old events. Full rebuild is rare and still tail-capped.
    let truncated = files.iter().any(|path| {
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        match cache.files.get(path) {
            Some(c) => meta.len() < c.len,
            None => false,
        }
    });
    if truncated {
        cache.files.clear();
        cache.events.clear();
    }

    for path in &files {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let len = meta.len();
        let cursor = cache.files.get(path).copied();

        let start = match cursor {
            Some(c) if c.len == len && c.offset == len => {
                // Unchanged — skip.
                continue;
            }
            Some(c) if len >= c.len && c.offset <= len => {
                // Append-only growth (or same len with unread gap): read suffix.
                c.offset
            }
            Some(_) | None => {
                // New file or first sight — tail-cap cold start so we never
                // re-slurp multi-hundred-MB histories in full.
                if len > MAX_TAIL_BYTES {
                    len - MAX_TAIL_BYTES
                } else {
                    0
                }
            }
        };

        if start >= len {
            cache.files.insert(path.clone(), FileCursor { offset: len, len });
            continue;
        }

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut buf = String::new();
        if file.read_to_string(&mut buf).is_err() {
            continue;
        }

        // When starting mid-file (tail cap), drop a partial first line.
        let text = if start > 0 {
            match buf.find('\n') {
                Some(i) => &buf[i + 1..],
                None => "",
            }
        } else {
            buf.as_str()
        };

        for line in text.lines() {
            if let Some(ev) = parse_usage_line(line) {
                if ev.ts >= w7d {
                    cache.events.push(ev);
                }
            }
        }

        cache.files.insert(
            path.clone(),
            FileCursor {
                offset: len,
                len,
            },
        );
    }

    // Age out events that fell outside the 7-day window.
    cache.events.retain(|e| e.ts >= w7d);

    let mut t5h: u64 = 0;
    let mut t7d: u64 = 0;
    let mut oldest_5h: Option<DateTime<Utc>> = None;
    let mut oldest_7d: Option<DateTime<Utc>> = None;
    let mut hits = 0usize;
    // Per-model 7d token totals, keyed by normalized family name.
    let mut by_family: HashMap<&'static str, u64> = HashMap::new();

    for ev in &cache.events {
        hits += 1;
        if ev.ts >= w5h {
            t5h += ev.tokens;
            oldest_5h = Some(oldest_5h.map_or(ev.ts, |prev| prev.min(ev.ts)));
        }
        if ev.ts >= w7d {
            t7d += ev.tokens;
            oldest_7d = Some(oldest_7d.map_or(ev.ts, |prev| prev.min(ev.ts)));
            if let Some(family) = ev.family {
                *by_family.entry(family).or_insert(0) += ev.tokens;
            }
        }
    }

    // Drop the lock before building the snapshot (no need to hold it).
    drop(cache);

    if hits == 0 {
        return Ok(soft_empty("no local claude data"));
    }

    let mut windows: Vec<UsageWindow> = Vec::new();
    if t5h > 0 {
        windows.push(UsageWindow {
            label: format!("5h · {}K", t5h / 1000),
            used_percent: 0.0,
            reset_at: oldest_5h.map(|t| t + Duration::hours(5)),
            bar_visible: false,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        });
    }
    if t7d > 0 {
        windows.push(UsageWindow {
            label: format!("7d · {}M", t7d / 1_000_000),
            used_percent: 0.0,
            reset_at: oldest_7d.map(|t| t + Duration::days(7)),
            bar_visible: false,
            is_unlimited: false,
            used_absolute: None,
            limit_absolute: None,
        });
        // Per-model split (Opus vs Sonnet vs Haiku), heaviest first.
        let mut families: Vec<(&'static str, u64)> = by_family.into_iter().collect();
        families.sort_by(|a, b| b.1.cmp(&a.1));
        for (family, tokens) in families {
            if tokens == 0 {
                continue;
            }
            windows.push(UsageWindow {
                label: format!("7d {family} · {}", format_token_count(tokens)),
                used_percent: 0.0,
                reset_at: oldest_7d.map(|t| t + Duration::days(7)),
                bar_visible: false,
                is_unlimited: false,
                used_absolute: Some(tokens as f64),
                limit_absolute: None,
            });
        }
    }

    if windows.is_empty() {
        // Had older history but nothing in the trailing windows.
        return Ok(soft_empty("no recent usage"));
    }

    Ok(UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: None,
        windows,
        unavailable_reason: None,
        fetched_at: Utc::now(),
    })
}

fn parse_usage_line(line: &str) -> Option<UsageEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let entry: SessionEntry = serde_json::from_str(line).ok()?;
    // Only assistant messages carry `usage`.
    let message = entry.message.as_ref()?;
    if message.role.as_deref() != Some("assistant") {
        return None;
    }
    let usage = message.usage.as_ref()?;
    let total = usage.input_tokens.unwrap_or(0)
        + usage.output_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0);
    if total == 0 {
        return None;
    }
    let ts = entry
        .timestamp
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))?;
    let family = message.model.as_deref().and_then(model_family);
    Some(UsageEvent { ts, tokens: total, family })
}

/// Map a raw model string ("claude-opus-4-1", "claude-sonnet-4-5-20250929")
/// to its family. Unknown models return None — they stay in the totals but
/// get no per-model row.
fn model_family(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("opus") {
        Some("Opus")
    } else if m.contains("sonnet") {
        Some("Sonnet")
    } else if m.contains("haiku") {
        Some("Haiku")
    } else {
        None
    }
}

/// Adaptive token count for window labels: "12M" at/above a million, "345K"
/// below (mirrors the aggregate 5h/7d label units).
fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}M", tokens / 1_000_000)
    } else {
        format!("{}K", tokens / 1_000)
    }
}

/// Soft empty state — usable for "no data" / "no files" / "no recent
/// usage". Kept as a snapshot (not Err) so the caller can decide whether
/// to surface it as a popup hint or treat it as an empty bar segment.
fn soft_empty(reason: &str) -> UsageSnapshot {
    UsageSnapshot {
        provider: PROVIDER_LABEL.to_string(),
        level: None,
        windows: Vec::new(),
        unavailable_reason: Some(reason.into()),
        fetched_at: Utc::now(),
    }
}

fn find_jsonl_files(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if max_depth == 0 {
        return files;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return files,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            // history.jsonl is prompt titles only — no usage blocks.
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.eq_ignore_ascii_case("history.jsonl") {
                continue;
            }
            files.push(path);
        } else if path.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if matches!(name, "node_modules" | ".git" | "target" | "debug-log") {
                continue;
            }
            files.extend(find_jsonl_files(&path, max_depth - 1));
        }
    }
    files
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SessionEntry {
    timestamp: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Message {
    role: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_family_maps_known_models() {
        assert_eq!(model_family("claude-opus-4-1"), Some("Opus"));
        assert_eq!(model_family("claude-sonnet-4-5-20250929"), Some("Sonnet"));
        assert_eq!(model_family("claude-haiku-3-5"), Some("Haiku"));
        assert_eq!(model_family("CLAUDE-OPUS-4-20250514"), Some("Opus"));
        assert_eq!(model_family("some-other-model"), None);
    }

    #[test]
    fn format_token_count_picks_units() {
        assert_eq!(format_token_count(12_345_678), "12M");
        assert_eq!(format_token_count(1_000_000), "1M");
        assert_eq!(format_token_count(345_678), "345K");
        assert_eq!(format_token_count(999), "0K");
    }

    #[test]
    fn parse_usage_line_extracts_model_family() {
        let line = r#"{
            "timestamp": "2026-07-18T12:00:00Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-1",
                "usage": { "input_tokens": 10, "output_tokens": 5 }
            }
        }"#;
        let ev = parse_usage_line(line).expect("usage event");
        assert_eq!(ev.tokens, 15);
        assert_eq!(ev.family, Some("Opus"));
    }

    #[test]
    fn parse_usage_line_without_model_keeps_tokens_but_no_family() {
        let line = r#"{
            "timestamp": "2026-07-18T12:00:00Z",
            "message": {
                "role": "assistant",
                "usage": { "input_tokens": 7 }
            }
        }"#;
        let ev = parse_usage_line(line).expect("usage event");
        assert_eq!(ev.tokens, 7);
        assert_eq!(ev.family, None);
    }

    #[test]
    fn parse_usage_line_ignores_non_assistant_messages() {
        let line = r#"{
            "timestamp": "2026-07-18T12:00:00Z",
            "message": { "role": "user", "model": "claude-opus-4-1" }
        }"#;
        assert!(parse_usage_line(line).is_none());
    }
}
