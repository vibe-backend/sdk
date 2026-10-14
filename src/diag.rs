use std::fmt;

/// One validator or compiler finding.
///
/// `file`, `section`, `step`, and `lines` are empty when they do not apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub level: Level,
    pub file: String,
    pub section: String,
    pub step: String,
    pub lines: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

impl Diag {
    pub fn error(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: Level::Error,
            file: file.into(),
            section: String::new(),
            step: String::new(),
            lines: String::new(),
            message: message.into(),
        }
    }

    pub fn warning(file: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: Level::Warning,
            file: file.into(),
            section: String::new(),
            step: String::new(),
            lines: String::new(),
            message: message.into(),
        }
    }

    pub fn at_section(mut self, section: impl Into<String>) -> Self {
        self.section = section.into();
        self
    }

    pub fn at_step(mut self, step: impl Into<String>) -> Self {
        self.step = step.into();
        self
    }

    pub fn at_lines(mut self, lines: impl Into<String>) -> Self {
        self.lines = lines.into();
        self
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.level {
            Level::Error => "error",
            Level::Warning => "warning",
        };
        write!(f, "{level}: {}", self.file)?;
        if !self.section.is_empty() {
            write!(f, " [{}]", self.section)?;
        }
        if !self.step.is_empty() {
            write!(f, " ({})", self.step)?;
        }
        if !self.lines.is_empty() {
            write!(f, " lines {}", self.lines)?;
        }
        write!(f, ": {}", self.message)
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub diags: Vec<Diag>,
}

impl Report {
    pub fn new() -> Self {
        Self { diags: Vec::new() }
    }

    pub fn push(&mut self, diag: Diag) {
        self.diags.push(diag);
    }

    pub fn extend(&mut self, diags: Vec<Diag>) {
        self.diags.extend(diags);
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diag> {
        self.diags.iter().filter(|d| d.level == Level::Warning)
    }

    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    pub fn into_result(self) -> Result<Self, Self> {
        if self.has_errors() {
            Err(self)
        } else {
            Ok(self)
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, diag) in self.diags.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{diag}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Report {}
