pub type ParseResult<T> = std::result::Result<T, ParseError>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub(crate) String);

impl ParseError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}
