use super::Collection;
use crate::insert::{ensure_id, expect_inserted_count, insert_command, serialize_many};
use crate::{InsertManyResult, InsertOneResult, Result};
use bson::Bson;
use serde::Serialize;
use std::borrow::Borrow;

impl<T: Serialize> Collection<'_, T> {
    pub async fn insert_one(&self, document: impl Borrow<T>) -> Result<InsertOneResult> {
        // Serialized here rather than on the worker, so `T` owes no `Send` bound and a
        // serialization failure never crosses a thread to come back.
        let mut document = bson::serialize_to_document(document.borrow())?;
        let inserted_id = ensure_id(&mut document);
        let response = self
            .client()
            .run_command(
                self.database_name(),
                insert_command(self.name(), vec![Bson::Document(document)]),
            )
            .await?;
        expect_inserted_count(&response, 1)?;
        Ok(InsertOneResult { inserted_id })
    }

    pub async fn insert_many(
        &self,
        documents: impl IntoIterator<Item = impl Borrow<T>>,
    ) -> Result<InsertManyResult> {
        let (serialized, inserted_ids) = serialize_many(documents)?;
        let expected = serialized.len();
        let response = self
            .client()
            .run_command(
                self.database_name(),
                insert_command(self.name(), serialized),
            )
            .await?;
        expect_inserted_count(&response, expected)?;
        Ok(InsertManyResult { inserted_ids })
    }
}
