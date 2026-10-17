//! Diagnostic model and rendering.

use nexum_source::{SourceMap, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticCode(pub &'static str);

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    pub span: Option<Span>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: DiagnosticCode(code),
            message: message.into(),
            span: None,
            notes: Vec::new(),
        }
    }
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }
    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.notes.push(n.into());
        self
    }
}

pub fn render(diag: &Diagnostic, sm: &SourceMap) -> String {
    let mut out = String::new();
    let sev = match diag.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    };
    out.push_str(&format!("{}[{}]: {}\n", sev, diag.code.0, diag.message));
    if let Some(sp) = diag.span {
        let sf = sm.get(sp.file);
        let (l, c) = sf.line_col(sp.start);
        out.push_str(&format!("  --> {}:{}:{}\n", sf.path.display(), l, c));
        // show line
        let line_start = sf.text[..sp.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = sf.text[sp.start..]
            .find('\n')
            .map(|i| sp.start + i)
            .unwrap_or(sf.text.len());
        let line_text = &sf.text[line_start..line_end];
        out.push_str(&format!("   | {}\n", line_text));
        let pad = sp.start - line_start;
        out.push_str(&format!(
            "   | {}{}\n",
            " ".repeat(pad),
            "^".repeat((sp.end - sp.start).max(1))
        ));
    }
    for n in &diag.notes {
        out.push_str(&format!("   note: {}\n", n));
    }
    out
}
