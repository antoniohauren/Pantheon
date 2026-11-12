//! Portable name rules (foundation §1), applied on every OS.

use crate::Reason;

/// The incomplete marker written first into every new output tree.
pub const MARKER: &str = ".pantheon-incomplete";
/// pkg-tools' marker, recognised as "incomplete" when reading source folders.
pub const PKG_TOOLS_MARKER: &str = ".pkg-tools-incomplete";
pub const MAX_COMPONENT_BYTES: usize = 255;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_PATH_COMPONENTS: usize = 128;

const MARKER_TEXT: &[u8] = b"Pantheon output incomplete. Remove this tree before retrying.\n";
const SCRATCH_INFIX: &str = ".pantheon-";
const SCRATCH_SUFFIX: &str = ".partial";
const SCRATCH_RANDOM_BYTES: usize = 8;
/// `.` + infix + two hex digits per random byte + suffix.
const SCRATCH_OVERHEAD: usize =
    1 + SCRATCH_INFIX.len() + 2 * SCRATCH_RANDOM_BYTES + SCRATCH_SUFFIX.len();
const FORBIDDEN: &[u8] = b"/\\<>:\"|?*";
const DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
const DEL: u8 = 0x7f;

pub(crate) fn marker_text() -> &'static [u8] {
    MARKER_TEXT
}

/// Checks one path component against the portable rules, reserved names included.
/// Sibling collisions need the directory's contents and are checked by the trees.
pub fn validate_component(name: &str) -> Result<(), Reason> {
    let bytes = name.as_bytes();
    if name.is_empty() {
        return Err(Reason::Empty);
    }
    if name == "." || name == ".." {
        return Err(Reason::DotName);
    }
    if bytes.len() > MAX_COMPONENT_BYTES {
        return Err(Reason::ComponentTooLong);
    }
    if bytes
        .iter()
        .any(|&b| b < b' ' || b == DEL || FORBIDDEN.contains(&b))
    {
        return Err(Reason::ForbiddenChar);
    }
    if name.ends_with(['.', ' ']) {
        return Err(Reason::TrailingDotOrSpace);
    }
    if is_device(name) {
        return Err(Reason::ReservedDevice);
    }
    if is_reserved(name) {
        return Err(Reason::ReservedName);
    }
    Ok(())
}

/// Checks a `/`-separated relative path: limits plus [`validate_component`] per part.
pub fn validate_path(rel: &str) -> Result<(), Reason> {
    if rel.len() > MAX_PATH_BYTES {
        return Err(Reason::PathTooLong);
    }
    if rel.split('/').nth(MAX_PATH_COMPONENTS).is_some() {
        return Err(Reason::TooManyComponents);
    }
    rel.split('/').try_for_each(validate_component)
}

fn is_device(name: &str) -> bool {
    // Windows ignores the extension and trailing spaces of the stem.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_bytes() {
        [b'C', b'O', b'M', b'1'..=b'9'] | [b'L', b'P', b'T', b'1'..=b'9'] => true,
        _ => DEVICES.contains(&upper.as_str()),
    }
}

/// The marker, or anything shaped like a scratch name, ignoring ASCII case.
pub(crate) fn is_reserved(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == MARKER
        || (lower.starts_with('.')
            && lower.ends_with(SCRATCH_SUFFIX)
            && lower.contains(SCRATCH_INFIX))
}

/// The ASCII-case-folded key used for sibling collision checks.
pub(crate) fn fold(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// `.<name>.pantheon-<random>.partial`, with `name` shortened to keep it ≤ 255 bytes.
pub(crate) fn scratch_name(name: &str) -> Result<String, crate::Error> {
    let mut random = [0u8; SCRATCH_RANDOM_BYTES];
    crate::os_random(&mut random)?;
    let keep = name.floor_char_boundary(MAX_COMPONENT_BYTES.saturating_sub(SCRATCH_OVERHEAD));
    Ok(format!(
        ".{}{SCRATCH_INFIX}{}{SCRATCH_SUFFIX}",
        &name[..keep],
        hex(&random)
    ))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in [
            "eboot.bin",
            "USRDIR",
            "a b",
            ".hidden",
            "ü",
            "COM10",
            "CONSOLE",
            "x.CON",
        ] {
            assert_eq!(validate_component(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn rejects_unsafe_names() {
        let long = "a".repeat(MAX_COMPONENT_BYTES + 1);
        let cases = [
            ("", Reason::Empty),
            (".", Reason::DotName),
            ("..", Reason::DotName),
            (long.as_str(), Reason::ComponentTooLong),
            ("a/b", Reason::ForbiddenChar),
            ("a\\b", Reason::ForbiddenChar),
            ("a\0b", Reason::ForbiddenChar),
            ("a\nb", Reason::ForbiddenChar),
            ("a\x7fb", Reason::ForbiddenChar),
            ("a:b", Reason::ForbiddenChar),
            ("a?", Reason::ForbiddenChar),
            ("a.", Reason::TrailingDotOrSpace),
            ("a ", Reason::TrailingDotOrSpace),
            ("con", Reason::ReservedDevice),
            ("Nul.txt", Reason::ReservedDevice),
            ("CON .txt", Reason::ReservedDevice),
            ("lpt9", Reason::ReservedDevice),
            ("COM1.tar.gz", Reason::ReservedDevice),
        ];
        for (name, reason) in cases {
            assert_eq!(validate_component(name), Err(reason), "{name:?}");
        }
    }

    #[test]
    fn reserved_pantheon_names_ignore_case() {
        for name in [
            MARKER,
            ".PANTHEON-INCOMPLETE",
            ".x.pantheon-0123.partial",
            ".X.Pantheon-ABCD.PARTIAL",
        ] {
            assert_eq!(
                validate_component(name),
                Err(Reason::ReservedName),
                "{name}"
            );
        }
        let scratch = scratch_name("game.pkg").unwrap();
        assert_eq!(validate_component(&scratch), Err(Reason::ReservedName));
    }

    #[test]
    fn scratch_names_stay_within_the_component_limit() {
        let long = "é".repeat(MAX_COMPONENT_BYTES / 2);
        let scratch = scratch_name(&long).unwrap();
        assert!(scratch.len() <= MAX_COMPONENT_BYTES);
        assert!(is_reserved(&scratch));
    }

    #[test]
    fn path_limits() {
        assert_eq!(validate_path("a/b/c"), Ok(()));
        assert_eq!(validate_path("a//b"), Err(Reason::Empty));
        assert_eq!(validate_path("a/../b"), Err(Reason::DotName));
        let deep = vec!["d"; MAX_PATH_COMPONENTS].join("/");
        assert_eq!(validate_path(&deep), Ok(()));
        assert_eq!(
            validate_path(&format!("{deep}/d")),
            Err(Reason::TooManyComponents)
        );
        let wide = vec!["a".repeat(200); 21].join("/");
        assert_eq!(validate_path(&wide), Err(Reason::PathTooLong));
    }
}
