use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommentSyntax {
    Plain,
    Line {
        marker: &'static str,
    },
    LineAndBlock {
        line: &'static str,
        block_start: &'static str,
        block_end: &'static str,
    },
    Block {
        block_start: &'static str,
        block_end: &'static str,
    },
    Python,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Language {
    pub name: &'static str,
    pub normalized_name: &'static str,
    pub extensions: &'static [&'static str],
    pub filenames: &'static [&'static str],
    pub shebangs: &'static [&'static str],
    pub syntax: CommentSyntax,
}

#[derive(Debug)]
pub struct LanguageRegistry {
    languages: &'static [Language],
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self {
            languages: LANGUAGES,
        }
    }

    pub fn languages(&self) -> &'static [Language] {
        self.languages
    }

    pub fn find_by_name(&self, name: &str) -> Option<&'static Language> {
        let normalized = normalize_language_name(name);
        self.languages
            .iter()
            .find(|language| language.normalized_name == normalized)
    }

    pub fn detect_path(&self, path: &Path) -> Option<&'static Language> {
        let filename = path.file_name()?.to_str()?;
        let filename_lower = filename.to_ascii_lowercase();

        if let Some(language) = self.languages.iter().find(|language| {
            language
                .filenames
                .iter()
                .any(|candidate| *candidate == filename_lower)
        }) {
            return Some(language);
        }

        self.languages.iter().find(|language| {
            language.extensions.iter().any(|extension| {
                filename_lower == *extension || filename_lower.ends_with(&format!(".{extension}"))
            })
        })
    }

    pub fn detect(&self, path: &Path, first_line: Option<&str>) -> Option<&'static Language> {
        self.detect_path(path).or_else(|| {
            let first_line = first_line?.trim_start();
            if !first_line.starts_with("#!") {
                return None;
            }

            let command = first_line.to_ascii_lowercase();
            self.languages.iter().find(|language| {
                language
                    .shebangs
                    .iter()
                    .any(|needle| command.contains(needle))
            })
        })
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub fn normalize_language_name(name: &str) -> String {
    let name = name.replace("++", "plusplus").replace('#', "sharp");
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

const SLASH_BLOCK: CommentSyntax = CommentSyntax::LineAndBlock {
    line: "//",
    block_start: "/*",
    block_end: "*/",
};

const HASH_LINE: CommentSyntax = CommentSyntax::Line { marker: "#" };

pub const LANGUAGES: &[Language] = &[
    Language {
        name: "Rust",
        normalized_name: "rust",
        extensions: &["rs", "rs.in"],
        filenames: &[],
        shebangs: &[],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "C",
        normalized_name: "c",
        extensions: &["c", "h"],
        filenames: &[],
        shebangs: &[],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "C++",
        normalized_name: "cplusplus",
        extensions: &[
            "cc", "cpp", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "inl",
        ],
        filenames: &[],
        shebangs: &[],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "Python",
        normalized_name: "python",
        extensions: &["py", "pyw", "pyi"],
        filenames: &[
            "sconstruct",
            "sconscript",
            "wscript",
            "gyp",
            "gypi",
            "snakefile",
        ],
        shebangs: &["python"],
        syntax: CommentSyntax::Python,
    },
    Language {
        name: "JavaScript",
        normalized_name: "javascript",
        extensions: &["js", "mjs", "cjs", "jsx"],
        filenames: &[],
        shebangs: &["node", "deno"],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "TypeScript",
        normalized_name: "typescript",
        extensions: &["ts", "tsx"],
        filenames: &[],
        shebangs: &["ts-node"],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "Go",
        normalized_name: "go",
        extensions: &["go"],
        filenames: &[],
        shebangs: &[],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "Java",
        normalized_name: "java",
        extensions: &["java"],
        filenames: &[],
        shebangs: &[],
        syntax: SLASH_BLOCK,
    },
    Language {
        name: "Shell",
        normalized_name: "shell",
        extensions: &["sh", "bash", "zsh", "ksh"],
        filenames: &[],
        shebangs: &["/sh", "bash", "zsh", "ksh"],
        syntax: HASH_LINE,
    },
    Language {
        name: "CSS",
        normalized_name: "css",
        extensions: &["css"],
        filenames: &[],
        shebangs: &[],
        syntax: CommentSyntax::Block {
            block_start: "/*",
            block_end: "*/",
        },
    },
    Language {
        name: "HTML",
        normalized_name: "html",
        extensions: &["html", "htm", "xhtml"],
        filenames: &[],
        shebangs: &[],
        syntax: CommentSyntax::Block {
            block_start: "<!--",
            block_end: "-->",
        },
    },
    Language {
        name: "XML",
        normalized_name: "xml",
        extensions: &["xml", "xsd", "xsl", "xslt", "svg"],
        filenames: &[],
        shebangs: &[],
        syntax: CommentSyntax::Block {
            block_start: "<!--",
            block_end: "-->",
        },
    },
    Language {
        name: "Markdown",
        normalized_name: "markdown",
        extensions: &["md", "markdown", "mdown", "mkd", "mkdn", "mdx"],
        filenames: &[],
        shebangs: &[],
        syntax: CommentSyntax::Plain,
    },
    Language {
        name: "JSON",
        normalized_name: "json",
        extensions: &["json", "jsonl"],
        filenames: &[],
        shebangs: &[],
        syntax: CommentSyntax::Plain,
    },
    Language {
        name: "YAML",
        normalized_name: "yaml",
        extensions: &["yaml", "yml"],
        filenames: &[],
        shebangs: &[],
        syntax: HASH_LINE,
    },
    Language {
        name: "TOML",
        normalized_name: "toml",
        extensions: &["toml"],
        filenames: &[],
        shebangs: &[],
        syntax: HASH_LINE,
    },
    Language {
        name: "Make",
        normalized_name: "make",
        extensions: &["mk", "mak"],
        filenames: &["makefile", "gnumakefile"],
        shebangs: &[],
        syntax: HASH_LINE,
    },
    Language {
        name: "Dockerfile",
        normalized_name: "dockerfile",
        extensions: &["dockerfile"],
        filenames: &["dockerfile", "containerfile"],
        shebangs: &[],
        syntax: HASH_LINE,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_and_filename() {
        let registry = LanguageRegistry::new();

        assert_eq!(
            registry.detect_path(Path::new("src/main.rs")).unwrap().name,
            "Rust"
        );
        assert_eq!(
            registry.detect_path(Path::new("Makefile")).unwrap().name,
            "Make"
        );
        assert_eq!(
            registry.detect_path(Path::new("Dockerfile")).unwrap().name,
            "Dockerfile"
        );
    }

    #[test]
    fn detects_by_shebang_without_extension() {
        let registry = LanguageRegistry::new();

        assert_eq!(
            registry
                .detect(Path::new("script"), Some("#!/usr/bin/env python3"))
                .unwrap()
                .name,
            "Python"
        );
        assert_eq!(
            registry
                .detect(Path::new("tool"), Some("#!/bin/bash"))
                .unwrap()
                .name,
            "Shell"
        );
    }

    #[test]
    fn normalizes_language_names_for_filters() {
        assert_eq!(normalize_language_name("C++"), "cplusplus");
        assert_eq!(normalize_language_name("Java Script"), "javascript");
    }
}
