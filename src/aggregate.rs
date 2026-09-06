use crate::{Result, collection::Collection, find::Cursor};
use bson::{Bson, Document};

impl<'client, T> Collection<'client, T> {
    pub fn aggregate(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
    ) -> Result<Cursor<'client, Document>> {
        let response = self.client().run_command(
            self.database_name(),
            &aggregate_command(self.name(), pipeline),
        )?;
        Cursor::from_response(
            self.client(),
            self.database_name(),
            self.name(),
            response,
            "firstBatch",
        )
    }
}

/// Shared with the async layer in `crate::nonblocking`, like the builders in `find` and
/// `insert`.
pub(crate) fn aggregate_command(
    collection: &str,
    pipeline: impl IntoIterator<Item = Document>,
) -> Document {
    let pipeline = pipeline.into_iter().map(Bson::Document).collect::<Vec<_>>();
    bson::doc! {
        "aggregate": collection,
        "pipeline": pipeline,
        "cursor": Document::new(),
    }
}
