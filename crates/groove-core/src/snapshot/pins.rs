//! Pin inspection and transactional pin creation/deletion.
use super::{invalid, text, SnapshotLock, Snapshots};
use crate::{
    git::{Git, Invocation},
    Error, ErrorCode, Result,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{Seek, Write},
    time::Duration,
};

pub(super) const PINS: &str = "refs/groove/snapshot-pins/";

impl Snapshots {
    fn ref_query(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        lock: &SnapshotLock,
        timeout: Duration,
    ) -> Result<String> {
        let fds = lock.descriptors();
        let stdin = input
            .map(|bytes| -> Result<fs::File> {
                let mut file = tempfile::tempfile()?;
                file.write_all(bytes)?;
                file.rewind()?;
                Ok(file)
            })
            .transpose()?;
        if timeout.is_zero() {
            return Err(Error::new(
                ErrorCode::GitError,
                "Snapshot pin inspection timed out",
            ));
        }
        let bytes = Git::new(timeout).run_with(
            &self.root,
            args,
            Invocation {
                inherit: &fds,
                stdin,
                reject_stderr: true,
                ..Invocation::snapshot()
            },
        )?;
        text(bytes)
    }

    /// Git can exit successfully while omitting broken refs. Such diagnostics
    /// are failures here: an incomplete inventory cannot establish an absent pin.
    pub(super) fn pin_inventory(
        &self,
        pattern: &str,
        lock: &SnapshotLock,
        timeout: Duration,
    ) -> Result<BTreeMap<String, String>> {
        let output = self.ref_query(
            &[
                "for-each-ref",
                "--format=%(refname)%09%(objectname)%09%(symref)",
                pattern,
            ],
            None,
            lock,
            timeout,
        )?;
        let mut pins = BTreeMap::new();
        for record in output.lines() {
            let mut fields = record.splitn(3, '\t');
            let reference = fields.next().unwrap_or_default();
            let oid = fields.next().unwrap_or_default();
            let symbolic = fields.next();
            if !reference.starts_with(PINS)
                || symbolic != Some("")
                || oid.parse::<crate::CommitOid>().is_err()
            {
                return Err(Error::new(
                    ErrorCode::GitError,
                    "Invalid snapshot pin reference",
                ));
            }
            pins.insert(reference.into(), oid.into());
        }
        Ok(pins)
    }

    /// A dangling symbolic ref may be silently omitted even by a diagnostic-free
    /// inventory. Ask the ref backend to lock and verify absence, then abort.
    /// Small batches bound Git's lock descriptors regardless of listing size.
    pub(super) fn verify_absent_pins(
        &self,
        absent: &[(String, usize)],
        lock: &SnapshotLock,
    ) -> Result<()> {
        for batch in absent.chunks(32) {
            // Git 2.40's ref transaction accepts a dangling symref as a zero
            // value. Object resolution diagnoses that state even when ref
            // enumeration silently omits it; retain those diagnostics as errors.
            let names: String = batch.iter().map(|(name, _)| format!("{name}\n")).collect();
            let resolved = self.ref_query(
                &["cat-file", "--batch-check=%(objectname)"],
                Some(names.as_bytes()),
                lock,
                self.timeout,
            )?;
            let expected: String = batch
                .iter()
                .map(|(name, _)| format!("{name} missing\n"))
                .collect();
            if resolved != expected.trim_end_matches('\n') {
                return Err(Error::new(
                    ErrorCode::GitError,
                    "Snapshot pin appeared during inspection",
                ));
            }
            let mut transaction = String::from("start\n");
            for (reference, width) in batch {
                transaction.push_str(&format!("verify {reference} {}\n", "0".repeat(*width)));
            }
            transaction.push_str("prepare\nabort\n");
            self.run(
                &["update-ref", "--no-deref", "--stdin"],
                None,
                Some(transaction.as_bytes()),
                Some(lock),
            )?;
        }
        Ok(())
    }

    pub(super) fn pinned(&self, id: &str, oid: &str, lock: &SnapshotLock) -> Result<bool> {
        let reference = format!("{PINS}{id}");
        let pins = self.pin_inventory(&reference, lock, self.timeout)?;
        match pins.get(&reference) {
            Some(pin) if pin == oid => Ok(true),
            Some(_) => Err(Error::new(
                ErrorCode::GitError,
                "Snapshot pin does not match its snapshot commit",
            )),
            None => {
                self.verify_absent_pins(&[(reference, oid.len())], lock)?;
                Ok(false)
            }
        }
    }

    /// Inspect the original ref while a prepared transaction holds its lock.
    /// This deliberately does not start another absence-verification transaction.
    fn validate_prepared_pin(
        &self,
        id: &str,
        expected: Option<&str>,
        lock: &SnapshotLock,
        timeout: Duration,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        let pin = format!("{PINS}{id}");
        let pins = self.pin_inventory(&pin, lock, timeout)?;
        match (expected, pins.get(&pin)) {
            (Some(oid), Some(pin_oid)) if pin_oid == oid => Ok(()),
            (None, None) => {
                // Old Git's zero-OID comparison accepts a dangling symref.
                // Resolution emits a diagnostic for that otherwise omitted ref.
                let resolved = self.ref_query(
                    &["cat-file", "--batch-check=%(objectname)"],
                    Some(format!("{pin}\n").as_bytes()),
                    lock,
                    timeout.saturating_sub(started.elapsed()),
                )?;
                if resolved == format!("{pin} missing") {
                    Ok(())
                } else {
                    Err(Error::new(
                        ErrorCode::GitError,
                        "Snapshot pin changed before committing its transaction",
                    ))
                }
            }
            _ => Err(Error::new(
                ErrorCode::GitError,
                "Snapshot pin changed before committing its transaction",
            )),
        }
    }

    /// Pin a checkpoint with a dedicated reference, so `delete` requires `force`.
    /// `release --save` pins the snapshot it saves before releasing a workspace.
    pub(crate) fn pin(&self, id: &str) -> Result<()> {
        let lock = self.lock()?;
        let (s, oid) = self.load_at(id, &lock)?;
        let reference = format!("{PINS}{}", s.id);
        let expected = if s.pinned {
            oid.clone()
        } else {
            "0".repeat(oid.len())
        };
        let snapshot_ref = Self::reference(id)?;
        let transaction = format!(
            "start\nverify {snapshot_ref} {oid}\nupdate {reference} {oid} {expected}\nprepare\n"
        );
        self.run_env(
            &["update-ref", "--no-deref", "--stdin"],
            None,
            Some(transaction.as_bytes()),
            Some(&lock),
            &[],
            Some(Box::new(|timeout| {
                self.validate_prepared_pin(&s.id, s.pinned.then_some(oid.as_str()), &lock, timeout)
            })),
        )?;
        Ok(())
    }
    pub(super) fn delete_inner(&self, id: &str, force: bool, lock: &SnapshotLock) -> Result<()> {
        let (s, oid) = self.load_at(id, lock)?;
        if s.pinned && !force {
            return Err(invalid(
                "Snapshot is pinned by release --save; pass --force to delete it",
            ));
        }
        let reference = Self::reference(id)?;
        crate::state::checkpoint("snapshot-before-delete");
        let mut transaction = format!("start\ndelete {reference} {oid}\n");
        if s.pinned {
            transaction.push_str(&format!("delete {PINS}{} {oid}\n", s.id));
        } else {
            transaction.push_str(&format!(
                "verify {PINS}{} {}\n",
                s.id,
                "0".repeat(oid.len())
            ));
        }
        transaction.push_str("prepare\n");
        self.run_env(
            &["update-ref", "--no-deref", "--stdin"],
            None,
            Some(transaction.as_bytes()),
            Some(lock),
            &[],
            Some(Box::new(|timeout| {
                self.validate_prepared_pin(&s.id, s.pinned.then_some(oid.as_str()), lock, timeout)
            })),
        )?;
        Ok(())
    }
}
