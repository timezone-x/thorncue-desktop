use std::path::PathBuf;

/// Why a package was rejected or couldn't be written. Every message is a sentence a sound
/// operator can act on; it is shown in the UI as is.
#[derive(Debug, thiserror::Error)]
pub enum TqsError {
    /// The package breaks a rule in FORMAT.md.
    #[error("{0}")]
    Invalid(String),
    /// The disk said no.
    #[error("Couldn't {action} {}: {source}. Check the disk has space and the folder isn't read-only.", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl TqsError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        TqsError::Invalid(msg.into())
    }

    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| TqsError::Io {
            action,
            path,
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, TqsError>;

/// Something worth telling the user that doesn't stop the import.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct Warning(pub String);

impl std::fmt::Display for Warning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
