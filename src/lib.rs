#[cfg(feature = "service")]
mod admin;
#[cfg(feature = "service")]
pub mod admin_session;
#[cfg(feature = "service")]
pub mod api;
#[cfg(feature = "service")]
pub mod auth;
pub mod backup;
pub mod build_info;
pub mod document;
pub mod document_management;
pub mod engine;
pub mod error;
pub mod execution;
#[cfg(feature = "service")]
pub mod metrics;
pub mod namespace;
#[cfg(feature = "service")]
pub mod password_cli;
pub mod registry;
pub mod result_schema;
pub mod store;
pub mod template_check;
#[cfg(feature = "service")]
pub mod tenancy;

pub mod routing_eval;
