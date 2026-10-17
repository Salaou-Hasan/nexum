//! Source file identity, spans and source map.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(file: FileId, start: usize, end: usize) -> Self {
        Self { file, start, end }
    }
    pub fn merge(a: Span, b: Span) -> Self {
        debug_assert_eq!(a.file, b.file);
        Self {
            file: a.file,
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub id: FileId,
    pub path: PathBuf,
    pub text: String,
}

impl SourceFile {
    pub fn new(id: FileId, path: PathBuf, text: String) -> Self {
        Self { id, path, text }
    }
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, ch) in self.text.char_indices() {
            if i >= offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }
    pub fn add(&mut self, path: PathBuf, text: String) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, path, text));
        id
    }
    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }
}
