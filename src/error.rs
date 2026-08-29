use crate::FreeDiskFloor;
use bson::{Bson, Document};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("BSON error: {0}")]
    Bson(#[from] bson::error::Error),
    #[error("embedded MongoDB client is closed")]
    Closed,
    /// A free-disk floor that could not be applied and could not be put back either, so the
    /// engine is on floors nobody chose.
    ///
    /// Materially different from an ordinary failure, and separate because of it: a caller who
    /// catches "your request failed" reasonably assumes nothing happened, and here something
    /// did. The two knobs take two commands, so a move stopped between them leaves one of them
    /// somewhere neither the caller nor this library asked for -- and downward, in the case
    /// that matters. An application that would rather refuse work than do it against an unknown
    /// floor catches this and refuses; both reasons are carried so it can say why.
    #[error(
        "the free disk floor could not be moved to {mebibytes} MiB and the floors could not be \
         put back either, so the engine is on floors nobody chose -- the move failed with: \
         {cause}; putting them back failed with: {rollback}",
        mebibytes = .requested.mebibytes()
    )]
    FreeDiskFloorNotRestored {
        /// The floor the caller asked for, which is not the one in force.
        requested: FreeDiskFloor,
        /// Why the move failed.
        #[source]
        cause: Box<Error>,
        /// Why putting the floors back failed.
        rollback: Box<Error>,
    },
    #[error("{0}")]
    InvalidArgument(&'static str),
    #[error("invalid embedded MongoDB response: {0}")]
    InvalidResponse(String),
    #[error(transparent)]
    Native(embedded_mongodb_sys::Error),
    #[error("database path is not valid UTF-8")]
    NonUtf8Path,
    #[error(
        "MongoDB error{code}: {message}",
        code = .code.map(|code| format!(" {code}")).unwrap_or_default()
    )]
    Server {
        code: Option<i64>,
        message: String,
        response: Box<Document>,
    },
}

impl From<embedded_mongodb_sys::Error> for Error {
    fn from(error: embedded_mongodb_sys::Error) -> Self {
        match error {
            embedded_mongodb_sys::Error::Closed => Self::Closed,
            error => Self::Native(error),
        }
    }
}

pub(crate) fn validate_response(response: Document) -> Result<Document> {
    let ok = match response.get("ok") {
        Some(Bson::Double(ok)) => *ok,
        Some(Bson::Int32(ok)) => f64::from(*ok),
        Some(Bson::Int64(ok)) => *ok as f64,
        _ => {
            return Err(Error::InvalidResponse(
                "command response has no valid ok field".to_owned(),
            ));
        }
    };

    let has_write_errors =
        matches!(response.get("writeErrors"), Some(Bson::Array(errors)) if !errors.is_empty());
    if ok == 0.0 || has_write_errors || response.contains_key("writeConcernError") {
        return Err(server_error(response));
    }
    Ok(response)
}

fn server_error(response: Document) -> Error {
    let details = response
        .get_array("writeErrors")
        .ok()
        .and_then(|errors| errors.first())
        .and_then(Bson::as_document)
        .or_else(|| response.get_document("writeConcernError").ok())
        .unwrap_or(&response);
    let code = match details.get("code") {
        Some(Bson::Int32(code)) => Some(i64::from(*code)),
        Some(Bson::Int64(code)) => Some(*code),
        _ => None,
    };
    let message = details
        .get_str("errmsg")
        .unwrap_or("MongoDB command failed")
        .to_owned();
    Error::Server {
        code,
        message,
        response: Box::new(response),
    }
}
