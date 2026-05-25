use crate::Result;
use crate::lang::{CommentSyntax, Language};
use memchr::{memchr, memmem};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LineCounts {
    pub files: u64,
    pub blank: u64,
    pub comment: u64,
    pub code: u64,
}

impl LineCounts {
    pub fn add_assign(&mut self, other: Self) {
        self.files += other.files;
        self.blank += other.blank;
        self.comment += other.comment;
        self.code += other.code;
    }
}

#[derive(Debug, Default)]
pub struct CounterSet;

impl CounterSet {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }

    pub fn count(&self, language: &Language, bytes: &[u8]) -> LineCounts {
        let mut state = CountState::default();
        let mut counts = LineCounts {
            files: 1,
            ..LineCounts::default()
        };

        for line in ByteLines::new(bytes) {
            match self.classify_line(language.syntax, line, &mut state) {
                LineKind::Blank => counts.blank += 1,
                LineKind::Comment => counts.comment += 1,
                LineKind::Code => counts.code += 1,
            }
        }

        counts
    }

    fn classify_line(
        &self,
        syntax: CommentSyntax,
        line: &[u8],
        state: &mut CountState,
    ) -> LineKind {
        if is_blank(line) {
            return if state.block_end.is_some() {
                LineKind::Comment
            } else if state.code_string_end.is_some() {
                LineKind::Code
            } else {
                LineKind::Blank
            };
        }

        if let Some(string_end) = state.code_string_end {
            return finish_code_string_line(line, state, string_end);
        }

        if let Some(block_end) = state.block_end {
            return finish_block_line(line, state, block_end);
        }

        match syntax {
            CommentSyntax::Plain => LineKind::Code,
            CommentSyntax::Line { marker } => self.classify_line_comment(line, marker),
            CommentSyntax::LineAndBlock {
                line: marker,
                block_start,
                block_end,
            } => self.classify_line_and_block(line, state, marker, block_start, block_end),
            CommentSyntax::Block {
                block_start,
                block_end,
            } => classify_block(line, state, block_start, block_end),
            CommentSyntax::Python => self.classify_python(line, state),
        }
    }

    fn classify_line_comment(&self, line: &[u8], marker: &str) -> LineKind {
        if trim_start(line).starts_with(marker.as_bytes()) {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    }

    fn classify_line_and_block(
        &self,
        line: &[u8],
        state: &mut CountState,
        marker: &str,
        block_start: &'static str,
        block_end: &'static str,
    ) -> LineKind {
        let trimmed = trim_start(line);
        if trimmed.starts_with(marker.as_bytes()) {
            return LineKind::Comment;
        }

        let comment = if marker == "//" && block_start == "/*" {
            find_slash_comment(line)
        } else {
            earliest_comment(
                find_subslice(line, marker.as_bytes()),
                find_subslice(line, block_start.as_bytes()),
            )
        };

        match comment {
            Some(CommentStart::Line(position)) if is_blank(&line[..position]) => LineKind::Comment,
            Some(CommentStart::Line(_)) => LineKind::Code,
            Some(CommentStart::Block(position)) if is_blank(&line[..position]) => {
                classify_block(trimmed, state, block_start, block_end)
            }
            Some(CommentStart::Block(_)) => LineKind::Code,
            None => LineKind::Code,
        }
    }

    fn classify_python(&self, line: &[u8], state: &mut CountState) -> LineKind {
        if trim_start(line).starts_with(b"#") {
            return LineKind::Comment;
        }

        let trimmed = trim_start(line);
        let single = find_subslice(trimmed, b"'''");
        let double = find_subslice(trimmed, b"\"\"\"");
        match earliest_python_block(single, double) {
            Some((position, token)) if is_blank(&trimmed[..position]) => {
                classify_block(trimmed, state, token, token)
            }
            Some((position, token)) => {
                let after_start = position + token.len();
                if find_subslice(&trimmed[after_start..], token.as_bytes()).is_none() {
                    state.code_string_end = Some(token);
                }
                LineKind::Code
            }
            None => LineKind::Code,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CountState {
    block_end: Option<&'static str>,
    code_string_end: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineKind {
    Blank,
    Comment,
    Code,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommentStart {
    Line(usize),
    Block(usize),
}

fn classify_block(
    line: &[u8],
    state: &mut CountState,
    block_start: &'static str,
    block_end: &'static str,
) -> LineKind {
    let block_start = block_start.as_bytes();
    let block_end_bytes = block_end.as_bytes();
    let Some(start) = find_subslice(line, block_start) else {
        return LineKind::Code;
    };

    if !is_blank(&line[..start]) {
        return LineKind::Code;
    }

    let after_start = start + block_start.len();
    if let Some(end) = find_subslice(&line[after_start..], block_end_bytes) {
        let after_end = after_start + end + block_end_bytes.len();
        if is_blank(&line[after_end..]) {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    } else {
        state.block_end = Some(block_end);
        LineKind::Comment
    }
}

fn finish_block_line(line: &[u8], state: &mut CountState, block_end: &'static str) -> LineKind {
    if let Some(end) = find_subslice(line, block_end.as_bytes()) {
        state.block_end = None;
        let after_end = end + block_end.len();
        if is_blank(&line[after_end..]) {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    } else {
        LineKind::Comment
    }
}

fn finish_code_string_line(
    line: &[u8],
    state: &mut CountState,
    string_end: &'static str,
) -> LineKind {
    if find_subslice(line, string_end.as_bytes()).is_some() {
        state.code_string_end = None;
    }
    LineKind::Code
}

fn earliest_comment(line: Option<usize>, block: Option<usize>) -> Option<CommentStart> {
    match (line, block) {
        (Some(line), Some(block)) if line < block => Some(CommentStart::Line(line)),
        (Some(line), Some(block)) if block < line => Some(CommentStart::Block(block)),
        (Some(line), Some(_)) => Some(CommentStart::Line(line)),
        (Some(line), None) => Some(CommentStart::Line(line)),
        (None, Some(block)) => Some(CommentStart::Block(block)),
        (None, None) => None,
    }
}

struct ByteLines<'a> {
    remaining: &'a [u8],
}

impl<'a> ByteLines<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }
}

impl<'a> Iterator for ByteLines<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }

        match memchr(b'\n', self.remaining) {
            Some(position) => {
                let line = &self.remaining[..position];
                self.remaining = &self.remaining[position + 1..];
                Some(line)
            }
            None => {
                let line = self.remaining;
                self.remaining = &[];
                Some(line)
            }
        }
    }
}

fn is_blank(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| byte.is_ascii_whitespace())
}

fn trim_start(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    &bytes[start..]
}

#[inline]
fn find_slash_comment(line: &[u8]) -> Option<CommentStart> {
    let mut offset = 0;
    while let Some(position) = memchr(b'/', &line[offset..]) {
        let index = offset + position;
        match line.get(index + 1) {
            Some(b'/') => return Some(CommentStart::Line(index)),
            Some(b'*') => return Some(CommentStart::Block(index)),
            _ => offset = index + 1,
        }
    }
    None
}

#[inline]
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    match needle.len() {
        0 => Some(0),
        1 => memchr(needle[0], haystack),
        2 => find_pair(haystack, needle[0], needle[1]),
        3 => find_triple(haystack, needle[0], needle[1], needle[2]),
        _ => memmem::find(haystack, needle),
    }
}

#[inline]
fn find_pair(haystack: &[u8], first: u8, second: u8) -> Option<usize> {
    let mut offset = 0;
    while let Some(position) = memchr(first, &haystack[offset..]) {
        let index = offset + position;
        if haystack.get(index + 1) == Some(&second) {
            return Some(index);
        }
        offset = index + 1;
    }
    None
}

#[inline]
fn find_triple(haystack: &[u8], first: u8, second: u8, third: u8) -> Option<usize> {
    let mut offset = 0;
    while let Some(position) = memchr(first, &haystack[offset..]) {
        let index = offset + position;
        if haystack.get(index + 1) == Some(&second) && haystack.get(index + 2) == Some(&third) {
            return Some(index);
        }
        offset = index + 1;
    }
    None
}

fn earliest_python_block(
    single: Option<usize>,
    double: Option<usize>,
) -> Option<(usize, &'static str)> {
    match (single, double) {
        (Some(single), Some(double)) if single < double => Some((single, "'''")),
        (Some(single), Some(double)) if double < single => Some((double, "\"\"\"")),
        (Some(single), Some(_)) => Some((single, "'''")),
        (Some(single), None) => Some((single, "'''")),
        (None, Some(double)) => Some((double, "\"\"\"")),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::LanguageRegistry;
    use std::path::Path;

    fn count_for(path: &str, text: &str) -> LineCounts {
        let registry = LanguageRegistry::new();
        let language = registry.detect_path(Path::new(path)).unwrap();
        CounterSet::default().count(language, text.as_bytes())
    }

    #[test]
    fn counts_slash_block_language() {
        let counts = count_for(
            "lib.rs",
            r#"
// module comment
fn main() {
    println!("hi"); // inline comment
}
/*
 * block comment
 */
"#,
        );

        assert_eq!(
            counts,
            LineCounts {
                files: 1,
                blank: 1,
                comment: 4,
                code: 3,
            }
        );
    }

    #[test]
    fn counts_hash_language() {
        let counts = count_for(
            "script.sh",
            r#"
# comment
echo hello # inline
"#,
        );

        assert_eq!(
            counts,
            LineCounts {
                files: 1,
                blank: 1,
                comment: 1,
                code: 1,
            }
        );
    }

    #[test]
    fn counts_python_triple_quoted_comment_blocks() {
        let counts = count_for(
            "tool.py",
            r#"
"""module docs"""
value = """
not treated as a comment block
"""
def run():
    pass
"#,
        );

        assert_eq!(
            counts,
            LineCounts {
                files: 1,
                blank: 1,
                comment: 1,
                code: 5,
            }
        );
    }

    #[test]
    fn counts_markup_comment_blocks() {
        let counts = count_for(
            "index.html",
            r#"
<!-- comment
still comment -->
<main>hello</main>
"#,
        );

        assert_eq!(
            counts,
            LineCounts {
                files: 1,
                blank: 1,
                comment: 2,
                code: 1,
            }
        );
    }
}
