use crate::{Collection, Cursor, Result};
use bson::{Bson, Document};

impl<'client, T> Collection<'client, T> {
    pub fn aggregate(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
    ) -> Result<Cursor<'client, Document>> {
        let pipeline = pipeline.into_iter().map(Bson::Document).collect::<Vec<_>>();
        let response = self.client().run_command(
            self.database_name(),
            &bson::doc! {
                "aggregate": self.name(),
                "pipeline": pipeline,
                "cursor": Document::new(),
            },
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
