//! The field is private to this module, so [`ListedName::of`] is the one
//! producer, and it reads a directory listing.

/// A name as its parent directory lists it.
///
/// On a folding filesystem `canonicalize` echoes back the spelling it was
/// asked with, so it can never report a name the operator did not type. A
/// listing entry carries the spelling on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedName(String);

impl ListedName {
    pub fn of(entry: &std::fs::DirEntry) -> Self {
        Self(entry.file_name().to_string_lossy().into_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ListedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
