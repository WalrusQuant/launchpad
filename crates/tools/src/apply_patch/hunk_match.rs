use super::types::{HunkLine, PatchHunk};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MatchMode {
    Exact,
    Trimmed,
    NormalizedWhitespace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HunkMatch {
    pub(super) start: usize,
    pub(super) mode: MatchMode,
}

pub(super) fn normalize_whitespace(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn lines_match_mode(expected: &str, actual: &str, mode: MatchMode) -> bool {
    match mode {
        MatchMode::Exact => expected == actual,
        MatchMode::Trimmed => expected.trim() == actual.trim(),
        MatchMode::NormalizedWhitespace => {
            normalize_whitespace(expected) == normalize_whitespace(actual)
        }
    }
}

pub(super) fn find_hunk_start(
    old_lines: &[String],
    cursor: usize,
    hunk: &PatchHunk,
) -> anyhow::Result<HunkMatch> {
    let expected = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(text) | HunkLine::Remove(text) => Some(text),
            HunkLine::Add(_) => None,
        })
        .collect::<Vec<_>>();

    if expected.is_empty() {
        return Ok(HunkMatch {
            start: cursor,
            mode: MatchMode::Exact,
        });
    }

    for mode in [
        MatchMode::Exact,
        MatchMode::Trimmed,
        MatchMode::NormalizedWhitespace,
    ] {
        if let Some(start) = try_find_hunk_start(old_lines, cursor, &expected, mode) {
            return Ok(HunkMatch { start, mode });
        }
    }

    if let Some(anchor) = select_hunk_anchor(hunk) {
        for mode in [
            MatchMode::Exact,
            MatchMode::Trimmed,
            MatchMode::NormalizedWhitespace,
        ] {
            if let Some(start) =
                try_find_hunk_start_from_anchor(old_lines, cursor, &expected, anchor, mode)
            {
                return Ok(HunkMatch { start, mode });
            }
        }
    }

    let (best_start, best_prefix, best_mode) =
        best_hunk_partial_match(old_lines, cursor, &expected).unwrap_or((0, 0, MatchMode::Exact));

    if best_prefix > 0 {
        let mismatch_at = best_prefix;
        let actual = old_lines
            .get(best_start + mismatch_at)
            .map(String::as_str)
            .unwrap_or("<EOF>");
        let expected_line = expected
            .get(mismatch_at)
            .map(|s| s.as_str())
            .unwrap_or("<none>");

        return Err(anyhow::anyhow!(
            "failed to locate hunk context; closest {:?} match started at old_lines[{}], mismatch at hunk line {}: expected {:?}, got {:?}",
            best_mode,
            best_start,
            mismatch_at,
            expected_line,
            actual,
        ));
    }

    Err(anyhow::anyhow!(
        "failed to locate hunk context in source file; no partial match found"
    ))
}

fn try_find_hunk_start(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
    mode: MatchMode,
) -> Option<usize> {
    let max_start = old_lines.len().saturating_sub(expected.len());

    (cursor..=max_start).find(|&start| {
        expected.iter().enumerate().all(|(offset, line)| {
            old_lines
                .get(start + offset)
                .map(|actual| lines_match_mode(line, actual, mode))
                .unwrap_or(false)
        })
    })
}

fn select_hunk_anchor(hunk: &PatchHunk) -> Option<(usize, &str)> {
    let mut sequence_index = 0usize;
    let mut best_anchor = None;

    for line in &hunk.lines {
        match line {
            HunkLine::Context(text) => {
                let candidate = (sequence_index, text.as_str());
                let beats_current = !text.trim().is_empty()
                    && best_anchor
                        .map(|(_, best_text): (usize, &str)| text.len() > best_text.len())
                        .unwrap_or(true);
                if beats_current || best_anchor.is_none() {
                    best_anchor = Some(candidate);
                }
                sequence_index += 1;
            }
            HunkLine::Remove(_) => sequence_index += 1,
            HunkLine::Add(_) => {}
        }
    }

    best_anchor
}

fn try_find_hunk_start_from_anchor(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
    anchor: (usize, &str),
    mode: MatchMode,
) -> Option<usize> {
    let (anchor_index, anchor_text) = anchor;
    let max_start = old_lines.len().saturating_sub(expected.len());

    (cursor..=max_start).find(|&start| {
        old_lines
            .get(start + anchor_index)
            .map(|actual| lines_match_mode(anchor_text, actual, mode))
            .unwrap_or(false)
            && expected.iter().enumerate().all(|(offset, line)| {
                old_lines
                    .get(start + offset)
                    .map(|actual| lines_match_mode(line, actual, mode))
                    .unwrap_or(false)
            })
    })
}

fn best_hunk_partial_match(
    old_lines: &[String],
    cursor: usize,
    expected: &[&String],
) -> Option<(usize, usize, MatchMode)> {
    let mut best_start = None;
    let mut best_prefix = 0usize;
    let mut best_mode = MatchMode::Exact;
    let max_start = old_lines.len().saturating_sub(expected.len());

    for mode in [
        MatchMode::Exact,
        MatchMode::Trimmed,
        MatchMode::NormalizedWhitespace,
    ] {
        for start in cursor..=max_start {
            let mut matched = 0usize;

            for (offset, expected_line) in expected.iter().enumerate() {
                let actual = old_lines
                    .get(start + offset)
                    .map(String::as_str)
                    .unwrap_or("<EOF>");
                if lines_match_mode(expected_line, actual, mode) {
                    matched += 1;
                } else {
                    break;
                }
            }

            if matched > best_prefix {
                best_prefix = matched;
                best_start = Some(start);
                best_mode = mode;
            }
        }
    }

    best_start.map(|start| (start, best_prefix, best_mode))
}

pub(super) fn normalized_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}
