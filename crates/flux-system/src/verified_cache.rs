//! Wire-independent, owner-private staging and cache primitives for verified executables.

#[cfg(unix)]
use sha2::{Digest, Sha256};
use std::fmt;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(unix)]
use std::io::Read;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Resource bounds for one complete cached install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheConfig {
    pub max_install_bytes: u64,
    pub max_install_members: usize,
}

/// A path-safe, wire-agnostic identifier for one exact release and target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn new(value: impl Into<String>) -> Result<Self, CacheError> {
        let value = value.into();
        let safe = !value.is_empty()
            && value.len() <= 128
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !safe {
            return Err(CacheError::InvalidKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelativeCachePath(PathBuf);

impl RelativeCachePath {
    fn new(path: impl AsRef<Path>) -> Result<Self, CacheError> {
        let path = path.as_ref();
        let valid = !path.as_os_str().is_empty()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(name) if !name.is_empty()));
        if !valid {
            return Err(CacheError::InvalidRelativePath);
        }
        Ok(Self(path.to_path_buf()))
    }
}

/// Cache identity required to revalidate an executable on every use.
///
/// Signed release/channel parsing deliberately lives above this type. The cache receives only the
/// already-selected opaque key, relative executable path, and expected digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSpec {
    key: CacheKey,
    executable: RelativeCachePath,
    executable_sha256: [u8; 32],
}

impl InstallSpec {
    pub fn new(
        key: CacheKey,
        executable: impl AsRef<Path>,
        executable_sha256: [u8; 32],
    ) -> Result<Self, CacheError> {
        Ok(Self {
            key,
            executable: RelativeCachePath::new(executable)?,
            executable_sha256,
        })
    }

    pub fn key(&self) -> &CacheKey {
        &self.key
    }
}

/// Closed storage outcomes. Callers map these onto their product diagnostics without exposing paths
/// or OS error text.
#[derive(Debug)]
pub enum CacheError {
    InvalidConfig,
    InvalidKey,
    InvalidRelativePath,
    CandidateRefused,
    Permissions,
    Ownership,
    Symlink,
    InvalidEntry,
    DigestMismatch,
    InstallTooLarge,
    TooManyMembers,
    Quarantined,
    OwnerProtectionUnavailable,
    Io(io::Error),
}

impl CacheError {
    /// Convert a verifier/extractor refusal into a value-free cache outcome.
    pub fn candidate_refused(_private_detail: &str) -> Self {
        Self::CandidateRefused
    }
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid verified-cache bounds",
            Self::InvalidKey => "invalid verified-cache key",
            Self::InvalidRelativePath => "invalid verified-cache relative path",
            Self::CandidateRefused => "verified-cache candidate refused",
            Self::Permissions => "verified-cache permissions refused",
            Self::Ownership => "verified-cache ownership refused",
            Self::Symlink => "verified-cache symlink refused",
            Self::InvalidEntry => "verified-cache entry refused",
            Self::DigestMismatch => "verified-cache digest mismatch",
            Self::InstallTooLarge => "verified-cache install exceeds byte bound",
            Self::TooManyMembers => "verified-cache install exceeds member bound",
            Self::Quarantined => "verified-cache install quarantined",
            Self::OwnerProtectionUnavailable => "owner-only cache protection unavailable",
            Self::Io(error) => return write!(formatter, "verified-cache I/O failed: {error}"),
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for CacheError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A cache result that was fully checked while holding its per-release lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedInstall {
    root: PathBuf,
    executable: PathBuf,
    cache_hit: bool,
}

impl CachedInstall {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }
}

/// Owner-private, bounded, atomically published executable cache.
#[derive(Debug)]
pub struct VerifiedCache {
    root: PathBuf,
    config: CacheConfig,
}

impl VerifiedCache {
    /// Open or create a private cache root.
    ///
    /// Unix ownership and modes are enforced directly. Windows fails closed until the guarded host
    /// supplies the user-SID ACL implementation; it never silently treats DOS readonly bits as an
    /// owner-only ACL.
    pub fn open(root: impl Into<PathBuf>, config: CacheConfig) -> Result<Self, CacheError> {
        if config.max_install_bytes == 0 || config.max_install_members == 0 {
            return Err(CacheError::InvalidConfig);
        }
        let root = root.into();
        #[cfg(windows)]
        {
            let _ = root;
            return Err(CacheError::OwnerProtectionUnavailable);
        }

        #[cfg(not(windows))]
        {
            ensure_private_dir(&root)?;
            for child in ["locks", "releases", "staging", "quarantine"] {
                ensure_private_dir(&root.join(child))?;
            }
            let cache = Self { root, config };
            cache.validate_roots()?;
            Ok(cache)
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn release_path(&self, key: &CacheKey) -> PathBuf {
        self.root.join("releases").join(key.as_str())
    }

    /// Revalidate a cache hit. Any visible invalid install is quarantined and never returned.
    pub fn lookup(&self, spec: &InstallSpec) -> Result<Option<CachedInstall>, CacheError> {
        self.validate_roots()?;
        let _lock = self.lock(spec.key())?;
        self.lookup_locked(spec)
    }

    /// Install when absent, or return a fully revalidated cache hit.
    ///
    /// If a visible install fails validation this call quarantines it and returns immediately. It
    /// intentionally does not invoke `build`, preventing one invocation from hiding an incident by
    /// redownloading. Network imports use this identical staging/publish path.
    pub fn install<F>(&self, spec: &InstallSpec, build: F) -> Result<CachedInstall, CacheError>
    where
        F: FnOnce(&StagingArea) -> Result<(), CacheError>,
    {
        self.validate_roots()?;
        let _lock = self.lock(spec.key())?;
        if let Some(install) = self.lookup_locked(spec)? {
            return Ok(install);
        }
        self.build_and_publish(spec, false, build)
    }

    /// Explicitly build and verify a replacement before disturbing an existing install.
    ///
    /// The lifecycle owner must ensure no supervised process is live before calling this primitive.
    /// A failed candidate leaves the prior directory untouched. A successful candidate is published
    /// by rename on the cache filesystem; the retired known-good directory is deleted only after the
    /// replacement is visible.
    pub fn replace<F>(&self, spec: &InstallSpec, build: F) -> Result<CachedInstall, CacheError>
    where
        F: FnOnce(&StagingArea) -> Result<(), CacheError>,
    {
        self.validate_roots()?;
        let _lock = self.lock(spec.key())?;
        self.build_and_publish(spec, true, build)
    }

    pub fn quarantine_entries(&self, key: &CacheKey) -> Result<usize, CacheError> {
        self.validate_roots()?;
        let _lock = self.lock(key)?;
        let release = self.root.join("quarantine").join(key.as_str());
        match no_follow_metadata(&release)? {
            None => Ok(0),
            Some(metadata) => {
                validate_private_dir_metadata(&metadata)?;
                let mut count = 0usize;
                for entry in fs::read_dir(release)? {
                    let entry = entry?;
                    if entry.file_name() != "incident" {
                        return Err(CacheError::InvalidEntry);
                    }
                    count += 1;
                }
                Ok(count)
            }
        }
    }

    fn validate_roots(&self) -> Result<(), CacheError> {
        validate_private_dir(&self.root)?;
        for child in ["locks", "releases", "staging", "quarantine"] {
            validate_private_dir(&self.root.join(child))?;
        }
        Ok(())
    }

    fn lock(&self, key: &CacheKey) -> Result<ReleaseLock, CacheError> {
        let path = self
            .root
            .join("locks")
            .join(format!("{}.lock", key.as_str()));
        let file = open_private_lock(&path)?;
        lock_file_exclusive(&file)?;
        validate_private_file_metadata(&file.metadata()?, false)?;
        Ok(ReleaseLock { _file: file })
    }

    fn lookup_locked(&self, spec: &InstallSpec) -> Result<Option<CachedInstall>, CacheError> {
        let release = self.release_path(spec.key());
        if no_follow_metadata(&release)?.is_none() {
            return Ok(None);
        }
        match self.validate_install(&release, spec) {
            Ok(()) => Ok(Some(self.cached_install(spec, true))),
            Err(_) => {
                self.quarantine_locked(spec.key(), &release)?;
                Err(CacheError::Quarantined)
            }
        }
    }

    fn build_and_publish<F>(
        &self,
        spec: &InstallSpec,
        replace: bool,
        build: F,
    ) -> Result<CachedInstall, CacheError>
    where
        F: FnOnce(&StagingArea) -> Result<(), CacheError>,
    {
        self.remove_stale_staging(spec.key())?;
        let mut staging = StagingArea::create(&self.root, spec.key())?;
        build(&staging)?;
        self.validate_install(staging.root(), spec)?;

        let release = self.release_path(spec.key());
        if replace && no_follow_metadata(&release)?.is_some() {
            let old_valid = self.validate_install(&release, spec).is_ok();
            if old_valid {
                let retired = self.unique_staging_path(spec.key(), "retired");
                fs::rename(&release, &retired)?;
                match staging.publish(&release) {
                    Ok(()) => remove_tree_no_follow(&retired)?,
                    Err(error) => {
                        let _ = fs::rename(&retired, &release);
                        return Err(error);
                    }
                }
            } else {
                self.quarantine_locked(spec.key(), &release)?;
                staging.publish(&release)?;
            }
        } else {
            staging.publish(&release)?;
        }

        if self.validate_install(&release, spec).is_err() {
            self.quarantine_locked(spec.key(), &release)?;
            return Err(CacheError::Quarantined);
        }
        Ok(self.cached_install(spec, false))
    }

    fn cached_install(&self, spec: &InstallSpec, cache_hit: bool) -> CachedInstall {
        let root = self.release_path(spec.key());
        CachedInstall {
            executable: root.join(&spec.executable.0),
            root,
            cache_hit,
        }
    }

    fn validate_install(&self, root: &Path, spec: &InstallSpec) -> Result<(), CacheError> {
        let mut members = 0usize;
        let mut bytes = 0u64;
        validate_tree(
            root,
            root,
            &spec.executable.0,
            &mut members,
            &mut bytes,
            self.config,
        )?;
        let executable = root.join(&spec.executable.0);
        let actual = sha256_no_follow(&executable)?;
        if actual != spec.executable_sha256 {
            return Err(CacheError::DigestMismatch);
        }
        Ok(())
    }

    fn quarantine_locked(&self, key: &CacheKey, release: &Path) -> Result<(), CacheError> {
        make_tree_non_executable(release)?;
        let quarantine_release = self.root.join("quarantine").join(key.as_str());
        ensure_private_dir(&quarantine_release)?;
        for entry in fs::read_dir(&quarantine_release)? {
            if entry?.file_name() != "incident" {
                return Err(CacheError::InvalidEntry);
            }
        }
        let incident = quarantine_release.join("incident");
        if no_follow_metadata(&incident)?.is_some() {
            remove_tree_no_follow(&incident)?;
        }
        fs::rename(release, incident)?;
        Ok(())
    }

    fn remove_stale_staging(&self, key: &CacheKey) -> Result<(), CacheError> {
        let prefix = format!("{}--", key.as_str());
        for entry in fs::read_dir(self.root.join("staging"))? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(CacheError::InvalidEntry);
            };
            if name.starts_with(&prefix) {
                remove_tree_no_follow(&entry.path())?;
            }
        }
        Ok(())
    }

    fn unique_staging_path(&self, key: &CacheKey, purpose: &str) -> PathBuf {
        self.root.join("staging").join(format!(
            "{}--{}-{}-{}",
            key.as_str(),
            purpose,
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

#[derive(Debug)]
struct ReleaseLock {
    _file: File,
}

static NEXT_STAGING: AtomicU64 = AtomicU64::new(0);

/// A newly created private directory on the cache filesystem. Its only mutation methods preserve
/// the cache's no-follow and owner-only invariants.
#[derive(Debug)]
pub struct StagingArea {
    root: PathBuf,
    published: std::sync::atomic::AtomicBool,
}

impl StagingArea {
    fn create(cache_root: &Path, key: &CacheKey) -> Result<Self, CacheError> {
        let path = cache_root.join("staging").join(format!(
            "{}--candidate-{}-{}",
            key.as_str(),
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_dir(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::metadata(cache_root)?.dev() != fs::metadata(&path)?.dev() {
                return Err(CacheError::InvalidEntry);
            }
        }
        Ok(Self {
            root: path,
            published: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create_dir(&self, relative: impl AsRef<Path>) -> Result<(), CacheError> {
        let relative = RelativeCachePath::new(relative)?;
        let destination = self.root.join(relative.0);
        let parent = destination
            .parent()
            .ok_or(CacheError::InvalidRelativePath)?;
        validate_private_dir(parent)?;
        create_private_dir(&destination)
    }

    pub fn write_private(
        &self,
        relative: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<(), CacheError> {
        self.write(relative, bytes, false)
    }

    pub fn write_executable(
        &self,
        relative: impl AsRef<Path>,
        bytes: &[u8],
    ) -> Result<(), CacheError> {
        self.write(relative, bytes, true)
    }

    fn write(
        &self,
        relative: impl AsRef<Path>,
        bytes: &[u8],
        executable: bool,
    ) -> Result<(), CacheError> {
        let relative = RelativeCachePath::new(relative)?;
        let destination = self.root.join(relative.0);
        let parent = destination
            .parent()
            .ok_or(CacheError::InvalidRelativePath)?;
        validate_private_dir(parent)?;
        let mut file = create_private_file(&destination, executable)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }

    fn publish(&mut self, destination: &Path) -> Result<(), CacheError> {
        fs::rename(&self.root, destination)?;
        self.published.store(true, Ordering::Release);
        Ok(())
    }
}

impl Drop for StagingArea {
    fn drop(&mut self) {
        if !self.published.load(Ordering::Acquire) {
            let _ = remove_tree_no_follow(&self.root);
        }
    }
}

fn no_follow_metadata(path: &Path) -> Result<Option<fs::Metadata>, CacheError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<(), CacheError> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)?;
    validate_private_dir(path)
}

#[cfg(not(unix))]
fn create_private_dir(_path: &Path) -> Result<(), CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

fn ensure_private_dir(path: &Path) -> Result<(), CacheError> {
    match no_follow_metadata(path)? {
        Some(_) => validate_private_dir(path),
        None => create_private_dir(path),
    }
}

fn validate_private_dir(path: &Path) -> Result<(), CacheError> {
    let metadata = no_follow_metadata(path)?.ok_or(CacheError::InvalidEntry)?;
    validate_private_dir_metadata(&metadata)
}

#[cfg(unix)]
fn validate_private_dir_metadata(metadata: &fs::Metadata) -> Result<(), CacheError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if metadata.file_type().is_symlink() {
        return Err(CacheError::Symlink);
    }
    if !metadata.is_dir() {
        return Err(CacheError::InvalidEntry);
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(CacheError::Ownership);
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(CacheError::Permissions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_dir_metadata(_metadata: &fs::Metadata) -> Result<(), CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

#[cfg(unix)]
fn create_private_file(path: &Path, executable: bool) -> Result<File, CacheError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mode = if executable { 0o700 } else { 0o600 };
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_private_file_metadata(&file.metadata()?, executable)?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_private_file(_path: &Path, _executable: bool) -> Result<File, CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

#[cfg(unix)]
fn open_private_lock(path: &Path) -> Result<File, CacheError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_private_file_metadata(&file.metadata()?, false)?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_lock(_path: &Path) -> Result<File, CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

#[cfg(unix)]
fn lock_file_exclusive(file: &File) -> Result<(), CacheError> {
    use std::os::fd::AsRawFd;

    loop {
        // SAFETY: `file` owns a live descriptor for the duration of this call. `flock` neither
        // retains the pointer nor accesses Rust memory; the lock is released when ReleaseLock drops
        // the descriptor.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

#[cfg(not(unix))]
fn lock_file_exclusive(_file: &File) -> Result<(), CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

#[cfg(unix)]
fn validate_private_file_metadata(
    metadata: &fs::Metadata,
    executable: bool,
) -> Result<(), CacheError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if metadata.file_type().is_symlink() {
        return Err(CacheError::Symlink);
    }
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(CacheError::InvalidEntry);
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(CacheError::Ownership);
    }
    let expected = if executable { 0o700 } else { 0o600 };
    if metadata.permissions().mode() & 0o777 != expected {
        return Err(CacheError::Permissions);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file_metadata(
    _metadata: &fs::Metadata,
    _executable: bool,
) -> Result<(), CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

fn validate_tree(
    install_root: &Path,
    current: &Path,
    executable: &Path,
    members: &mut usize,
    bytes: &mut u64,
    config: CacheConfig,
) -> Result<(), CacheError> {
    validate_private_dir(current)?;
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        *members = members.checked_add(1).ok_or(CacheError::TooManyMembers)?;
        if *members > config.max_install_members {
            return Err(CacheError::TooManyMembers);
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(CacheError::Symlink);
        }
        if metadata.is_dir() {
            validate_tree(install_root, &path, executable, members, bytes, config)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(install_root)
                .map_err(|_| CacheError::InvalidEntry)?;
            validate_private_file_metadata(&metadata, relative == executable)?;
            *bytes = bytes
                .checked_add(metadata.len())
                .ok_or(CacheError::InstallTooLarge)?;
            if *bytes > config.max_install_bytes {
                return Err(CacheError::InstallTooLarge);
            }
        } else {
            return Err(CacheError::InvalidEntry);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn sha256_no_follow(path: &Path) -> Result<[u8; 32], CacheError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    validate_private_file_metadata(&file.metadata()?, true)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}

#[cfg(not(unix))]
fn sha256_no_follow(_path: &Path) -> Result<[u8; 32], CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

fn make_tree_non_executable(path: &Path) -> Result<(), CacheError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            make_tree_non_executable(&entry?.path())?;
        }
        set_private_mode(path, 0o700)?;
    } else if metadata.is_file() {
        set_private_mode(path, 0o600)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_mode(path: &Path, mode: u32) -> Result<(), CacheError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path, _mode: u32) -> Result<(), CacheError> {
    Err(CacheError::OwnerProtectionUnavailable)
}

fn remove_tree_no_follow(path: &Path) -> Result<(), CacheError> {
    let Some(metadata) = no_follow_metadata(path)? else {
        return Ok(());
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CacheConfig, CacheError, CacheKey, InstallSpec, VerifiedCache};
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::sync::{Arc, Barrier};

    fn digest(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    #[derive(Debug)]
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "flux-verified-cache-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (TestDir, VerifiedCache, InstallSpec) {
        let temp = TestDir::new();
        let cache = VerifiedCache::open(
            temp.path().join("cache"),
            CacheConfig {
                max_install_bytes: 1024,
                max_install_members: 8,
            },
        )
        .unwrap();
        let spec = InstallSpec::new(
            CacheKey::new("v1.2.3-linux-x86_64").unwrap(),
            "bin/flux-exchange",
            digest(b"verified executable"),
        )
        .unwrap();
        (temp, cache, spec)
    }

    #[test]
    fn install_is_private_and_published_only_after_complete_staging() {
        let (_temp, cache, spec) = fixture();
        let refused = cache.install(&spec, |stage| {
            stage.create_dir("bin")?;
            stage.write_executable("bin/flux-exchange", b"partial")?;
            Err(CacheError::candidate_refused("interrupted"))
        });
        assert!(matches!(refused, Err(CacheError::CandidateRefused)));
        assert!(!cache.release_path(spec.key()).exists());

        let installed = cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
        assert!(!installed.cache_hit());
        assert!(installed.executable().is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(cache.root()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(installed.executable())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn concurrent_and_repeated_installs_build_once() {
        let (_temp, cache, spec) = fixture();
        let cache = Arc::new(cache);
        let spec = Arc::new(spec);
        let barrier = Arc::new(Barrier::new(2));
        let builds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let spec = Arc::clone(&spec);
            let barrier = Arc::clone(&barrier);
            let builds = Arc::clone(&builds);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                cache
                    .install(&spec, |stage| {
                        builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        stage.create_dir("bin")?;
                        stage.write_executable("bin/flux-exchange", b"verified executable")
                    })
                    .unwrap()
                    .cache_hit()
            }));
        }
        let hits = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hits.iter().filter(|hit| **hit).count(), 1);
        assert!(cache
            .install(&spec, |_| panic!("cache hit rebuilt"))
            .unwrap()
            .cache_hit());
    }

    #[cfg(unix)]
    #[test]
    fn cache_hit_rejects_symlinks_and_widened_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let (_temp, cache, spec) = fixture();
        let installed = cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
        fs::set_permissions(installed.executable(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(cache.lookup(&spec), Err(CacheError::Quarantined)));
        assert!(!cache.release_path(spec.key()).exists());

        cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
        let executable = cache.release_path(spec.key()).join("bin/flux-exchange");
        fs::remove_file(&executable).unwrap();
        symlink("/bin/true", &executable).unwrap();
        assert!(matches!(cache.lookup(&spec), Err(CacheError::Quarantined)));
    }

    #[test]
    fn digest_mismatch_is_quarantined_once_and_not_reinstalled_same_call() {
        let (_temp, cache, spec) = fixture();
        let installed = cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
        fs::write(installed.executable(), b"tampered executable").unwrap();

        let builds = std::cell::Cell::new(0);
        assert!(matches!(
            cache.install(&spec, |_| {
                builds.set(builds.get() + 1);
                Ok(())
            }),
            Err(CacheError::Quarantined)
        ));
        assert_eq!(builds.get(), 0);
        assert_eq!(cache.quarantine_entries(spec.key()).unwrap(), 1);

        cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
        fs::write(
            cache.release_path(spec.key()).join("bin/flux-exchange"),
            b"tampered again",
        )
        .unwrap();
        assert!(matches!(cache.lookup(&spec), Err(CacheError::Quarantined)));
        assert_eq!(cache.quarantine_entries(spec.key()).unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn staging_is_on_cache_filesystem() {
        use std::os::unix::fs::MetadataExt;
        let (_temp, cache, spec) = fixture();
        let cache_device = fs::metadata(cache.root()).unwrap().dev();
        cache
            .install(&spec, |stage| {
                assert_eq!(fs::metadata(stage.root()).unwrap().dev(), cache_device);
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")
            })
            .unwrap();
    }

    #[test]
    fn explicit_replace_verifies_before_retiring_known_good_install() {
        let (_temp, cache, spec) = fixture();
        cache
            .install(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")?;
                stage.write_private("audit", b"old")
            })
            .unwrap();

        assert!(matches!(
            cache.replace(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"wrong")?;
                stage.write_private("audit", b"new")
            }),
            Err(CacheError::DigestMismatch)
        ));
        assert_eq!(
            fs::read(cache.release_path(spec.key()).join("audit")).unwrap(),
            b"old"
        );

        let replaced = cache
            .replace(&spec, |stage| {
                stage.create_dir("bin")?;
                stage.write_executable("bin/flux-exchange", b"verified executable")?;
                stage.write_private("audit", b"new")
            })
            .unwrap();
        assert!(!replaced.cache_hit());
        assert_eq!(
            fs::read(cache.release_path(spec.key()).join("audit")).unwrap(),
            b"new"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cache_root_symlink_is_refused_without_following_it() {
        use std::os::unix::fs::symlink;
        let temp = TestDir::new();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let root = temp.path().join("cache");
        symlink(&outside, &root).unwrap();
        assert!(matches!(
            VerifiedCache::open(
                root,
                CacheConfig {
                    max_install_bytes: 1,
                    max_install_members: 1,
                }
            ),
            Err(CacheError::Symlink)
        ));
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }
}
