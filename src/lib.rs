#[cxx::bridge(namespace = "embedded_mongodb")]
mod ffi {
    unsafe extern "C++" {
        include!("embedded-mongodb/bridge.h");

        type EmbeddedMongo;

        fn open(path: &str) -> Result<UniquePtr<EmbeddedMongo>>;
        fn run_command(self: &EmbeddedMongo, database: &str, command: &[u8]) -> Result<Vec<u8>>;
        fn close(self: Pin<&mut EmbeddedMongo>) -> Result<()>;
    }
}

pub use bson;
use bson::Document;
use std::fmt;
use std::path::Path;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Bson(bson::error::Error),
    Closed,
    Native(cxx::Exception),
    NonUtf8Path,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bson(error) => write!(formatter, "BSON error: {error}"),
            Self::Closed => formatter.write_str("embedded MongoDB client is closed"),
            Self::Native(error) => write!(formatter, "embedded MongoDB error: {error}"),
            Self::NonUtf8Path => formatter.write_str("database path is not valid UTF-8"),
        }
    }
}

impl std::error::Error for Error {}

impl From<bson::error::Error> for Error {
    fn from(error: bson::error::Error) -> Self {
        Self::Bson(error)
    }
}

impl From<cxx::Exception> for Error {
    fn from(error: cxx::Exception) -> Self {
        Self::Native(error)
    }
}

pub struct Client {
    inner: cxx::UniquePtr<ffi::EmbeddedMongo>,
}

impl Client {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_str().ok_or(Error::NonUtf8Path)?;
        let inner = ffi::open(path)?;
        if inner.is_null() {
            return Err(Error::Closed);
        }
        Ok(Self { inner })
    }

    pub fn run_command(&self, database: &str, command: &Document) -> Result<Document> {
        let command = command.to_vec()?;
        let client = self.inner.as_ref().ok_or(Error::Closed)?;
        let response = client.run_command(database, &command)?;
        Ok(Document::from_reader(response.as_slice())?)
    }

    pub fn close(mut self) -> Result<()> {
        self.inner.pin_mut().close()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bson::{Bson, doc};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn persists_across_reopen() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("embedded-mongodb-{}-{unique}", std::process::id()));

        let client = super::Client::new(&path).unwrap();
        let response = client
            .run_command(
                "test",
                &doc! {
                    "insert": "items",
                    "documents": [{"_id": 1, "name": "persisted"}],
                },
            )
            .unwrap();
        assert_eq!(response.get("ok"), Some(&Bson::Double(1.0)));
        assert_eq!(response.get_i32("n").unwrap(), 1, "{response:?}");
        client.close().unwrap();

        let client = super::Client::new(&path).unwrap();
        let response = client
            .run_command(
                "test",
                &doc! {
                    "find": "items",
                    "filter": {"_id": 1},
                },
            )
            .unwrap();
        let first_batch = response
            .get_document("cursor")
            .unwrap()
            .get_array("firstBatch")
            .unwrap();
        assert_eq!(first_batch.len(), 1);
        assert_eq!(
            first_batch[0]
                .as_document()
                .unwrap()
                .get_str("name")
                .unwrap(),
            "persisted"
        );
        client.close().unwrap();

        fs::remove_dir_all(path).unwrap();
    }
}
