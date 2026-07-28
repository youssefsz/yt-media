//! Shared typed errors for engine boundary composition.

pub use crate::{
    manifest::ManifestError, path::PathValidationError, process::ProcessError, target::TargetError,
    tool::ToolIdentityError,
};
