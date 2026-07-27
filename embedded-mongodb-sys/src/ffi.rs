#[cxx::bridge(namespace = "embedded_mongodb")]
pub(crate) mod bridge {
    extern "Rust" {
        fn emit_mongodb_log(
            severity: i32,
            id: i32,
            component: &str,
            context: &str,
            message: &str,
            record: &str,
        );
    }

    unsafe extern "C++" {
        include!("embedded-mongodb/bridge.h");

        type EmbeddedMongo;

        fn open(path: &str) -> Result<UniquePtr<EmbeddedMongo>>;
        fn run_command(self: &EmbeddedMongo, database: &str, command: &[u8]) -> Result<Vec<u8>>;
        fn close(self: Pin<&mut EmbeddedMongo>) -> Result<()>;
    }
}

fn mongodb_log_level(severity: i32) -> tracing::Level {
    match severity {
        ..=-3 => tracing::Level::ERROR,
        -2 => tracing::Level::WARN,
        -1 | 0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    }
}

fn emit_mongodb_log(
    severity: i32,
    id: i32,
    component: &str,
    context: &str,
    message: &str,
    record: &str,
) {
    macro_rules! emit {
        ($level:expr) => {
            tracing::event!(
                target: "embedded_mongodb::mongo",
                $level,
                mongodb_id = id,
                mongodb_component = component,
                mongodb_context = context,
                mongodb_severity = severity,
                mongodb_record = record,
                "{message}"
            )
        };
    }

    match mongodb_log_level(severity) {
        tracing::Level::ERROR => emit!(tracing::Level::ERROR),
        tracing::Level::WARN => emit!(tracing::Level::WARN),
        tracing::Level::INFO => emit!(tracing::Level::INFO),
        tracing::Level::DEBUG => emit!(tracing::Level::DEBUG),
        tracing::Level::TRACE => emit!(tracing::Level::TRACE),
    }
}

#[cfg(test)]
mod tests {
    use super::mongodb_log_level;
    use tracing::Level;

    #[test]
    fn maps_mongodb_log_levels() {
        assert_eq!(mongodb_log_level(-4), Level::ERROR);
        assert_eq!(mongodb_log_level(-3), Level::ERROR);
        assert_eq!(mongodb_log_level(-2), Level::WARN);
        assert_eq!(mongodb_log_level(-1), Level::INFO);
        assert_eq!(mongodb_log_level(0), Level::INFO);
        assert_eq!(mongodb_log_level(1), Level::DEBUG);
        assert_eq!(mongodb_log_level(5), Level::TRACE);
    }
}
