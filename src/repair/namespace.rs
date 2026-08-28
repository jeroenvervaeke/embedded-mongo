use std::fmt;

/// One collection the repair pass works on.
///
/// Resolved once, where `listCollections` is read, so that nothing further in returns to a
/// `Document` to ask what it is looking at, and so a database name and a collection name can
/// never be handed over in the wrong order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Namespace {
    database: String,
    collection: String,
}

impl Namespace {
    pub(crate) fn new(database: impl Into<String>, collection: impl Into<String>) -> Self {
        Self {
            database: database.into(),
            collection: collection.into(),
        }
    }

    pub(crate) fn database(&self) -> &str {
        &self.database
    }

    pub(crate) fn collection(&self) -> &str {
        &self.collection
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.database, self.collection)
    }
}

#[cfg(test)]
mod tests {
    use super::Namespace;

    #[test]
    fn displays_as_a_dotted_namespace() {
        let namespace = Namespace::new("shop", "orders");

        assert_eq!(namespace.to_string(), "shop.orders");
    }

    #[test]
    fn keeps_the_database_and_the_collection_apart() {
        let namespace = Namespace::new("local", "lost_and_found.abc");

        assert_eq!(namespace.database(), "local");
        assert_eq!(namespace.collection(), "lost_and_found.abc");
    }
}
