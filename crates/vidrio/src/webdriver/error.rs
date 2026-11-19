//! The errors of W3C WebDriver (§6.6 "Errors"): each code with its HTTP
//! status and JSON name.

use super::json::Json;

/// An error code of WebDriver §6.6.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    ElementClickIntercepted,
    ElementNotInteractable,
    InsecureCertificate,
    InvalidArgument,
    InvalidCookieDomain,
    InvalidElementState,
    InvalidSelector,
    InvalidSessionId,
    JavascriptError,
    MoveTargetOutOfBounds,
    NoSuchAlert,
    NoSuchCookie,
    NoSuchElement,
    NoSuchFrame,
    NoSuchWindow,
    NoSuchShadowRoot,
    ScriptTimeout,
    SessionNotCreated,
    StaleElementReference,
    DetachedShadowRoot,
    Timeout,
    UnableToSetCookie,
    UnexpectedAlertOpen,
    UnknownCommand,
    UnknownError,
    UnknownMethod,
    UnsupportedOperation,
}

impl ErrorCode {
    /// The HTTP status of the error.
    pub fn status(self) -> u16 {
        use ErrorCode::*;
        match self {
            ElementClickIntercepted
            | ElementNotInteractable
            | InsecureCertificate
            | InvalidArgument
            | InvalidCookieDomain
            | InvalidElementState
            | InvalidSelector => 400,
            InvalidSessionId
            | NoSuchAlert
            | NoSuchCookie
            | NoSuchElement
            | NoSuchFrame
            | NoSuchWindow
            | NoSuchShadowRoot
            | StaleElementReference
            | DetachedShadowRoot
            | UnknownCommand => 404,
            UnknownMethod => 405,
            JavascriptError
            | MoveTargetOutOfBounds
            | ScriptTimeout
            | SessionNotCreated
            | Timeout
            | UnableToSetCookie
            | UnexpectedAlertOpen
            | UnknownError
            | UnsupportedOperation => 500,
        }
    }

    /// The JSON error code.
    pub fn name(self) -> &'static str {
        use ErrorCode::*;
        match self {
            ElementClickIntercepted => "element click intercepted",
            ElementNotInteractable => "element not interactable",
            InsecureCertificate => "insecure certificate",
            InvalidArgument => "invalid argument",
            InvalidCookieDomain => "invalid cookie domain",
            InvalidElementState => "invalid element state",
            InvalidSelector => "invalid selector",
            InvalidSessionId => "invalid session id",
            JavascriptError => "javascript error",
            MoveTargetOutOfBounds => "move target out of bounds",
            NoSuchAlert => "no such alert",
            NoSuchCookie => "no such cookie",
            NoSuchElement => "no such element",
            NoSuchFrame => "no such frame",
            NoSuchWindow => "no such window",
            NoSuchShadowRoot => "no such shadow root",
            ScriptTimeout => "script timeout",
            SessionNotCreated => "session not created",
            StaleElementReference => "stale element reference",
            DetachedShadowRoot => "detached shadow root",
            Timeout => "timeout",
            UnableToSetCookie => "unable to set cookie",
            UnexpectedAlertOpen => "unexpected alert open",
            UnknownCommand => "unknown command",
            UnknownError => "unknown error",
            UnknownMethod => "unknown method",
            UnsupportedOperation => "unsupported operation",
        }
    }
}

/// An error with its message.
#[derive(Clone, Debug, PartialEq)]
pub struct WdError {
    pub code: ErrorCode,
    pub message: String,
}

impl WdError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> WdError {
        WdError {
            code,
            message: message.into(),
        }
    }

    /// The `value` of the error response.
    pub fn to_json(&self) -> Json {
        Json::object([
            ("error", Json::str(self.code.name())),
            ("message", Json::String(self.message.clone())),
            ("stacktrace", Json::str("")),
        ])
    }
}

pub type WdResult<T> = Result<T, WdError>;

/// Shorthand for an `invalid argument` error.
pub fn invalid_argument<T>(message: &str) -> WdResult<T> {
    Err(WdError::new(ErrorCode::InvalidArgument, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_follow_the_error_table() {
        assert_eq!(ErrorCode::NoSuchWindow.status(), 404);
        assert_eq!(ErrorCode::UnknownCommand.status(), 404);
        assert_eq!(ErrorCode::InvalidArgument.status(), 400);
        assert_eq!(ErrorCode::ScriptTimeout.status(), 500);
        assert_eq!(ErrorCode::UnknownMethod.status(), 405);
        assert_eq!(
            WdError::new(ErrorCode::NoSuchWindow, "gone")
                .to_json()
                .serialize(),
            r#"{"error":"no such window","message":"gone","stacktrace":""}"#
        );
    }
}
