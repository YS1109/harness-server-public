use thiserror::Error;

/// Fixed, content-free diagnostics; independent of the public HTTP error code.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ValidationReason {
    FieldFormat,
    BodyBudget,
    OtherValidation,
    #[serde(other)]
    Unclassified,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{field}: expected {expected}; actual {actual}; 未保存")]
    Field {
        field: String,
        expected: String,
        actual: serde_json::Value,
    },
    #[error("{kind}正文超过{limit}字（实际{actual}字）；请精简后重新提交，未保存")]
    Budget {
        kind: String,
        path: String,
        actual: usize,
        limit: usize,
    },
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("存储操作失败；请沿用相同幂等键重试")]
    Storage(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
pub fn conflict(message: impl Into<String>) -> Error {
    Error::Conflict(message.into())
}

impl Error {
    pub fn validation_reason(&self) -> Option<ValidationReason> {
        match self {
            Self::Field { .. } => Some(ValidationReason::FieldFormat),
            Self::Budget { .. } => Some(ValidationReason::BodyBudget),
            Self::Invalid(_) => Some(ValidationReason::OtherValidation),
            _ => None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Forbidden(_) => "forbidden",
            Self::Invalid(_) | Self::Field { .. } | Self::Budget { .. } => "validation",
            Self::Conflict(_) => "conflict",
            Self::NotFound(_) => "not_found",
            Self::Storage(_) => "storage",
        }
    }
    pub fn details(&self) -> serde_json::Value {
        match self {
            Self::Field {
                field,
                expected,
                actual,
            } => {
                serde_json::json!({"field":field,"expected":expected,"actual":actual,"saved":false})
            }
            Self::Budget {
                kind,
                path,
                actual,
                limit,
            } => {
                serde_json::json!({"field":"body","kind":kind,"path":path,"actual":actual,"limit":limit,"unit":"characters","saved":false,"retry":"精简正文后以新幂等键提交；不自动截断"})
            }
            Self::Storage(_) => {
                serde_json::json!({"saved":null,"retry":"结果未确认；同一正文和首次幂等键重试"})
            }
            _ => serde_json::json!({"saved":false}),
        }
    }
}
