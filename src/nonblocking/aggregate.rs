use super::{Collection, Cursor};
use crate::Result;
use crate::aggregate::aggregate_command;
use bson::Document;

impl<T> Collection<'_, T> {
    pub async fn aggregate(
        &self,
        pipeline: impl IntoIterator<Item = Document>,
    ) -> Result<Cursor<Document>> {
        let response = self
            .client()
            .run_command(
                self.database_name(),
                aggregate_command(self.name(), pipeline),
            )
            .await?;
        Cursor::from_response(
            self.client().engine().clone(),
            self.database_name(),
            self.name(),
            response,
            "firstBatch",
        )
    }
}
