//! Compiler-owned source provenance over the exact numbered compile input.

#[derive(Clone, Copy)]
struct Block {
    start: usize,
    end: usize,
    frontmatter: bool,
}

fn fence(line: &str) -> Option<(char, usize)> {
    let line = line.trim_start();
    let ch = line.chars().next()?;
    let length = line.chars().take_while(|c| *c == ch).count();
    ((ch == '`' || ch == '~') && length >= 3).then_some((ch, length))
}

fn heading(line: &str) -> bool {
    let count = line.bytes().take_while(|c| *c == b'#').count();
    (1..=6).contains(&count) && line[count..].chars().next().is_none_or(char::is_whitespace)
}

/// Snap a model citation to enclosing markdown blocks, with a fixed body fallback.
pub fn snap_source_lines(text: &str, cited: Option<&str>) -> String {
    let lines: Vec<_> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    if lines.first() == Some(&"---") {
        if let Some(close) = lines.iter().skip(1).position(|line| *line == "---") {
            i = close + 2;
            blocks.push(Block {
                start: 1,
                end: i,
                frontmatter: true,
            });
        }
    }
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i + 1;
        if let Some((ch, length)) = fence(lines[i]) {
            i += 1;
            while i < lines.len() {
                let closes = fence(lines[i]).is_some_and(|(c, n)| c == ch && n >= length);
                i += 1;
                if closes {
                    break;
                }
            }
        } else if heading(lines[i]) {
            i += 1;
        } else {
            i += 1;
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && fence(lines[i]).is_none()
                && !heading(lines[i])
            {
                i += 1;
            }
        }
        blocks.push(Block {
            start,
            end: i,
            frontmatter: false,
        });
    }
    let body: Vec<_> = blocks.iter().filter(|b| !b.frontmatter).collect();
    let fallback = || {
        let selected = if body.is_empty() {
            blocks.iter().collect::<Vec<_>>()
        } else {
            body.clone()
        };
        match (selected.first(), selected.last()) {
            (Some(first), Some(last)) => format!("{}-{}", first.start, last.end),
            _ => "1-1".into(),
        }
    };
    let parse = |s: &str| -> Option<usize> {
        (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())).then(|| {
            s.bytes().fold(0usize, |n, digit| {
                n.saturating_mul(10).saturating_add((digit - b'0') as usize)
            })
        })
    };
    let citation = cited.and_then(|value| {
        if let Some((a, b)) = value.split_once('-') {
            Some((parse(a.trim())?, parse(b.trim())?))
        } else {
            let n = parse(value)?;
            Some((n, n))
        }
    });
    let Some((a, b)) = citation else {
        return fallback();
    };
    if a == 0 || a > b || a > lines.len() || body.is_empty() {
        return fallback();
    }
    let b = b.min(lines.len());
    let touched: Vec<_> = blocks
        .iter()
        .filter(|block| block.end >= a && block.start <= b)
        .collect();
    if let (Some(first), Some(last)) = (touched.first(), touched.last()) {
        format!("{}-{}", first.start, last.end)
    } else if let Some(block) = blocks
        .iter()
        .find(|block| block.start > b)
        .or_else(|| blocks.last())
    {
        format!("{}-{}", block.start, block.end)
    } else {
        fallback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn block_rules() {
        let text = "---\npublic: true\n---\n# Title\n\nparagraph\ncontinued\n\n```json\n{}\n```\n- one\n- two\n\n~~~\nunclosed\n\n";
        for (cite, expected) in [
            (Some("6"), "6-7"),
            (Some("10"), "9-11"),
            (Some("9"), "9-11"),
            (Some("7-10"), "6-11"),
            (Some("4"), "4-4"),
            (Some("12"), "12-13"),
            (Some("8"), "9-11"),
            (Some("99"), "4-17"),
            (Some("12-99"), "12-17"),
            (Some("abc"), "4-17"),
            (Some("0"), "4-17"),
            (Some("5-3"), "4-17"),
            (None, "4-17"),
            (Some(" 6 - 7 "), "6-7"),
            (Some("1"), "1-3"),
        ] {
            assert_eq!(snap_source_lines(text, cite), expected, "{cite:?}");
        }
        assert_eq!(snap_source_lines("---\na: b\n---\n", Some("2")), "1-3");
        assert_eq!(snap_source_lines("\n\n", None), "1-1");
        assert_eq!(snap_source_lines("# Title\n\n", Some("2")), "1-1");
        assert_eq!(
            snap_source_lines("# Title\n\n", Some("2-999999999999999999999999999999")),
            "1-1"
        );
        assert_eq!(
            snap_source_lines(
                "# Title\nbody\ncontinued",
                Some("2-999999999999999999999999999999")
            ),
            "2-3"
        );
    }

    #[test]
    fn fences_lists_tables_and_comments() {
        let text = "paragraph\n  ````rust\n# inside\n\n```\n~~~~\n  `````\nafter\n# Heading\n- one\n- two\n\n- loose\n\n<!-- schema.md -->\n| a | b |\n| - | - |\n";
        for (cite, expected) in [
            ("3", "2-7"),
            ("2", "2-7"),
            ("1-2", "1-7"),
            ("8", "8-8"),
            ("9", "9-9"),
            ("11", "10-11"),
            ("12", "13-13"),
            ("16", "15-17"),
        ] {
            assert_eq!(snap_source_lines(text, Some(cite)), expected);
        }
        assert_eq!(snap_source_lines("# A\n\n\n", Some("2-3")), "1-1");
        assert_eq!(snap_source_lines("a\n~~~\nb\n\n~~~\nc", Some("4")), "2-5");
    }
}
