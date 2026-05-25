use crate::Result;
use crate::lang::{CommentSyntax, Language};
use regex::Regex;
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

#[derive(Debug)]
pub struct CounterSet {
    slash_line: Regex,
    hash_line: Regex,
}

impl CounterSet {
    pub fn new() -> Result<Self> {
        Ok(Self {
            slash_line: Regex::new(r"^\s*//")?,
            hash_line: Regex::new(r"^\s*#")?,
        })
    }

    pub fn count(&self, language: &Language, bytes: &[u8]) -> LineCounts {
        let text = String::from_utf8_lossy(bytes);
        let mut state = CountState::default();
        let mut counts = LineCounts {
            files: 1,
            ..LineCounts::default()
        };

        for line in text.lines() {
            match self.classify_line(language.syntax, line, &mut state) {
                LineKind::Blank => counts.blank += 1,
                LineKind::Comment => counts.comment += 1,
                LineKind::Code => counts.code += 1,
            }
        }

        counts
    }

    fn classify_line(&self, syntax: CommentSyntax, line: &str, state: &mut CountState) -> LineKind {
        if line.trim().is_empty() {
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

    fn classify_line_comment(&self, line: &str, marker: &str) -> LineKind {
        if self.line_comment_regex(marker).is_match(line) {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    }

    fn classify_line_and_block(
        &self,
        line: &str,
        state: &mut CountState,
        marker: &str,
        block_start: &'static str,
        block_end: &'static str,
    ) -> LineKind {
        let trimmed = line.trim_start();
        if self.line_comment_regex(marker).is_match(line) {
            return LineKind::Comment;
        }

        let line_comment = line.find(marker);
        let block_comment = line.find(block_start);
        match earliest_comment(line_comment, block_comment) {
            Some(CommentStart::Line(position)) if line[..position].trim().is_empty() => {
                LineKind::Comment
            }
            Some(CommentStart::Line(_)) => LineKind::Code,
            Some(CommentStart::Block(position)) if line[..position].trim().is_empty() => {
                classify_block(trimmed, state, block_start, block_end)
            }
            Some(CommentStart::Block(_)) => LineKind::Code,
            None => LineKind::Code,
        }
    }

    fn classify_python(&self, line: &str, state: &mut CountState) -> LineKind {
        if self.hash_line.is_match(line) {
            return LineKind::Comment;
        }

        let trimmed = line.trim_start();
        let single = trimmed.find("'''");
        let double = trimmed.find("\"\"\"");
        match earliest_python_block(single, double) {
            Some((position, token)) if trimmed[..position].trim().is_empty() => {
                classify_block(trimmed, state, token, token)
            }
            Some((position, token)) => {
                let after_start = position + token.len();
                if !trimmed[after_start..].contains(token) {
                    state.code_string_end = Some(token);
                }
                LineKind::Code
            }
            None => LineKind::Code,
        }
    }

    fn line_comment_regex(&self, marker: &str) -> &Regex {
        match marker {
            "#" => &self.hash_line,
            "//" => &self.slash_line,
            _ => &self.hash_line,
        }
    }
}

impl Default for CounterSet {
    fn default() -> Self {
        Self::new().expect("built-in counter regexes must compile")
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
    line: &str,
    state: &mut CountState,
    block_start: &'static str,
    block_end: &'static str,
) -> LineKind {
    let Some(start) = line.find(block_start) else {
        return LineKind::Code;
    };

    if !line[..start].trim().is_empty() {
        return LineKind::Code;
    }

    let after_start = start + block_start.len();
    if let Some(end) = line[after_start..].find(block_end) {
        let after_end = after_start + end + block_end.len();
        if line[after_end..].trim().is_empty() {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    } else {
        state.block_end = Some(block_end);
        LineKind::Comment
    }
}

fn finish_block_line(line: &str, state: &mut CountState, block_end: &'static str) -> LineKind {
    if let Some(end) = line.find(block_end) {
        state.block_end = None;
        let after_end = end + block_end.len();
        if line[after_end..].trim().is_empty() {
            LineKind::Comment
        } else {
            LineKind::Code
        }
    } else {
        LineKind::Comment
    }
}

fn finish_code_string_line(
    line: &str,
    state: &mut CountState,
    string_end: &'static str,
) -> LineKind {
    if line.contains(string_end) {
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
