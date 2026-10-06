//! Source-preserving Markdown projection and sentence boundaries.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::ops::Range;
pub struct View {
    pub prose: String,
    pub blocks: Vec<Range<usize>>,
    pub protected: Vec<Range<usize>>,
}
impl View {
    pub fn new(source: &str) -> Self {
        let mut bytes = vec![b' '; source.len()];
        let mut blocks = Vec::new();
        let mut protected = 0usize;
        let mut metadata = None;
        let mut protected_ranges = Vec::new();
        for (event, range) in Parser::new_ext(
            source,
            Options::ENABLE_TABLES | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        )
        .into_offset_iter()
        {
            match event {
                Event::Start(Tag::MetadataBlock(_)) => {
                    metadata = Some(range);
                    protected += 1;
                }
                Event::End(TagEnd::MetadataBlock(_)) => {
                    protected = protected.saturating_sub(1);
                    if let Some(range) = metadata.take() {
                        let mut offset = range.start;
                        for line in source[range.clone()].split_inclusive('\n') {
                            if let Some(description) = line.strip_prefix("description: ") {
                                let start = offset + "description: ".len();
                                bytes[start..start + description.len()]
                                    .copy_from_slice(description.as_bytes());
                                blocks.push(start..start + description.len());
                            }
                            offset += line.len();
                        }
                    }
                }
                Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_) | Tag::HtmlBlock) => {
                    if protected == 0 && !range.is_empty() {
                        bytes[range.start] = b'Z';
                        protected_ranges.push(range.clone());
                    }
                    protected += 1;
                }
                Event::End(TagEnd::CodeBlock | TagEnd::BlockQuote(_) | TagEnd::HtmlBlock) => {
                    protected = protected.saturating_sub(1)
                }
                Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell)
                    if protected == 0 =>
                {
                    blocks.push(range)
                }
                Event::Text(_) if protected == 0 => {
                    bytes[range.clone()].copy_from_slice(&source.as_bytes()[range])
                }
                Event::Code(_) | Event::InlineHtml(_) if protected == 0 && !range.is_empty() => {
                    bytes[range.start] = b'Z';
                    protected_ranges.push(range.clone());
                }
                _ => {}
            }
        }
        let mut prose = String::from_utf8(bytes).expect("source-aligned UTF-8");
        let mut quotes = Vec::new();
        let mut open = None;
        for (i, c) in prose.char_indices() {
            if c == '"' || c == '“' || c == '”' {
                if let Some((start, close)) = open {
                    if c == close {
                        quotes.push(start..i + c.len_utf8());
                        open = None;
                    }
                } else if c != '”' {
                    open = Some((i, if c == '“' { '”' } else { '"' }));
                }
            }
        }
        for r in quotes {
            protected_ranges.push(r.clone());
            prose.replace_range(r.clone(), &format!("Z{}", " ".repeat(r.len() - 1)));
        }
        if blocks.is_empty() && !prose.trim().is_empty() {
            blocks.push(0..prose.len());
        }
        Self {
            prose,
            blocks,
            protected: protected_ranges,
        }
    }
}
pub fn has_explanation(source: &str) -> bool {
    let v = View::new(source);
    words(&v.prose)
        .iter()
        .filter(|r| {
            let w = &v.prose[(*r).clone()];
            w != "Z"
        })
        .count()
        >= 2
}
pub fn identifier(s: &str) -> bool {
    s.chars().any(|c| c.is_numeric() || "_/:=\\".contains(c))
        || s.contains('.')
        || (s.chars().filter(|c| c.is_alphabetic()).count() > 1
            && s.chars()
                .filter(|c| c.is_alphabetic())
                .all(|c| c.is_uppercase()))
        || s.chars().skip(1).any(char::is_uppercase)
}
pub fn words(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices().chain(std::iter::once((s.len(), ' '))) {
        if c.is_alphanumeric()
            || (c == ','
                && i > 0
                && s.as_bytes()[i - 1].is_ascii_digit()
                && s.as_bytes().get(i + 1).is_some_and(u8::is_ascii_digit))
            || (("-'’‐‑_./:=\\".contains(c)) && start.is_some())
        {
            start.get_or_insert(i);
        } else if let Some(a) = start.take() {
            let end = s[a..i].trim_end_matches(['.', ',', ':', '/', '-']).len() + a;
            if end > a {
                out.push(a..end);
            }
        }
    }
    out
}
pub fn parentheticals(s: &str) -> Vec<Range<usize>> {
    let mut stack = Vec::new();
    let mut out = Vec::new();
    for (i, c) in s.char_indices() {
        if c == '(' {
            stack.push(i + 1);
        } else if c == ')'
            && let Some(start) = stack.pop()
        {
            out.push(start..i);
        }
    }
    out
}
pub fn count_words(s: &str) -> usize {
    let mut text = s.to_string();
    let mut ranges = parentheticals(s);
    ranges.sort_by_key(|r| r.start);
    let mut until = 0;
    for r in ranges {
        if r.start >= until {
            text.replace_range(
                r.start - 1..r.end + 1,
                &format!("Z{}", " ".repeat(r.len() + 1)),
            );
            until = r.end + 1;
        }
    }
    let tokens = words(&text);
    let mut count = tokens.len();
    for (i, pair) in tokens.windows(2).enumerate() {
        let a = &text[pair[0].clone()];
        let b = &text[pair[1].clone()];
        let numeric = number(a);
        if numeric && measurement_unit(b) {
            count = count.saturating_sub(1);
            if ["degree", "degrees"].contains(&b.to_lowercase().as_str())
                && tokens.get(i + 2).is_some_and(|r| {
                    ["celsius", "fahrenheit"].contains(&text[r.clone()].to_lowercase().as_str())
                })
            {
                count = count.saturating_sub(1);
            }
        }
        if a.eq_ignore_ascii_case("No") && number(b) {
            count = count.saturating_sub(1);
        }
    }
    // A contiguous capitalized name counts as one unit. Meaning still needs review.
    for pair in tokens.windows(2) {
        let a = &text[pair[0].clone()];
        let b = &text[pair[1].clone()];
        let gap = &text[pair[0].end..pair[1].start];
        if a != "Z"
            && b != "Z"
            && b != "No"
            && a.chars().next().is_some_and(char::is_uppercase)
            && a.chars().skip(1).any(char::is_lowercase)
            && b.chars().next().is_some_and(char::is_uppercase)
            && b.chars().skip(1).any(char::is_lowercase)
            && gap.chars().all(|c| c == ' ' || c == char::from(9))
            && ![
                "A", "An", "The", "Use", "If", "Do", "No", "Read", "Keep", "Save", "Print",
                "Write", "Remove", "Add", "Run", "Open", "Close", "Start", "Stop", "Check",
                "Verify", "Select", "Install", "Copy",
            ]
            .contains(&a)
        {
            count = count.saturating_sub(1);
        }
    }
    count
}
pub fn sentences(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth: usize = 0;
    for (i, c) in s.char_indices() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth = depth.saturating_sub(1);
        }
        if depth > 0 || !".!?:".contains(c) {
            continue;
        }
        let after = &s[i + c.len_utf8()..];
        if !after.is_empty() && !after.starts_with(char::is_whitespace) {
            continue;
        }
        let before = &s[start..i];
        let last = before
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_lowercase();
        let previous = before.split_whitespace().rev().nth(1).unwrap_or("");
        let after_number = previous.chars().any(|c| c.is_ascii_digit())
            && previous
                .chars()
                .all(|c| c.is_ascii_digit() || ",./+-".contains(c));
        let abbreviation = [
            "e.g", "i.e", "dr", "mr", "mrs", "ms", "prof", "fig", "no", "vs", "approx", "deg",
        ]
        .contains(&last.as_str());
        let initial = last.len() == 1
            && !["z", "m", "s", "g", "v", "a", "c", "f", "h", "b", "i"].contains(&last.as_str())
            && last.chars().all(char::is_alphabetic);
        if c == '.' && !after_number && (abbreviation || initial) {
            continue;
        }
        if count_words(&s[start..i + 1]) > 0 {
            out.push(start..i + 1);
        }
        start = i + 1;
    }
    if count_words(&s[start..]) > 0 {
        out.push(start..s.len());
    }
    out
}
/// Pack contiguous source slices. An oversized literal uses UTF-8 boundaries.
/// Measure each candidate with the real tokenizer, including the heading.
pub fn pack(
    source: &str,
    mut fits: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<Vec<String>> {
    let view = View::new(source);
    let mut boundaries: Vec<_> = sentences(&view.prose).iter().map(|r| r.end).collect();
    boundaries.extend(view.blocks.iter().map(|r| r.end));
    boundaries.push(source.len());
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut out = Vec::new();
    let mut start = 0;
    let mut best = 0;
    for end in boundaries {
        if end <= start {
            continue;
        }
        if fits(&source[start..end])? {
            best = end;
            continue;
        }
        if best > start {
            out.push(source[start..best].to_string());
            start = best;
        }
        if fits(&source[start..end])? {
            best = end;
            continue;
        }
        while start < end {
            // Token counts need not be monotonic. Only return verified fitting slices.
            let points: Vec<_> = source[start..end]
                .char_indices()
                .map(|(i, _)| start + i)
                .skip(1)
                .chain(std::iter::once(end))
                .collect();
            let mut lo = 0;
            let mut hi = points.len();
            let mut chosen = None;
            while lo < hi {
                let mid = (lo + hi) / 2;
                if fits(&source[start..points[mid]])? {
                    chosen = Some(points[mid]);
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            let next = chosen.ok_or_else(|| {
                anyhow::anyhow!("The heading uses the full token budget. Use a shorter heading.")
            })?;
            if next == end {
                best = end;
                break;
            }
            out.push(source[start..next].to_string());
            start = next;
            best = start;
        }
    }
    if best > start {
        out.push(source[start..best].to_string());
    }
    anyhow::ensure!(out.concat() == source, "Chunk packing changed the source.");
    Ok(out)
}

/// Borrow exact fenced and inline evidence from the original source.
pub fn evidence(source: &str) -> Vec<&str> {
    let mut spans = Vec::new();
    for (event, range) in Parser::new(source).into_offset_iter() {
        if matches!(
            event,
            Event::Start(Tag::CodeBlock(_) | Tag::BlockQuote(_)) | Event::Code(_)
        ) {
            spans.push(&source[range]);
        }
    }
    let mut open = None;
    for (i, c) in source.char_indices() {
        if c == '"' || c == '“' || c == '”' {
            if let Some((start, close)) = open {
                if c == close {
                    spans.push(&source[start..i + c.len_utf8()]);
                    open = None;
                }
            } else if c != '”' {
                open = Some((i, if c == '“' { '”' } else { '"' }));
            }
        }
    }
    spans
}

pub fn number_or_unit(s: &str) -> bool {
    [
        "first",
        "second",
        "third",
        "fourth",
        "fifth",
        "sixth",
        "seventh",
        "eighth",
        "ninth",
        "tenth",
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
        "twenty",
        "thirty",
        "forty",
        "fifty",
        "sixty",
        "seventy",
        "eighty",
        "ninety",
        "hundred",
        "thousand",
        "million",
        "billion",
        "ms",
        "s",
        "mm",
        "cm",
        "m",
        "km",
        "kg",
        "g",
        "min",
        "hz",
        "khz",
        "mhz",
        "ghz",
        "kib",
        "mib",
        "gib",
        "µs",
        "μs",
    ]
    .contains(&s.to_lowercase().as_str())
}

fn number(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit() || ",./+-".contains(c))
        || s.split(['-', '‐', '‑']).all(|s| {
            [
                "zero",
                "one",
                "two",
                "three",
                "four",
                "five",
                "six",
                "seven",
                "eight",
                "nine",
                "ten",
                "eleven",
                "twelve",
                "thirteen",
                "fourteen",
                "fifteen",
                "sixteen",
                "seventeen",
                "eighteen",
                "nineteen",
                "twenty",
                "thirty",
                "forty",
                "fifty",
                "sixty",
                "seventy",
                "eighty",
                "ninety",
                "hundred",
                "thousand",
                "million",
            ]
            .contains(&s.to_lowercase().as_str())
        })
}
fn measurement_unit(s: &str) -> bool {
    [
        "mm",
        "cm",
        "m",
        "km",
        "ms",
        "µs",
        "μs",
        "s",
        "min",
        "h",
        "hz",
        "khz",
        "mhz",
        "ghz",
        "b",
        "kb",
        "mb",
        "gb",
        "kib",
        "mib",
        "gib",
        "v",
        "a",
        "w",
        "kg",
        "g",
        "deg",
        "c",
        "f",
        "knots",
        "ω",
        "ohm",
        "ohms",
        "ma",
        "a.m",
        "p.m",
        "byte",
        "bytes",
        "bit",
        "bits",
        "second",
        "seconds",
        "millisecond",
        "milliseconds",
        "minute",
        "minutes",
        "hour",
        "hours",
        "volt",
        "volts",
        "ampere",
        "amperes",
        "meter",
        "meters",
        "metre",
        "metres",
        "millimeter",
        "millimeters",
        "degree",
        "degrees",
        "megabyte",
        "megabytes",
        "gigabyte",
        "gigabytes",
        "kilobyte",
        "kilobytes",
        "watt",
        "watts",
        "kilogram",
        "kilograms",
        "gram",
        "grams",
        "pound",
        "pounds",
        "foot",
        "feet",
        "inch",
        "inches",
    ]
    .contains(&s.to_lowercase().as_str())
}
