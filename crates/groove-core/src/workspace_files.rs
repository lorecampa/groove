//! Physical workspace entries which Git does not inventory.
use crate::{Error, ErrorCode, Result};
use std::{
    fs,
    io::{BufReader, BufWriter, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(crate) const DIRECTORY_STATE_NAME: &str = "groove-directories.v2";
const MAGIC: &[u8] = b"groove-directories-v2\n";
const FIELD_LIMIT: usize = 1024 * 1024;

#[derive(PartialEq, Eq)]
pub(crate) struct DirectoryState {
    pub(crate) path: Vec<u8>,
    pub(crate) mode: u32,
    pub(crate) attributes: Vec<(Vec<u8>, Vec<u8>)>,
}

fn invalid_inventory() -> Error {
    Error::new(
        ErrorCode::RecoveryFailed,
        "Unrecognized directory metadata inventory",
    )
}

pub(crate) fn write_directory_state(path: &Path, entries: &[DirectoryState]) -> Result<()> {
    crate::repository::no_symlinks(path)?;
    let parent = path.parent().ok_or_else(invalid_inventory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        output.write_all(MAGIC)?;
        fn field(output: &mut impl Write, bytes: &[u8]) -> Result<()> {
            if bytes.len() > FIELD_LIMIT {
                return Err(invalid_inventory());
            }
            output.write_all(&(bytes.len() as u32).to_le_bytes())?;
            output.write_all(bytes)?;
            Ok(())
        }
        for entry in entries {
            output.write_all(&entry.mode.to_le_bytes())?;
            field(&mut output, &entry.path)?;
            let count = u32::try_from(entry.attributes.len()).map_err(|_| invalid_inventory())?;
            output.write_all(&count.to_le_bytes())?;
            for (name, value) in &entry.attributes {
                field(&mut output, name)?;
                field(&mut output, value)?;
            }
        }
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    crate::state::sync_dir(parent)
}

pub(crate) struct SavedDirectories(BufReader<fs::File>);

impl SavedDirectories {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid_inventory());
        }
        let mut saved = Self(BufReader::new(file));
        let mut magic = [0; MAGIC.len()];
        saved.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(invalid_inventory());
        }
        Ok(saved)
    }

    fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.0.read_exact(bytes).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                invalid_inventory().with_source(error)
            } else {
                error.into()
            }
        })
    }

    fn number(&mut self) -> Result<u32> {
        let mut bytes = [0; 4];
        self.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn field(&mut self) -> Result<Vec<u8>> {
        let length = self.number()? as usize;
        if length > FIELD_LIMIT {
            return Err(invalid_inventory());
        }
        let mut bytes = vec![0; length];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn next_entry(&mut self) -> Result<Option<DirectoryState>> {
        let mut mode = [0; 4];
        if self.0.read(&mut mode[..1])? == 0 {
            return Ok(None);
        }
        self.read_exact(&mut mode[1..])?;
        let path = self.field()?;
        let count = self.number()?;
        let mut attributes = Vec::new();
        for _ in 0..count {
            attributes.push((self.field()?, self.field()?));
        }
        Ok(Some(DirectoryState {
            path,
            mode: u32::from_le_bytes(mode),
            attributes,
        }))
    }
}

/// Observe directory metadata which Git checkout does not reset. File contents
/// and file metadata have their own normalization path.
pub(crate) fn directory_state(root: &Path) -> Result<Vec<DirectoryState>> {
    Ok(crate::walk::inventory(
        root,
        crate::walk::Options {
            attributes: true,
            ..Default::default()
        },
    )?
    .directories)
}

/// Reject task-created metadata only when Git adopted the directory into its index.
pub(crate) fn validate_new_directories(
    git: &crate::git::Git,
    root: &Path,
    admin: &Path,
    current: &[DirectoryState],
) -> Result<()> {
    use std::ffi::OsStr;
    let inventory = admin.join(DIRECTORY_STATE_NAME);
    crate::repository::no_symlinks(&inventory)?;
    if !inventory.try_exists()? {
        return Ok(());
    }
    let mut saved = SavedDirectories::open(&inventory)?;
    let refusal = || {
        Error::new(ErrorCode::RecoveryFailed,
        "A tracked directory kept permissions or extended attributes from an earlier task; rebuilding the checkout")
    };
    let mut previous = saved.next_entry()?;
    let mut defaults = None;
    let mut suspicious = Vec::new();
    for entry in current {
        while previous.as_ref().is_some_and(|old| old.path < entry.path) {
            previous = saved.next_entry()?;
        }
        if previous.as_ref().is_some_and(|old| old.path == entry.path) {
            continue;
        }
        if defaults.is_none() {
            let probe = tempfile::Builder::new()
                .prefix(".groove-directory-probe-")
                .tempdir_in(root)?;
            let child = probe.path().join("default");
            fs::create_dir(&child)?;
            defaults = Some((
                fs::metadata(&child)?.mode() & 0o7777,
                directory_attributes(&child)?,
            ));
            probe.close()?;
        }
        if defaults
            .as_ref()
            .is_some_and(|(mode, attrs)| entry.mode != *mode || entry.attributes != *attrs)
        {
            let name = std::str::from_utf8(&entry.path).map_err(|_| refusal())?;
            suspicious.push(name);
        }
    }
    for batch in suspicious.chunks(64) {
        let mut args = vec!["ls-files", "-z", "--"];
        args.extend_from_slice(batch);
        let output = git.spool(
            root,
            &args,
            crate::git::Invocation {
                env: &[("GIT_LITERAL_PATHSPECS", OsStr::new("1"))],
                spool_directory: Some(admin),
                reject_stderr: true,
                ..Default::default()
            },
        )?;
        if output.metadata()?.len() != 0 {
            return Err(refusal());
        }
    }
    Ok(())
}

pub(crate) fn directory_metadata_matches(
    current: &[DirectoryState],
    saved: &mut SavedDirectories,
) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let current = current
        .iter()
        .map(|entry| (entry.path.as_slice(), entry))
        .collect::<std::collections::HashMap<_, _>>();
    while let Some(entry) = saved.next_entry()? {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&entry.path));
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Ok(false);
        }
        let Some(observed) = current.get(entry.path.as_slice()) else {
            continue;
        };
        if observed.mode != entry.mode || observed.attributes != entry.attributes {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn directory_attributes(path: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    use std::{ffi::CString, os::fd::AsRawFd};
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
        // SAFETY: path is NUL-terminated; null storage with size zero queries only.
        let size = unsafe { libc::llistxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
        if size == 0 {
            return Ok(Vec::new());
        }
        if size < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTSUP) {
                return Ok(Vec::new());
            }
            return Err(error.into());
        }
    }
    let file = fs::File::open(path)?;
    let fd = file.as_raw_fd();
    let list = |buffer: *mut libc::c_char, size| {
        // SAFETY: fd is owned by file, and each caller supplies either a null
        // size query or writable storage of exactly size bytes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::flistxattr(fd, buffer, size)
            }
            #[cfg(target_os = "macos")]
            {
                libc::flistxattr(fd, buffer, size, 0)
            }
        }
    };
    let size = list(std::ptr::null_mut(), 0);
    if size == 0 {
        return Ok(Vec::new());
    }
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(Vec::new());
        }
        return Err(error.into());
    }
    let mut names = vec![0u8; size as usize];
    let size = list(names.as_mut_ptr().cast(), names.len());
    if size < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    names.truncate(size as usize);
    let mut attributes = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let key = CString::new(name).map_err(std::io::Error::other)?;
        let get = |buffer: *mut libc::c_void, size| {
            // SAFETY: file owns fd, key is terminated, and the caller provides
            // a null query or a writable buffer covering exactly size bytes.
            unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::fgetxattr(fd, key.as_ptr(), buffer, size)
                }
                #[cfg(target_os = "macos")]
                {
                    libc::fgetxattr(fd, key.as_ptr(), buffer, size, 0, 0)
                }
            }
        };
        let size = get(std::ptr::null_mut(), 0);
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut value = vec![0u8; size as usize];
        let size = get(value.as_mut_ptr().cast(), value.len());
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        value.truncate(size as usize);
        attributes.push((name.to_vec(), value));
    }
    attributes.sort();
    Ok(attributes)
}

pub(crate) fn first_special(root: &Path, timeout: std::time::Duration) -> Result<Option<PathBuf>> {
    use std::{io::Seek, os::unix::ffi::OsStrExt};
    for path in crate::walk::inventory(root, Default::default())?.specials {
        let mut input = tempfile::tempfile()?;
        input.write_all(
            path.strip_prefix(root)
                .map_err(std::io::Error::other)?
                .as_os_str()
                .as_bytes(),
        )?;
        input.write_all(&[0])?;
        input.rewind()?;
        match crate::git::Git::new(timeout).run_with(
            root,
            &["check-ignore", "--stdin", "-z"],
            crate::git::Invocation {
                stdin: Some(input),
                reject_stderr: true,
                ..crate::git::Invocation::snapshot()
            },
        ) {
            Ok(_) => (),
            Err(error) if error.git_exit_code() == Some(1) => return Ok(Some(path)),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

pub(crate) fn make_directory_writable(directory: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.mode() & 0o700 != 0o700 {
        // SAFETY: geteuid has no preconditions and only observes this process.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::new(
                ErrorCode::RecoveryFailed,
                "Cannot repair a directory owned by another user",
            )
            .with_details(|details| details.path = Some(directory.into())));
        }
        fs::set_permissions(
            directory,
            fs::Permissions::from_mode((metadata.mode() & 0o7777) | 0o700),
        )?;
    }
    Ok(())
}

pub(crate) fn make_writable(root: &Path) -> Result<()> {
    fn visit(directory: &Path, device: u64) -> Result<()> {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.dev() != device {
            return Err(Error::new(
                ErrorCode::RecoveryFailed,
                "Mounted or changed directory in workspace inventory",
            )
            .with_details(|details| details.path = Some(directory.into())));
        }
        make_directory_writable(directory, &metadata)?;
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), device)?;
            }
        }
        Ok(())
    }
    visit(root, fs::symlink_metadata(root)?.dev())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_inventory_has_a_binary_contract_and_rejects_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .canonicalize()
            .unwrap()
            .join(DIRECTORY_STATE_NAME);
        let entry = DirectoryState {
            path: b"raw-\xff".to_vec(),
            mode: 0o755,
            attributes: vec![(b"user.test".to_vec(), b"value".to_vec())],
        };
        write_directory_state(&path, &[entry]).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(
            bytes.starts_with(b"groove-directories-v2\n\xed\x01\0\0\x05\0\0\0raw-\xff\x01\0\0\0")
        );
        let mut saved = SavedDirectories::open(&path).unwrap();
        let read = saved.next_entry().unwrap().unwrap();
        assert_eq!(read.path, b"raw-\xff");
        assert_eq!(read.mode, 0o755);
        assert_eq!(
            read.attributes,
            vec![(b"user.test".to_vec(), b"value".to_vec())]
        );
        assert!(saved.next_entry().unwrap().is_none());
        for length in 0..bytes.len() {
            if length == MAGIC.len() {
                continue;
            } // A record-boundary EOF is clean.
            fs::write(&path, &bytes[..length]).unwrap();
            let result = SavedDirectories::open(&path).and_then(|mut saved| saved.next_entry());
            assert!(
                matches!(result, Err(error) if error.code == ErrorCode::RecoveryFailed),
                "length {length}"
            );
        }
        fs::write(&path, b"groove-directories-v1\n").unwrap();
        assert!(SavedDirectories::open(&path).is_err());
        let oversized = DirectoryState {
            path: vec![b'x'; FIELD_LIMIT + 1],
            mode: 0o755,
            attributes: Vec::new(),
        };
        assert!(write_directory_state(&path, &[oversized]).is_err());
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&0o755_u32.to_le_bytes());
        bytes.extend_from_slice(&((FIELD_LIMIT + 1) as u32).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(SavedDirectories::open(&path).unwrap().next_entry().is_err());
    }
}
