#[cxx::bridge(namespace = "embedded_mongodb")]
pub(crate) mod bridge {
    unsafe extern "C++" {
        include!("embedded-mongodb/bridge.h");

        type EmbeddedMongo;

        fn open(path: &str) -> Result<UniquePtr<EmbeddedMongo>>;
        fn run_command(self: &EmbeddedMongo, database: &str, command: &[u8]) -> Result<Vec<u8>>;
        fn close(self: Pin<&mut EmbeddedMongo>) -> Result<()>;
    }
}
