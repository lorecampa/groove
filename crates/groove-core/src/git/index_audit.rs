//! Bounded parsing of `ls-files -v --stage -z`; filenames are never retained.

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct IndexAudit {
    pub conflicts: bool,
    pub gitlinks: bool,
    pub assume_unchanged: bool,
    pub skip_worktree: bool,
    pub may_have_intent: bool,
    pub malformed: bool,
}

#[derive(Default)]
pub(super) struct Scanner {
    audit: IndexAudit,
    header: Vec<u8>,
    in_path: bool,
    path_seen: bool,
    pending: bool,
}

impl Scanner {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == 0 {
                self.audit.malformed |= !self.in_path || !self.path_seen;
                self.header.clear();
                self.in_path = false;
                self.path_seen = false;
                self.pending = false;
            } else if self.in_path {
                self.path_seen = true;
            } else {
                self.pending = true;
                if byte == b'\t' {
                    self.check_header();
                    self.in_path = true;
                } else if self.header.len() < 80 {
                    self.header.push(byte);
                } else {
                    self.audit.malformed = true;
                }
            }
        }
    }

    fn check_header(&mut self) {
        let fields: Vec<_> = self.header.split(|byte| *byte == b' ').collect();
        let [tag, mode, oid, stage] = fields.as_slice() else {
            self.audit.malformed = true;
            return;
        };
        if tag.len() != 1
            || !matches!(
                tag[0].to_ascii_uppercase(),
                b'H' | b'S' | b'M' | b'R' | b'C' | b'K' | b'?'
            )
            || mode.len() != 6
            || !mode.iter().all(|byte| matches!(byte, b'0'..=b'7'))
            || !matches!(oid.len(), 40 | 64)
            || !oid.iter().all(u8::is_ascii_hexdigit)
            || stage.len() != 1
            || !matches!(stage[0], b'0'..=b'3')
        {
            self.audit.malformed = true;
            return;
        }
        self.audit.conflicts |= stage[0] != b'0';
        self.audit.gitlinks |= *mode == b"160000";
        self.audit.assume_unchanged |= tag[0].is_ascii_lowercase();
        self.audit.skip_worktree |= tag[0].eq_ignore_ascii_case(&b'S');
        // Git records intent-to-add using the empty blob, for either object format.
        self.audit.may_have_intent |= matches!(
            *oid,
            b"e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
                | b"473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813"
        );
    }

    pub fn finish(mut self) -> IndexAudit {
        self.audit.malformed |= self.pending;
        self.audit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const OID: &str = "0123456789012345678901234567890123456789";

    fn scan(bytes: &[u8], chunk: usize) -> IndexAudit {
        let mut scanner = Scanner::default();
        for bytes in bytes.chunks(chunk) {
            scanner.feed(bytes);
        }
        scanner.finish()
    }

    #[test]
    fn flags_conflicts_gitlinks_and_intent_are_detected_across_read_boundaries() {
        let cases = [
            (format!("H 100644 {OID} 2\tconflict\0"), "conflict"),
            (format!("H 160000 {OID} 0\tsubmodule\0"), "gitlink"),
            (format!("h 100644 {OID} 0\tassume\0"), "assume"),
            (format!("S 100644 {OID} 0\tskip\0"), "skip"),
            ("H 100644 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tintent\0".into(), "intent"),
            ("H 100644 473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813 0\tsha256\0".into(), "intent"),
        ];
        for (record, kind) in cases {
            for chunk in 1..=record.len() {
                let audit = scan(record.as_bytes(), chunk);
                assert!(!audit.malformed, "{record:?}");
                assert_eq!(audit.conflicts, kind == "conflict");
                assert_eq!(audit.gitlinks, kind == "gitlink");
                assert_eq!(audit.assume_unchanged, kind == "assume");
                assert_eq!(audit.skip_worktree, kind == "skip");
                assert_eq!(audit.may_have_intent, kind == "intent");
            }
        }
    }

    #[test]
    fn malformed_headers_and_unterminated_or_empty_paths_are_rejected() {
        for record in [
            "garbage\0".to_owned(),
            format!("H 100644 {OID} 0\t\0"),
            format!("H 100644 {OID} 0\tunterminated"),
            format!("H 100644 {OID} 9\tbad-stage\0"),
            format!("Q 100644 {OID} 0\tbad-tag\0"),
            "H 100644 xyz 0\tbad-oid\0".into(),
            format!("H 999999 {OID} 0\tbad-mode\0"),
            format!("H {} {OID} 0\tlong\0", "0".repeat(100)),
        ] {
            assert!(scan(record.as_bytes(), 3).malformed, "{record:?}");
        }
    }

    #[test]
    fn arbitrary_filenames_and_sha256_headers_do_not_grow_parser_storage() {
        let record = format!(
            "H 100644 {} 0\tspace\n2:name\t{}\0",
            "1".repeat(64),
            "x".repeat(100_000)
        );
        assert_eq!(scan(record.as_bytes(), 8192), IndexAudit::default());
        assert_eq!(scan(b"", 1), IndexAudit::default());
    }
}
