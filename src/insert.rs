use crate::{Collection, Error, Result};
use bson::{Bson, Document, oid::ObjectId};
use serde::Serialize;
use std::{borrow::Borrow, collections::HashMap};

#[derive(Clone, Debug, PartialEq)]
pub struct InsertOneResult {
    pub inserted_id: Bson,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InsertManyResult {
    pub inserted_ids: HashMap<usize, Bson>,
}

impl<T: Serialize> Collection<'_, T> {
    pub fn insert_one(&self, document: impl Borrow<T>) -> Result<InsertOneResult> {
        let mut document = bson::serialize_to_document(document.borrow())?;
        let inserted_id = ensure_id(&mut document);
        let response = self.client().run_command(
            self.database_name(),
            &bson::doc! {
                "insert": self.name(),
                "documents": [document],
            },
        )?;
        expect_inserted_count(&response, 1)?;
        Ok(InsertOneResult { inserted_id })
    }

    pub fn insert_many(
        &self,
        documents: impl IntoIterator<Item = impl Borrow<T>>,
    ) -> Result<InsertManyResult> {
        let mut serialized = Vec::new();
        let mut inserted_ids = HashMap::new();

        for (index, document) in documents.into_iter().enumerate() {
            let mut document = bson::serialize_to_document(document.borrow())?;
            inserted_ids.insert(index, ensure_id(&mut document));
            serialized.push(Bson::Document(document));
        }
        if serialized.is_empty() {
            return Err(Error::InvalidArgument(
                "insert_many requires at least one document",
            ));
        }

        let expected = serialized.len();
        let response = self.client().run_command(
            self.database_name(),
            &bson::doc! {
                "insert": self.name(),
                "documents": serialized,
            },
        )?;
        expect_inserted_count(&response, expected)?;
        Ok(InsertManyResult { inserted_ids })
    }
}

fn ensure_id(document: &mut Document) -> Bson {
    if let Some(id) = document.get("_id") {
        return id.clone();
    }
    let id = Bson::ObjectId(ObjectId::new());
    document.insert("_id", id.clone());
    id
}

fn expect_inserted_count(response: &Document, expected: usize) -> Result<()> {
    let actual = match response.get("n") {
        Some(Bson::Int32(value)) => usize::try_from(*value).ok(),
        Some(Bson::Int64(value)) => usize::try_from(*value).ok(),
        _ => None,
    }
    .ok_or_else(|| Error::InvalidResponse("insert response has no valid n field".to_owned()))?;

    if actual != expected {
        return Err(Error::InvalidResponse(format!(
            "inserted {actual} documents, expected {expected}"
        )));
    }
    Ok(())
}
