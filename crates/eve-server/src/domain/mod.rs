//! Protocol-agnostic domain services shared across MachoNet RPCs, Quasar gRPC/protobuf
//! handlers, and ESI HTTP routes (`#593`).
//!
//! Domain functions operate on [`crate::router::ServerContext`] / world snapshots and return
//! typed Rust structs and [`DomainError`], without depending on `PyRep`, `serde_json::Value`,
//! `RouteExecutionResult`, or Quasar protobuf messages.

pub mod career_goals;
pub mod corp_governance;
pub mod fleets;
pub mod mail;
pub mod market_acl;

#[cfg(test)]
mod parity_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainErrorKind {
    InvalidInput,
    NotFound,
    Forbidden,
    Conflict,
    PreconditionFailed,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomainError {
    pub kind: DomainErrorKind,
    pub code: String,
    pub message: String,
    pub http_status_override: Option<u16>,
}

impl DomainError {
    pub fn invalid_input(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::InvalidInput,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::NotFound,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::Forbidden,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::Conflict,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn precondition_failed(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::PreconditionFailed,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn internal(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: DomainErrorKind::Internal,
            code: code.into(),
            message: message.into(),
            http_status_override: None,
        }
    }

    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status_override = Some(status);
        self
    }

    pub fn http_status(&self) -> u16 {
        if let Some(status) = self.http_status_override {
            return status;
        }
        match self.kind {
            DomainErrorKind::InvalidInput => 400,
            DomainErrorKind::Forbidden => 403,
            DomainErrorKind::NotFound => 404,
            DomainErrorKind::Conflict => 409,
            DomainErrorKind::PreconditionFailed => 420,
            DomainErrorKind::Internal => 500,
        }
    }
}

impl eve_db::FromWalError for DomainError {
    fn from_wal_error(err: String) -> Self {
        Self::internal("PersistenceFailure", err)
    }
}
