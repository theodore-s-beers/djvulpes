use std::fmt;

pub type ParseResult<T> = std::result::Result<T, ParseError>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseError(pub(crate) String);

impl ParseError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}
