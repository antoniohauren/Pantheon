//! pantheon-fs: every write to the host filesystem, and the safety-relevant reads
//! of source folders (docs/03-foundation.md, `pantheon-fs`).
//!
//! Linux is the verified platform. The macOS and Windows code paths are written
//! to the same contract but are unverified (docs/spikes/phase1-fs.md). On Windows
//! the checks are path-based and weaker than the Unix descriptor-relative ones.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod error;
mod file;
#[cfg(test)]
mod inject;
mod name;
mod source;
mod sys;
mod tree;

pub use error::{Error, Reason};
pub use file::{OutputFile, ScratchFile};
pub use name::{
    MARKER, MAX_COMPONENT_BYTES, MAX_PATH_BYTES, MAX_PATH_COMPONENTS, PKG_TOOLS_MARKER,
    validate_component, validate_path,
};
pub use source::{EntryKind, SourceEntry, SourceSnapshot, SourceTree};
pub use tree::{OutputTree, TreeFile};

/// Permissions of a newly created file, applied at creation, never by a later `chmod`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Access {
    /// The platform default (mode `0666` minus the umask on Unix).
    #[default]
    Default,
    /// Readable and writable by the owner only (mode `0600` on Unix).
    /// Windows returns [`Error::Unsupported`] until the owner-only ACL spike passes.
    OwnerOnly,
}

/// Fills `buf` from the OS cryptographic random source (D26). Never falls back
/// to a weaker source; a failure is an `Io` error.
pub fn os_random(buf: &mut [u8]) -> Result<(), Error> {
    sys::os_random(buf).map_err(Error::Random)
}

#[cfg(test)]
mod testdir {
    use std::path::{Path, PathBuf};

    /// A fresh, canonical (symlink-free) directory under the system temp dir.
    /// Kept on failure for inspection; removed on drop otherwise.
    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            let mut bytes = [0u8; 8];
            crate::os_random(&mut bytes).unwrap();
            let base = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = base.join(format!("pantheon-fs-test-{}", crate::name::hex(&bytes)));
            std::fs::create_dir(&path).unwrap();
            TestDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn os_random_fills_the_buffer() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        super::os_random(&mut a).unwrap();
        super::os_random(&mut b).unwrap();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }
}
