#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("embedded MongoDB client is closed")]
    Closed,
    #[error("embedded MongoDB error: {0}")]
    Native(#[from] cxx::Exception),
}

pub type Result<T> = std::result::Result<T, Error>;
