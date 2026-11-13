use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// A portable pool namespace: 1..64 lowercase ASCII letters, digits, `_` or `-`,
/// starting with a letter. `default` selects the original repository pool.
/// Parsing proves a safe name, not membership or ownership.
///
/// ```
/// let pool: groove_core::PoolName = "frontend-ci".parse()?;
/// assert_eq!(pool.as_str(), "frontend-ci");
/// assert!("../other".parse::<groove_core::PoolName>().is_err());
/// # Ok::<(), groove_core::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PoolName(String);
impl PoolName {
    /// Return the canonical namespace string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn is_default(&self) -> bool {
        self.0 == "default"
    }
}
impl Default for PoolName {
    fn default() -> Self {
        Self("default".into())
    }
}
impl FromStr for PoolName {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 64
            || !value.as_bytes()[0].is_ascii_lowercase()
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        {
            return Err(Error::new(ErrorCode::InvalidArgument, "Pool name must be 1..64 lowercase ASCII letters, digits, '_' or '-', starting with a letter"));
        }
        Ok(Self(value.into()))
    }
}
impl TryFrom<String> for PoolName {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}
impl From<PoolName> for String {
    fn from(value: PoolName) -> Self {
        value.0
    }
}
impl fmt::Display for PoolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Identity of one initialized pool, preserved through deletion and changed by reinit.
/// This is an optimistic precondition, never proof of descriptor ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PoolId(Uuid);
impl PoolId {
    pub(crate) fn generate() -> Self {
        Self(Uuid::new_v4())
    }
}
impl FromStr for PoolId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        let id = Uuid::parse_str(value).map_err(|e| {
            Error::new(ErrorCode::InvalidArgument, "Pool ID must be a UUID").with_source(e)
        })?;
        if id.get_version_num() != 4 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Pool ID must be UUID v4",
            ));
        }
        Ok(Self(id))
    }
}
impl fmt::Display for PoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl TryFrom<String> for PoolId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}
impl From<PoolId> for String {
    fn from(value: PoolId) -> Self {
        value.to_string()
    }
}

/// A validated permanent slot name (`wt-0001` through `wt-4096`).
///
/// Parsing and deserialization enforce the same rules. Constructing an ID does
/// not imply that the slot exists in a particular pool.
///
/// ```
/// use groove_core::SlotId;
/// let slot: SlotId = "wt-0001".parse()?;
/// assert_eq!(slot.as_str(), "wt-0001");
/// assert!("../outside".parse::<SlotId>().is_err());
/// # Ok::<(), groove_core::Error>(())
/// ```
///
/// Raw strings require validation before becoming IDs:
///
/// ```compile_fail
/// use groove_core::SlotId;
/// let slot: SlotId = "../outside".to_owned();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SlotId(String);

impl SlotId {
    /// Construct a slot ID from its one-based number.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidArgument`] unless `number` is in
    /// `1..=`[`MAX_CAPACITY`](crate::MAX_CAPACITY).
    pub fn new(number: usize) -> Result<Self> {
        if !(1..=crate::MAX_CAPACITY).contains(&number) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Slot number must be 1..=4096",
            ));
        }
        Ok(Self(format!("wt-{number:04}")))
    }
    /// Return the canonical slot name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Return the stable one-based workspace number, independent of its task.
    #[must_use]
    pub fn number(&self) -> u16 {
        self.0.as_bytes()[3..]
            .iter()
            .fold(0, |number, byte| number * 10 + u16::from(byte - b'0'))
    }
}
impl TryFrom<String> for SlotId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 7
            || !bytes.starts_with(b"wt-")
            || !bytes[3..].iter().all(u8::is_ascii_digit)
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Slot ID must have the form wt-NNNN",
            ));
        }
        let number = bytes[3..]
            .iter()
            .fold(0u16, |n, b| n * 10 + u16::from(b - b'0'));
        if !(1..=crate::MAX_CAPACITY).contains(&usize::from(number)) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Slot number must be 1..=4096",
            ));
        }
        Ok(Self(value))
    }
}
impl FromStr for SlotId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        value.to_owned().try_into()
    }
}
impl From<SlotId> for String {
    fn from(value: SlotId) -> Self {
        value.0
    }
}
impl AsRef<str> for SlotId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A unique acquisition generation, distinct from slot IDs and raw UUIDs.
///
/// Groove generates UUID v4 values. Imported UUIDs identify generations but never
/// confer ownership; release also requires the owning lock descriptor.
///
/// ```compile_fail
/// use groove_core::LeaseId;
/// let unrelated_uuid = uuid::Uuid::new_v4();
/// let lease: LeaseId = unrelated_uuid;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LeaseId(Uuid);
impl LeaseId {
    /// Generate a fresh UUID v4 acquisition identity.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4())
    }
    /// Borrow the underlying UUID for explicit interoperability.
    #[must_use]
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}
impl From<Uuid> for LeaseId {
    fn from(value: Uuid) -> Self {
        Self(value)
    }
}
impl From<LeaseId> for Uuid {
    fn from(value: LeaseId) -> Self {
        value.0
    }
}
impl FromStr for LeaseId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Uuid::parse_str(value).map(Self).map_err(|e| {
            Error::new(ErrorCode::InvalidArgument, "Lease ID must be a UUID").with_source(e)
        })
    }
}
impl fmt::Display for LeaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A full lowercase SHA-1 or SHA-256 commit object ID.
///
/// Parsing validates the representation. Acquisition resolves and verifies that
/// the object actually identifies a commit in the selected repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitOid(String);
impl CommitOid {
    /// Return the canonical hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for CommitOid {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        if ![40, 64].contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Commit OID must be 40 or 64 lowercase hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }
}
impl FromStr for CommitOid {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        value.to_owned().try_into()
    }
}
impl From<CommitOid> for String {
    fn from(value: CommitOid) -> Self {
        value.0
    }
}
impl AsRef<str> for CommitOid {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for CommitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
