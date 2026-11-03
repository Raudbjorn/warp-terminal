/// The line terminator a text predominantly uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        if crlf > 0 && crlf >= lf {
            Self::CrLf
        } else {
            Self::Lf
        }
    }

    /// Rewrites every line terminator in `text` to this one.
    pub fn apply(self, text: String) -> String {
        if self == Self::Lf && !text.contains("\r\n") {
            return text;
        }
        let normalized = text.replace("\r\n", "\n");
        match self {
            Self::Lf => normalized,
            Self::CrLf => normalized.replace('\n', "\r\n"),
        }
    }
}

#[cfg(test)]
#[path = "line_ending_tests.rs"]
mod tests;
