use super::Client;
use bson::Document;
use std::marker::PhantomData;

pub struct Collection<'client, T = Document> {
    client: &'client Client,
    database: String,
    name: String,
    document_type: PhantomData<fn() -> T>,
}

impl<'client, T> Collection<'client, T> {
    pub(super) fn new(client: &'client Client, database: String, name: &str) -> Self {
        Self {
            client,
            database,
            name: name.to_owned(),
            document_type: PhantomData,
        }
    }

    pub(super) fn client(&self) -> &'client Client {
        self.client
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn database_name(&self) -> &str {
        &self.database
    }
}
