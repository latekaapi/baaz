//! Diff rows and their `+N/−N` chip, one way for every provider.

use aui_protocol::{DiffKind, DiffLine, DiffStat};

/// The display rows of one unified-diff hunk body: `+` adds, `-` deletes,
/// anything else context (its leading space dropped), numbered from
/// `old_start` / `new_start`. `+++`/`---` file headers are skipped; with
/// `skip_markers`, so are `\ No newline at end of file` markers.
pub fn hunk_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    old_start: u32,
    new_start: u32,
    skip_markers: bool,
) -> Vec<DiffLine> {
    let (mut old_no, mut new_no) = (old_start, new_start);
    let mut rows = Vec::new();
    for line in lines {
        if skip_markers && line.starts_with('\\') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            if !rest.starts_with('+') {
                rows.push(DiffLine { kind: DiffKind::Add, old_no: None, new_no: Some(new_no), text: rest.to_owned() });
                new_no += 1;
            }
        } else if let Some(rest) = line.strip_prefix('-') {
            if !rest.starts_with('-') {
                rows.push(DiffLine { kind: DiffKind::Del, old_no: Some(old_no), new_no: None, text: rest.to_owned() });
                old_no += 1;
            }
        } else {
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(DiffLine {
                kind: DiffKind::Context,
                old_no: Some(old_no),
                new_no: Some(new_no),
                text: text.to_owned(),
            });
            old_no += 1;
            new_no += 1;
        }
    }
    rows
}

/// A created file's content as all-addition rows, numbered from 1.
pub fn addition_lines(content: &str) -> Vec<DiffLine> {
    content
        .lines()
        .enumerate()
        .map(|(index, line)| DiffLine {
            kind: DiffKind::Add,
            old_no: None,
            new_no: Some(index as u32 + 1),
            text: line.to_owned(),
        })
        .collect()
}

/// `(added, removed)` over display rows.
pub fn counts(rows: &[DiffLine]) -> (u64, u64) {
    rows.iter().fold((0, 0), |(added, removed), row| match row.kind {
        DiffKind::Add => (added + 1, removed),
        DiffKind::Del => (added, removed + 1),
        DiffKind::Context => (added, removed),
    })
}

/// The chip over several hunks' rows touching `files` files.
pub fn stat<'a>(hunks: impl IntoIterator<Item = &'a [DiffLine]>, files: u64) -> DiffStat {
    let (added, removed) =
        hunks.into_iter().map(counts).fold((0, 0), |(a, r), (added, removed)| (a + added, r + removed));
    DiffStat { added, removed, files }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_rows_number_both_sides_and_skip_headers() {
        let rows = hunk_lines(["--- a", "+++ b", " keep", "-old", "+new", "\\ No newline at end of file"], 3, 3, true);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[1].old_no, rows[1].new_no), (Some(4), None));
        assert_eq!((rows[2].old_no, rows[2].new_no), (None, Some(4)));
        assert_eq!(counts(&rows), (1, 1));
        let kept = hunk_lines(["\\ No newline at end of file"], 1, 1, false);
        assert_eq!(kept[0].kind, DiffKind::Context, "markers read as context unless skipped");
        assert_eq!(stat([addition_lines("a\nb").as_slice()], 1), DiffStat { added: 2, removed: 0, files: 1 });
    }
}
