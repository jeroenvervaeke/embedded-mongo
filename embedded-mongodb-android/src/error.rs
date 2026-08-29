use std::any::Any;
use std::fmt;

use embedded_mongodb::Error as EmbeddedError;
use embedded_mongodb_sys::Error as NativeError;
use jni::sys::jint;

/// What reaches Java as `EmbeddedMongoException.code`.
///
/// MongoDB numbers its own errors from 1 upwards, so every code this binding invents for a
/// failure of its own is negative and cannot collide with one. `0` is never used: it is
/// `int`'s default in Java and would be indistinguishable from an unset field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    /// The handle was zero, forged, already closed, or belongs to a previous process. Also
    /// covers `open` having no id left to issue, which is the other way to end up with none.
    ClosedHandle,
    /// An argument was unusable before the engine saw it -- a `null` reference, mostly.
    InvalidArgument,
    /// A Rust panic was caught at the JNI boundary.
    Panic,
    /// The JNI call itself failed: out of memory allocating a `byte[]`, and the like.
    Jni,
    /// The engine failed and its message carried no number. See [`ErrorCode::Mongo`].
    Native,
    /// The engine failed with this MongoDB error code.
    ///
    /// MongoDB spells most codes by name (`BadValue`, `InvalidBSON`) and only the anonymous
    /// `uassert` codes as `Location<n>`, so only the latter arrive as a number; the name is
    /// always the first word of the message either way.
    Mongo(i32),
}

/// One failure on its way to Java, already reduced to what the exception carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeError {
    code: ErrorCode,
    message: String,
}

pub type Result<T> = std::result::Result<T, BridgeError>;

impl ErrorCode {
    pub fn as_java(self) -> jint {
        match self {
            Self::ClosedHandle => -1,
            Self::InvalidArgument => -2,
            Self::Panic => -3,
            Self::Jni => -4,
            Self::Native => -5,
            Self::Mongo(code) => code,
        }
    }
}

impl BridgeError {
    pub fn closed_handle(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ClosedHandle, message)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    /// Turns a caught panic payload into an error, recovering the panic message when the
    /// payload is one of the two shapes `panic!` produces.
    pub fn from_panic(payload: &(dyn Any + Send)) -> Self {
        let described = match payload.downcast_ref::<&'static str>() {
            Some(message) => Some(*message),
            None => payload.downcast_ref::<String>().map(String::as_str),
        };
        Self::new(
            ErrorCode::Panic,
            match described {
                Some(message) => format!("embedded MongoDB binding panicked: {message}"),
                None => "embedded MongoDB binding panicked".to_owned(),
            },
        )
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} (code {})", self.message, self.code.as_java())
    }
}

impl std::error::Error for BridgeError {}

impl From<EmbeddedError> for BridgeError {
    fn from(error: EmbeddedError) -> Self {
        match &error {
            // The C++ bridge stringifies `mongo::DBException` with `Status::toString()`, which
            // is `codeString(): reason`. Reporting the inner exception rather than either
            // crate's Display keeps that prefix -- and the code inside it -- intact.
            EmbeddedError::Native(NativeError::Native(exception)) => {
                let message = exception.what();
                let code = location_code(message).map_or(ErrorCode::Native, ErrorCode::Mongo);
                Self::new(code, message)
            }
            // The bridge passes replies through unread, so nothing it does raises this; the
            // index repair pass is the one caller that reads a reply, and it reports its own
            // failures rather than returning them. Mapped anyway, because a code that reached
            // Java as -5 would be indistinguishable from the engine having named none.
            EmbeddedError::Server { code, .. } => Self::new(server_code(*code), error.to_string()),
            // Not `ClosedHandle`: `Closed` is what a null client produces, which is a failed
            // `open`, and -1 is reserved for a handle the registry could not resolve.
            // Reporting an `open` failure as a stale handle would send the Kotlin side looking
            // for a handle that never existed.
            //
            // `Native(Closed)` cannot occur -- the safe crate folds a closed native client
            // into `Closed` -- and neither can `FreeDiskFloorNotRestored`: the floors are moved
            // from Kotlin over `command`, and the only floor this layer's opens establish is
            // MongoDB's own, which an open reports as it failed rather than putting back. Both
            // are named anyway, because naming them here rather than under a wildcard is what
            // makes a new variant a compile error.
            EmbeddedError::Bson(_)
            | EmbeddedError::Closed
            | EmbeddedError::FreeDiskFloorNotRestored { .. }
            | EmbeddedError::InvalidArgument(_)
            | EmbeddedError::InvalidResponse(_)
            | EmbeddedError::Native(NativeError::Closed)
            | EmbeddedError::NonUtf8Path => Self::new(ErrorCode::Native, error.to_string()),
        }
    }
}

impl From<jni::errors::Error> for BridgeError {
    fn from(error: jni::errors::Error) -> Self {
        Self::new(ErrorCode::Jni, format!("JNI call failed: {error}"))
    }
}

impl BridgeError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The code a reply named, when Java can hold it.
///
/// Same rule as [`location_code`]: anything outside `1..=i32::MAX` falls back to the sentinel,
/// because `0` is what the Kotlin side reads as "the reply named no code" and a truncated
/// `i64` would name a different error entirely.
fn server_code(code: Option<i64>) -> ErrorCode {
    code.and_then(|code| i32::try_from(code).ok())
        .filter(|code| *code > 0)
        .map_or(ErrorCode::Native, ErrorCode::Mongo)
}

/// Reads the number out of a `Location<n>: reason` message, which is how MongoDB renders an
/// error code that has no name of its own.
fn location_code(message: &str) -> Option<i32> {
    let rest = message.strip_prefix("Location")?;
    let end = rest
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, tail) = rest.split_at(end);
    // Insisting on the separator keeps `Location12Foo` from reading as code 12. A bare
    // `Location123` never occurs in practice, but costs nothing to accept.
    if !tail.is_empty() && !tail.starts_with(':') {
        return None;
    }
    // Upholds the promise on `ErrorCode` that `0` never reaches Java: the Kotlin side reads
    // it as "the reply named no code", and a `Location0` would be indistinguishable from it.
    digits.parse().ok().filter(|code| *code > 0)
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::panic::UnwindSafe;

    use super::{BridgeError, EmbeddedError, ErrorCode, location_code, server_code};
    use embedded_mongodb::bson::doc;

    #[test]
    fn reads_the_number_out_of_an_anonymous_mongodb_code() {
        assert_eq!(
            location_code("Location13180000: only one embedded MongoDB runtime may be open"),
            Some(13_180_000)
        );
    }

    #[test]
    fn reads_a_location_code_with_no_reason() {
        assert_eq!(location_code("Location51024"), Some(51_024));
    }

    #[test]
    fn reports_a_null_client_as_an_engine_failure_not_a_stale_handle() {
        // `Client::new` answers `Closed` when the engine hands back nothing; that is a
        // failure to open, and -1 would tell Kotlin its handle had gone stale instead.
        let error = BridgeError::from(EmbeddedError::Closed);
        assert_eq!(error.code(), ErrorCode::Native);
        assert_eq!(error.message(), EmbeddedError::Closed.to_string());
    }

    /// Every variant that is not an engine exception still has to arrive as a description
    /// rather than as an empty message with a sentinel code.
    #[test]
    fn reports_every_other_client_failure_as_an_engine_failure() {
        for error in [
            EmbeddedError::InvalidArgument("no"),
            EmbeddedError::InvalidResponse("no ok field".to_owned()),
            EmbeddedError::NonUtf8Path,
        ] {
            let described = error.to_string();
            let bridged = BridgeError::from(error);
            assert_eq!(bridged.code(), ErrorCode::Native);
            assert_eq!(bridged.message(), described);
        }
    }

    /// A reply the engine refused carries its own number, and that number is what Java is
    /// owed: -5 would say the reply named no code at all.
    #[test]
    fn a_server_error_reaches_java_as_its_mongodb_code() {
        let error = EmbeddedError::Server {
            code: Some(11_000),
            message: "duplicate key".to_owned(),
            response: Box::new(doc! { "ok": 0.0 }),
        };

        let bridged = BridgeError::from(error);

        assert_eq!(bridged.code(), ErrorCode::Mongo(11_000));
        assert!(bridged.message().contains("duplicate key"), "{bridged}");
    }

    #[test]
    fn a_server_code_java_cannot_hold_falls_back_to_the_sentinel() {
        assert_eq!(server_code(None), ErrorCode::Native);
        assert_eq!(server_code(Some(0)), ErrorCode::Native);
        assert_eq!(server_code(Some(-1)), ErrorCode::Native);
        assert_eq!(
            server_code(Some(i64::from(i32::MAX) + 1)),
            ErrorCode::Native
        );
        assert_eq!(server_code(Some(11_000)), ErrorCode::Mongo(11_000));
    }

    #[test]
    fn rejects_a_named_mongodb_code() {
        assert_eq!(location_code("BadValue: unknown operator"), None);
    }

    #[test]
    fn rejects_a_location_prefix_that_is_not_followed_by_a_pure_number() {
        assert_eq!(location_code("LocationUnknown: something"), None);
        assert_eq!(location_code("Location12Foo: something"), None);
    }

    #[test]
    fn rejects_a_location_code_of_zero() {
        // `0` is what the Kotlin side reads as "no code at all".
        assert_eq!(
            location_code("Location0: impossible but unmistakable"),
            None
        );
    }

    #[test]
    fn rejects_a_location_code_too_large_for_an_int() {
        assert_eq!(location_code("Location99999999999: overflowing"), None);
    }

    #[test]
    fn sentinel_codes_never_collide_with_mongodb_codes() {
        for sentinel in [
            ErrorCode::ClosedHandle,
            ErrorCode::InvalidArgument,
            ErrorCode::Panic,
            ErrorCode::Jni,
            ErrorCode::Native,
        ] {
            assert!(sentinel.as_java() < 0, "{sentinel:?} is not negative");
        }
        assert_eq!(ErrorCode::Mongo(13_180_000).as_java(), 13_180_000);
    }

    #[test]
    fn recovers_the_message_of_a_string_literal_panic() {
        let error = BridgeError::from_panic(caught_panic(|| panic!("deliberate")).as_ref());
        assert_eq!(error.code(), ErrorCode::Panic);
        assert!(error.message().contains("deliberate"), "{error}");
    }

    #[test]
    fn recovers_the_message_of_a_formatted_panic() {
        let value = 7;
        let error =
            BridgeError::from_panic(caught_panic(move || panic!("deliberate {value}")).as_ref());
        assert!(error.message().contains("deliberate 7"), "{error}");
    }

    #[test]
    fn describes_a_panic_whose_payload_is_not_a_message() {
        let error = BridgeError::from_panic(caught_panic(|| std::panic::panic_any(7_u32)).as_ref());
        assert_eq!(error.code(), ErrorCode::Panic);
        assert_eq!(error.message(), "embedded MongoDB binding panicked");
    }

    fn caught_panic(body: impl FnOnce() + UnwindSafe) -> Box<dyn Any + Send> {
        let Err(payload) = std::panic::catch_unwind(body) else {
            panic!("the closure under test is required to panic");
        };
        payload
    }
}
