use super::{Client, Collection};
use crate::Result;
use bson::Document;

pub struct Database<'client> {
    client: &'client Client,
    name: String,
}

impl<'client> Database<'client> {
    pub(super) fn new(client: &'client Client, name: &str) -> Self {
        Self {
            client,
            name: name.to_owned(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn collection<T>(&self, name: &str) -> Collection<'client, T> {
        Collection::new(self.client, self.name.clone(), name)
    }

    pub async fn run_command(&self, command: Document) -> Result<Document> {
        self.client.run_command(&self.name, command).await
    }
}
