use super::{Collection, engine::Engine};
use crate::find::{
    find_command, find_one_command, get_more_command, kill_cursors_command, take_cursor_batch,
};
use crate::{Error, Result};
use bson::Document;
use serde::de::DeserializeOwned;
use std::{collections::VecDeque, marker::PhantomData};

impl<T: DeserializeOwned> Collection<'_, T> {
    pub async fn find_one(&self, filter: Document) -> Result<Option<T>> {
        let response = self
            .client()
            .run_command(self.database_name(), find_one_command(self.name(), filter))
            .await?;
        let mut cursor = Cursor::<T>::from_response(
            self.client().engine().clone(),
            self.database_name(),
            self.name(),
            response,
            "firstBatch",
        )?;
        cursor.next().await.transpose()
    }

    pub async fn find(&self, filter: Document) -> Result<Cursor<T>> {
        let response = self
            .client()
            .run_command(self.database_name(), find_command(self.name(), filter))
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

/// [`blocking::Cursor`](crate::blocking::Cursor) with the batches fetched across awaits.
///
/// It holds its own handle on the engine's worker pool rather than a borrow of the
/// [`Client`](super::Client), so it can be held across awaits and moved between tasks
/// freely; what it cannot do is outlive a [`Client::close`](super::Client::close), after
/// which its next batch answers [`Error::Closed`].
pub struct Cursor<T> {
    engine: Engine,
    database: String,
    collection: String,
    id: i64,
    documents: VecDeque<Document>,
    finished: bool,
    document_type: PhantomData<fn() -> T>,
}

impl<T> Cursor<T> {
    pub(super) fn from_response(
        engine: Engine,
        database: &str,
        collection: &str,
        response: Document,
        batch_name: &str,
    ) -> Result<Self> {
        let (id, documents) = take_cursor_batch(response, batch_name)?;
        Ok(Self {
            engine,
            database: database.to_owned(),
            collection: collection.to_owned(),
            id,
            documents,
            finished: false,
            document_type: PhantomData,
        })
    }

    async fn fetch_next_batch(&mut self) -> Result<()> {
        let command = get_more_command(self.id, &self.collection);
        let database = self.database.clone();
        let response = self
            .engine
            .run_cancellable(move |client, slot| {
                client.run_command_cancellable(slot, &database, &command)
            })
            .await?;
        let (id, documents) = take_cursor_batch(response, "nextBatch")?;
        self.id = id;
        self.documents = documents;
        Ok(())
    }
}

impl<T: DeserializeOwned> Cursor<T> {
    /// The next document, or `None` once the cursor is exhausted -- the async shape of the
    /// blocking cursor's `Iterator`.
    pub async fn next(&mut self) -> Option<Result<T>> {
        loop {
            if let Some(document) = self.documents.pop_front() {
                return Some(bson::deserialize_from_document(document).map_err(Error::from));
            }
            if self.finished || self.id == 0 {
                self.finished = true;
                return None;
            }
            if let Err(error) = self.fetch_next_batch().await {
                self.finished = true;
                return Some(Err(error));
            }
        }
    }

    pub async fn try_collect(mut self) -> Result<Vec<T>> {
        let mut collected = Vec::new();
        while let Some(document) = self.next().await {
            collected.push(document?);
        }
        Ok(collected)
    }
}

impl<T> Drop for Cursor<T> {
    fn drop(&mut self) {
        let id = std::mem::replace(&mut self.id, 0);
        if id == 0 {
            return;
        }
        // Nothing a Drop could await on, so the kill is detached: it runs when a worker gets
        // to it, and a cursor dropped after the client closed leaves nothing behind for it to
        // kill anyway.
        let command = kill_cursors_command(&self.collection, id);
        let database = std::mem::take(&mut self.database);
        self.engine.run_detached(move |client| {
            let _ = client.run_command(&database, &command);
        });
    }
}
