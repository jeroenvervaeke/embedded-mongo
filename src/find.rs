use crate::{Client, Collection, Error, Result};
use bson::{Bson, Document};
use serde::de::DeserializeOwned;
use std::{collections::VecDeque, marker::PhantomData};

impl<'client, T: DeserializeOwned> Collection<'client, T> {
    pub fn find_one(&self, filter: Document) -> Result<Option<T>> {
        let response = self.client().run_command(
            self.database_name(),
            &bson::doc! {
                "find": self.name(),
                "filter": filter,
                "limit": 1_i64,
                "singleBatch": true,
            },
        )?;
        let mut cursor = Cursor::<T>::from_response(
            self.client(),
            self.database_name(),
            self.name(),
            response,
            "firstBatch",
        )?;
        cursor.next().transpose()
    }

    pub fn find(&self, filter: Document) -> Result<Cursor<'client, T>> {
        let response = self.client().run_command(
            self.database_name(),
            &bson::doc! {
                "find": self.name(),
                "filter": filter,
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

pub struct Cursor<'client, T> {
    client: &'client Client,
    database: String,
    collection: String,
    id: i64,
    documents: VecDeque<Document>,
    finished: bool,
    document_type: PhantomData<fn() -> T>,
}

impl<'client, T> Cursor<'client, T> {
    pub(crate) fn from_response(
        client: &'client Client,
        database: &str,
        collection: &str,
        response: Document,
        batch_name: &str,
    ) -> Result<Self> {
        let (id, documents) = take_cursor_batch(response, batch_name)?;
        Ok(Self {
            client,
            database: database.to_owned(),
            collection: collection.to_owned(),
            id,
            documents,
            finished: false,
            document_type: PhantomData,
        })
    }
}

impl<T> Cursor<'_, T> {
    fn fetch_next_batch(&mut self) -> Result<()> {
        let response = self.client.run_command(
            &self.database,
            &bson::doc! {
                "getMore": self.id,
                "collection": self.collection.as_str(),
            },
        )?;
        let (id, documents) = take_cursor_batch(response, "nextBatch")?;
        self.id = id;
        self.documents = documents;
        Ok(())
    }

    fn kill(&mut self) -> Result<()> {
        let id = std::mem::replace(&mut self.id, 0);
        if id == 0 {
            return Ok(());
        }
        self.client.run_command(
            &self.database,
            &bson::doc! {
                "killCursors": self.collection.as_str(),
                "cursors": [id],
            },
        )?;
        Ok(())
    }
}

impl<T: DeserializeOwned> Cursor<'_, T> {
    pub fn try_collect(self) -> Result<Vec<T>> {
        self.collect()
    }
}

impl<T: DeserializeOwned> Iterator for Cursor<'_, T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(document) = self.documents.pop_front() {
                return Some(bson::deserialize_from_document(document).map_err(Error::from));
            }
            if self.finished || self.id == 0 {
                self.finished = true;
                return None;
            }
            if let Err(error) = self.fetch_next_batch() {
                self.finished = true;
                return Some(Err(error));
            }
        }
    }
}

impl<T> Drop for Cursor<'_, T> {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

fn take_cursor_batch(
    mut response: Document,
    batch_name: &str,
) -> Result<(i64, VecDeque<Document>)> {
    let mut cursor = match response.remove("cursor") {
        Some(Bson::Document(cursor)) => cursor,
        _ => {
            return Err(Error::InvalidResponse(
                "command response has no cursor document".to_owned(),
            ));
        }
    };
    let id = match cursor.remove("id") {
        Some(Bson::Int64(id)) => id,
        Some(Bson::Int32(id)) => i64::from(id),
        _ => {
            return Err(Error::InvalidResponse(
                "cursor response has no valid id".to_owned(),
            ));
        }
    };
    let batch = match cursor.remove(batch_name) {
        Some(Bson::Array(batch)) => batch,
        _ => {
            return Err(Error::InvalidResponse(format!(
                "cursor response has no {batch_name} array"
            )));
        }
    };
    let mut documents = VecDeque::with_capacity(batch.len());
    for value in batch {
        match value {
            Bson::Document(document) => documents.push_back(document),
            _ => {
                return Err(Error::InvalidResponse(format!(
                    "cursor {batch_name} contains a non-document value"
                )));
            }
        }
    }
    Ok((id, documents))
}
