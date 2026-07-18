use thiserror::Error;

pub type Result<T> = std::result::Result<T, RelayError>;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML decode error: {0}")]
    TomlDecode(#[from] toml::de::Error),

    #[error("TOML encode error: {0}")]
    TomlEncode(#[from] toml::ser::Error),

    #[error("ledger error: {0}")]
    Ledger(#[from] rusqlite::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("unknown model or alias '{0}'")]
    UnknownModel(String),

    #[error("unsupported provider '{0}'")]
    UnsupportedProvider(String),

    #[error("missing provider credential; set environment variable {0}")]
    MissingCredential(String),

    #[error("invalid price entry for '{model}': {reason}")]
    InvalidPrice { model: String, reason: String },

    #[error(
        "{period} cap exceeded: spent {spent_microusd} µUSD, reserved {reserved_microusd} µUSD, request needs {request_microusd} µUSD, cap is {cap_microusd} µUSD; no provider request was sent"
    )]
    CapExceeded {
        period: &'static str,
        spent_microusd: i64,
        reserved_microusd: i64,
        request_microusd: i64,
        cap_microusd: i64,
    },

    #[error(
        "streaming is not supported in Relay v0.1 because partial-response usage cannot yet be settled safely"
    )]
    StreamingUnsupported,

    #[error(
        "request requires up to {requested_tokens} tokens but model context is {max_context} tokens"
    )]
    ContextExceeded {
        requested_tokens: u64,
        max_context: u64,
    },

    #[error("provider '{provider}' rejected the request ({status}): {message}")]
    Provider {
        provider: String,
        status: u16,
        message: String,
    },

    #[error("provider returned invalid usage data: {0}")]
    InvalidUsage(String),

    #[error("reservation '{0}' was not found or is no longer active")]
    ReservationUnavailable(String),

    #[error(
        "provider outcome is ambiguous; reservation {reservation_id} remains held for reconciliation: {reason}"
    )]
    PendingReconciliation {
        reservation_id: String,
        reason: String,
    },

    #[error("amount is outside Relay's supported microdollar range: {0}")]
    AmountOutOfRange(String),
}
