//! Server-side apply pipeline: payload parse, SSA orchestration, error
//! parsing, and error-to-line mapping. See #21 (with #10/#15 resolutions)
//! for the design this module executes.

pub mod error;
pub mod fieldpath;
pub mod locate;
pub mod payload;
pub mod run;

pub use error::{ApplyError, FieldCause, ManagerConflict, parse_status};
pub use fieldpath::{FieldPathError, Segment, parse_field_path};
pub use locate::{Located, Location, locate_field};
pub use payload::{DocPayload, PayloadError, parse_documents};
pub use run::{
    ApplyReport, DocError, DocOutcome, FIELD_MANAGER, LocatedCause, apply_documents,
    precheck_documents,
};
