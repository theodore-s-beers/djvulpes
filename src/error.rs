use std::fmt;

pub type Result<T> = std::result::Result<T, ParseError>;

#[derive(Debug)]
pub struct ParseError(pub(crate) String);

impl ParseError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}
