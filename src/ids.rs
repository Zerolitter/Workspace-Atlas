//! Stable identifier derivation per ADR-015.

use blake3::Hasher;

const WS_PREFIX: &str = "ws_";
const GEN_PREFIX: &str = "gen_";
const FILE_PREFIX: &str = "file_";
const REV_PREFIX: &str = "rev_";
const FILE_REVISION_IDENTITY_DOMAIN: &str = "file-revision-identity-v2.0.0";

/// Derive the canonical `workspace_id` from a fully canonicalized root path and a
/// display name. Uses BLAKE3, truncated to 32 lowercase hex chars, prefixed `ws_`.
pub fn workspace_id(canonical_root: &str, display_name: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(canonical_root.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(display_name.as_bytes());
    let hash = hasher.finalize();
    let hex = hash.to_hex();
    // 16 bytes = 32 hex chars
    let short = &hex.as_str()[..32];
    format!("{WS_PREFIX}{short}")
}

/// Derive a generation ID from a deterministic workspace ID and monotonic
/// sequence number, with a stable prefix for human inspection.
pub fn generation_id(workspace_id: &str, sequence_no: i64) -> String {
    let mut hasher = Hasher::new();
    hasher.update(workspace_id.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(sequence_no.to_le_bytes().as_slice());
    let hex = hasher.finalize().to_hex();
    let short = &hex.as_str()[..32];
    format!("{GEN_PREFIX}{short}")
}

/// Derive a stable, workspace-scoped file ID from its canonical relative path.
pub fn file_id_from_path(workspace_id: &str, canonical_relative_path: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(workspace_id.as_bytes());
    hasher.update(&[0x1f]);
    hasher.update(canonical_relative_path.as_bytes());
    let hex = hasher.finalize().to_hex();
    let short = &hex.as_str()[..32];
    format!("{FILE_PREFIX}{short}")
}

/// The complete immutable material tuple for `file-revision-identity-v2.0.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileRevisionIdentity<'a> {
    pub file_id: &'a str,
    pub content_hash: &'a str,
    pub artifact_class: &'a str,
    pub language: Option<&'a str>,
    pub is_generated: bool,
    pub is_test: bool,
}

/// Derive a revision ID from the complete immutable material tuple.
///
/// Canonical bytes are the seven UTF-8 values below in order: the policy
/// domain, `file_id`, the full content hash, `artifact_class`,
/// language-or-empty, generated flag, and test flag. Each value is framed by
/// its UTF-8 **byte** length encoded as an unsigned 64-bit little-endian
/// integer immediately before its bytes. Booleans are the one-byte ASCII
/// values `0` or `1`; `None` language is encoded exactly like an empty string.
/// The result is `rev_` plus the full lowercase BLAKE3 digest.
pub(crate) fn file_revision_id(identity: FileRevisionIdentity<'_>) -> String {
    fn update_framed(hasher: &mut Hasher, value: &str) {
        let bytes = value.as_bytes();
        let byte_length = u64::try_from(bytes.len()).expect("UTF-8 value length must fit in a u64");
        hasher.update(&byte_length.to_le_bytes());
        hasher.update(bytes);
    }

    let mut hasher = Hasher::new();
    for value in [
        FILE_REVISION_IDENTITY_DOMAIN,
        identity.file_id,
        identity.content_hash,
        identity.artifact_class,
        identity.language.unwrap_or(""),
        if identity.is_generated { "1" } else { "0" },
        if identity.is_test { "1" } else { "0" },
    ] {
        update_framed(&mut hasher, value);
    }
    format!("{REV_PREFIX}{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision_id(
        file_id: &str,
        content_hash: &str,
        artifact_class: &str,
        language: Option<&str>,
        is_generated: bool,
        is_test: bool,
    ) -> String {
        file_revision_id(FileRevisionIdentity {
            file_id,
            content_hash,
            artifact_class,
            language,
            is_generated,
            is_test,
        })
    }

    #[test]
    fn workspace_id_is_deterministic() {
        let a = workspace_id("C:\\<workspace>\\example", "Example");
        let b = workspace_id("C:\\<workspace>\\example", "Example");
        assert_eq!(a, b);
        assert!(a.starts_with("ws_"));
        assert_eq!(a.len(), "ws_".len() + 32);
    }

    #[test]
    fn workspace_id_differs_with_display_name() {
        let a = workspace_id("C:\\<workspace>\\example", "Example A");
        let b = workspace_id("C:\\<workspace>\\example", "Example B");
        assert_ne!(a, b);
    }

    #[test]
    fn workspace_id_differs_with_root() {
        let a = workspace_id("C:\\<workspace>\\example", "Example");
        let b = workspace_id("C:\\<workspace>\\other", "Example");
        assert_ne!(a, b);
    }

    #[test]
    fn generation_id_is_deterministic() {
        let a = generation_id("ws_abc", 1);
        let b = generation_id("ws_abc", 1);
        assert_eq!(a, b);
        let c = generation_id("ws_abc", 2);
        assert_ne!(a, c);
    }

    #[test]
    fn file_id_is_deterministic() {
        let a = file_id_from_path("ws_abc", "src/lib.rs");
        let b = file_id_from_path("ws_abc", "src/lib.rs");
        assert_eq!(a, b);
        let c = file_id_from_path("ws_abc", "src/main.rs");
        assert_ne!(a, c);
    }

    #[test]
    fn file_revision_identity_v2_uses_every_material_field() {
        let content_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let base = revision_id("file_a", content_hash, "source", None, false, false);
        assert_eq!(
            base,
            revision_id("file_a", content_hash, "source", None, false, false)
        );
        assert!(base.starts_with("rev_"));
        assert_eq!(base.len(), "rev_".len() + 64);

        assert_ne!(
            base,
            revision_id("file_b", content_hash, "source", None, false, false)
        );
        assert_ne!(
            base,
            revision_id(
                "file_a",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source",
                None,
                false,
                false,
            )
        );
        assert_ne!(
            base,
            revision_id("file_a", content_hash, "test", None, false, false)
        );
        assert_ne!(
            base,
            revision_id("file_a", content_hash, "source", Some("rust"), false, false,)
        );
        assert_ne!(
            base,
            revision_id("file_a", content_hash, "source", None, true, false)
        );
        assert_ne!(
            base,
            revision_id("file_a", content_hash, "source", None, false, true)
        );
        assert_eq!(
            base,
            revision_id("file_a", content_hash, "source", Some(""), false, false)
        );
    }

    #[test]
    fn file_revision_identity_v2_uses_the_full_content_hash() {
        let shared_prefix = "0123456789ab";
        let first_hash = format!("{shared_prefix}{}", "0".repeat(52));
        let second_hash = format!("{shared_prefix}{}", "1".repeat(52));
        assert_eq!(first_hash.len(), 64);
        assert_eq!(second_hash.len(), 64);
        assert_ne!(
            revision_id("file_a", &first_hash, "source", None, false, false),
            revision_id("file_a", &second_hash, "source", None, false, false)
        );
    }

    #[test]
    fn file_revision_identity_v2_known_vectors_pin_u64_byte_framing() {
        fn independently_framed_id(fields: &[(u64, &[u8])]) -> String {
            let mut bytes = Vec::new();
            for (declared_byte_length, value) in fields {
                assert_eq!(
                    *declared_byte_length,
                    u64::try_from(value.len()).unwrap(),
                    "vector must declare the UTF-8 byte length explicitly"
                );
                bytes.extend_from_slice(&declared_byte_length.to_le_bytes());
                bytes.extend_from_slice(value);
            }
            format!("rev_{}", blake3::hash(&bytes).to_hex())
        }

        let ascii_hash = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let ascii_vector = independently_framed_id(&[
            (29, b"file-revision-identity-v2.0.0"),
            (8, b"file_abc"),
            (64, ascii_hash),
            (6, b"source"),
            (0, b""),
            (1, b"0"),
            (1, b"1"),
        ]);
        assert_eq!(
            ascii_vector,
            "rev_f16f3294866ad417f6ce0d98d5f8f282f8d632012496b655681e11c75aa19939"
        );
        assert_eq!(
            revision_id(
                "file_abc",
                std::str::from_utf8(ascii_hash).unwrap(),
                "source",
                None,
                false,
                true,
            ),
            ascii_vector
        );

        let utf8_file_id = "file_δ";
        let utf8_language = "日本語";
        let utf8_hash = b"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let utf8_vector = independently_framed_id(&[
            (29, b"file-revision-identity-v2.0.0"),
            (7, utf8_file_id.as_bytes()),
            (64, utf8_hash),
            (6, b"source"),
            (9, utf8_language.as_bytes()),
            (1, b"1"),
            (1, b"1"),
        ]);
        assert_eq!(
            utf8_vector,
            "rev_baaa73fb377983bc3f8f4c75871c5a29fd6cb008b610439471a97b3faeca2e2a"
        );
        assert_eq!(
            revision_id(
                utf8_file_id,
                std::str::from_utf8(utf8_hash).unwrap(),
                "source",
                Some(utf8_language),
                true,
                true,
            ),
            utf8_vector
        );
    }
}
