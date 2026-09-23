//! Failure causes: a closed set, with one sentence each.
//!
//! The cause owns both the wording and the key a notice is de-duplicated by, so the same failure
//! reads the same wherever it surfaces. Anything unrecognised keeps the system's message exactly
//! rather than guessing.

use std::io;
use std::path::Path;

/// What the caller was attempting — it disambiguates errors that mean different things for
/// different operations (a permission error on a lock is "in use", on a read is "no access").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Write,
    Create,
    Rename,
    Delete,
    List,
    Watch,
    Spawn,
    Lock,
}

impl Operation {
    fn verb(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "save",
            Self::Create => "create",
            Self::Rename => "move",
            Self::Delete => "delete",
            Self::List => "list",
            Self::Watch => "watch",
            Self::Spawn => "start",
            Self::Lock => "lock",
        }
    }
}

/// The closed set of causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    NotFound,
    NoAccess,
    InUse,
    AlreadyExists,
    NoSpace,
    ReadOnly,
    /// Not recognised: the raw message is shown as-is.
    Other,
}

/// Classify an I/O error for `operation`.
#[must_use]
pub fn classify(error: &io::Error, operation: Operation) -> Cause {
    use io::ErrorKind as K;
    match error.kind() {
        K::NotFound => Cause::NotFound,
        K::PermissionDenied if operation == Operation::Lock => Cause::InUse,
        K::PermissionDenied => Cause::NoAccess,
        K::AlreadyExists => Cause::AlreadyExists,
        K::StorageFull | K::QuotaExceeded => Cause::NoSpace,
        K::ReadOnlyFilesystem => Cause::ReadOnly,
        K::ResourceBusy | K::WouldBlock => Cause::InUse,
        _ => Cause::Other,
    }
}

/// One sentence describing the failure, naming the file.
#[must_use]
pub fn describe(error: &io::Error, operation: Operation, subject: &Path) -> String {
    let name = subject
        .file_name()
        .map_or_else(|| subject.display().to_string(), |n| n.to_string_lossy().into_owned());
    let verb = operation.verb();
    match classify(error, operation) {
        Cause::NotFound => format!("Could not {verb} \"{name}\": it no longer exists."),
        Cause::NoAccess => format!("Could not {verb} \"{name}\": permission denied."),
        Cause::InUse => format!("Could not {verb} \"{name}\": another program is using it."),
        Cause::AlreadyExists => {
            format!("Could not {verb} \"{name}\": something with that name already exists.")
        }
        Cause::NoSpace => format!("Could not {verb} \"{name}\": the disk is full."),
        Cause::ReadOnly => format!("Could not {verb} \"{name}\": the disk is read-only."),
        Cause::Other => format!("Could not {verb} \"{name}\": {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_operation_disambiguates_permission_errors() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(classify(&denied, Operation::Read), Cause::NoAccess);
        assert_eq!(classify(&denied, Operation::Lock), Cause::InUse);
    }

    #[test]
    fn unrecognised_errors_keep_their_message() {
        let odd = io::Error::other("the flux capacitor is misaligned");
        let text = describe(&odd, Operation::Write, Path::new("/p/a.txt"));
        assert_eq!(text, "Could not save \"a.txt\": the flux capacitor is misaligned");
    }

    #[test]
    fn known_errors_read_the_same_everywhere() {
        let gone = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(
            describe(&gone, Operation::Read, Path::new("/p/x.rs")),
            "Could not read \"x.rs\": it no longer exists."
        );
    }
}
