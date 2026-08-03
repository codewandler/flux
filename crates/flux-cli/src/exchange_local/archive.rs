//! Pure, wire-neutral archive admission for managed Exchange releases.
//!
//! Archive decoding and filesystem writes remain outside this module. A format-specific reader
//! first presents each entry header to [`ArchiveValidator::begin_member`], streams the body while
//! enforcing the returned size, and then supplies its observed byte count and SHA-256. This keeps
//! provider-owned manifest fields out of Flux while giving every reader one fail-closed admission
//! policy.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub(crate) type Sha256Digest = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArchiveLimits {
    pub(crate) max_members: usize,
    pub(crate) max_path_bytes: usize,
    pub(crate) max_member_bytes: u64,
    pub(crate) max_expanded_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MemberKind {
    File,
    Directory,
    Symlink,
    HardLink,
    BlockDevice,
    CharacterDevice,
    Fifo,
    Socket,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum MemberExpectation {
    File { size: u64, sha256: Sha256Digest },
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AllowedMember {
    path: String,
    expectation: MemberExpectation,
}

impl AllowedMember {
    pub(crate) fn file(path: impl Into<String>, size: u64, sha256: Sha256Digest) -> Self {
        Self {
            path: path.into(),
            expectation: MemberExpectation::File { size, sha256 },
        }
    }

    pub(crate) fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            expectation: MemberExpectation::Directory,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathRefusal {
    Empty,
    TooLong,
    Absolute,
    Parent,
    Backslash,
    Nul,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArchiveRefusal {
    InvalidLimits,
    InvalidPolicyPath { reason: PathRefusal },
    DuplicatePolicyMember,
    PolicyExceedsMemberLimit,
    InvalidMemberPath { reason: PathRefusal },
    MemberInProgress,
    NoMemberInProgress,
    PlanMismatch,
    ArchiveAlreadyRefused,
    ArchiveAlreadyFinished,
    TooManyMembers,
    DuplicateMember,
    UndeclaredMember,
    UnsupportedMemberKind,
    MemberKindMismatch,
    DirectoryHasData,
    MemberTooLarge,
    ExpandedSizeOverflow,
    ExpandedSizeLimit,
    DeclaredSizeMismatch,
    ObservedSizeMismatch,
    DigestMissing,
    DigestMismatch,
    TrailingData,
    MissingMember,
}

#[derive(Clone)]
pub(crate) struct ArchivePolicy {
    limits: ArchiveLimits,
    members: BTreeMap<String, MemberExpectation>,
}

impl fmt::Debug for ArchivePolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArchivePolicy")
            .finish_non_exhaustive()
    }
}

impl ArchivePolicy {
    pub(crate) fn new(
        limits: ArchiveLimits,
        members: impl IntoIterator<Item = AllowedMember>,
    ) -> Result<Self, ArchiveRefusal> {
        if limits.max_members == 0 || limits.max_path_bytes == 0 {
            return Err(ArchiveRefusal::InvalidLimits);
        }

        let mut normalized = BTreeMap::new();
        for member in members {
            if normalized.len() >= limits.max_members {
                return Err(ArchiveRefusal::PolicyExceedsMemberLimit);
            }
            let path = normalize_member_path(&member.path, limits.max_path_bytes)
                .map_err(|reason| ArchiveRefusal::InvalidPolicyPath { reason })?;
            if normalized
                .insert(path.clone(), member.expectation)
                .is_some()
            {
                return Err(ArchiveRefusal::DuplicatePolicyMember);
            }
        }

        Ok(Self {
            limits,
            members: normalized,
        })
    }
}

#[derive(Eq, PartialEq)]
pub(crate) struct MemberPlan {
    path: String,
    declared_size: u64,
    expected_sha256: Option<Sha256Digest>,
}

impl fmt::Debug for MemberPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("MemberPlan").finish_non_exhaustive()
    }
}

impl MemberPlan {
    /// The normalized relative path admitted by the closed policy.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// The exact number of bytes the archive reader may consume for this member.
    pub(crate) fn declared_size(&self) -> u64 {
        self.declared_size
    }

    /// The digest the streaming reader must compare after consuming a regular file.
    pub(crate) fn expected_sha256(&self) -> Option<Sha256Digest> {
        self.expected_sha256
    }
}

pub(crate) struct ArchiveValidator<'a> {
    policy: &'a ArchivePolicy,
    seen: BTreeSet<String>,
    active_path: Option<String>,
    expanded_bytes: u64,
    finished: bool,
    refused: bool,
}

impl<'a> ArchiveValidator<'a> {
    pub(crate) fn new(policy: &'a ArchivePolicy) -> Self {
        Self {
            policy,
            seen: BTreeSet::new(),
            active_path: None,
            expanded_bytes: 0,
            finished: false,
            refused: false,
        }
    }

    pub(crate) fn begin_member(
        &mut self,
        path: &str,
        kind: MemberKind,
        declared_size: u64,
    ) -> Result<MemberPlan, ArchiveRefusal> {
        if self.refused {
            return Err(ArchiveRefusal::ArchiveAlreadyRefused);
        }
        let result = self.begin_member_inner(path, kind, declared_size);
        if result.is_err() {
            self.refused = true;
        }
        result
    }

    fn begin_member_inner(
        &mut self,
        path: &str,
        kind: MemberKind,
        declared_size: u64,
    ) -> Result<MemberPlan, ArchiveRefusal> {
        if self.finished {
            return Err(ArchiveRefusal::ArchiveAlreadyFinished);
        }
        if self.active_path.is_some() {
            return Err(ArchiveRefusal::MemberInProgress);
        }

        let path = normalize_member_path(path, self.policy.limits.max_path_bytes)
            .map_err(|reason| ArchiveRefusal::InvalidMemberPath { reason })?;
        if self.seen.contains(&path) {
            return Err(ArchiveRefusal::DuplicateMember);
        }
        if self
            .seen
            .len()
            .checked_add(1)
            .is_none_or(|count| count > self.policy.limits.max_members)
        {
            return Err(ArchiveRefusal::TooManyMembers);
        }
        if !matches!(kind, MemberKind::File | MemberKind::Directory) {
            return Err(ArchiveRefusal::UnsupportedMemberKind);
        }

        let expectation = self
            .policy
            .members
            .get(&path)
            .ok_or(ArchiveRefusal::UndeclaredMember)?;
        let expected_sha256 = match (kind, expectation) {
            (MemberKind::File, MemberExpectation::File { size, sha256 }) => {
                if declared_size > self.policy.limits.max_member_bytes {
                    return Err(ArchiveRefusal::MemberTooLarge);
                }
                if declared_size != *size {
                    return Err(ArchiveRefusal::DeclaredSizeMismatch);
                }
                Some(*sha256)
            }
            (MemberKind::Directory, MemberExpectation::Directory) => {
                if declared_size != 0 {
                    return Err(ArchiveRefusal::DirectoryHasData);
                }
                None
            }
            _ => return Err(ArchiveRefusal::MemberKindMismatch),
        };

        let expanded_bytes = self
            .expanded_bytes
            .checked_add(declared_size)
            .ok_or(ArchiveRefusal::ExpandedSizeOverflow)?;
        if expanded_bytes > self.policy.limits.max_expanded_bytes {
            return Err(ArchiveRefusal::ExpandedSizeLimit);
        }

        self.expanded_bytes = expanded_bytes;
        self.seen.insert(path.clone());
        self.active_path = Some(path.clone());
        Ok(MemberPlan {
            path,
            declared_size,
            expected_sha256,
        })
    }

    pub(crate) fn finish_member(
        &mut self,
        plan: MemberPlan,
        observed_size: u64,
        observed_sha256: Option<Sha256Digest>,
    ) -> Result<(), ArchiveRefusal> {
        if self.refused {
            return Err(ArchiveRefusal::ArchiveAlreadyRefused);
        }
        let result = self.finish_member_inner(plan, observed_size, observed_sha256);
        if result.is_err() {
            self.refused = true;
        }
        result
    }

    fn finish_member_inner(
        &mut self,
        plan: MemberPlan,
        observed_size: u64,
        observed_sha256: Option<Sha256Digest>,
    ) -> Result<(), ArchiveRefusal> {
        if self.finished {
            return Err(ArchiveRefusal::ArchiveAlreadyFinished);
        }
        let active_path = self
            .active_path
            .as_deref()
            .ok_or(ArchiveRefusal::NoMemberInProgress)?;
        if active_path != plan.path {
            return Err(ArchiveRefusal::PlanMismatch);
        }
        if observed_size != plan.declared_size {
            return Err(ArchiveRefusal::ObservedSizeMismatch);
        }
        match (plan.expected_sha256, observed_sha256) {
            (Some(_), None) => return Err(ArchiveRefusal::DigestMissing),
            (Some(expected), Some(observed)) if expected != observed => {
                return Err(ArchiveRefusal::DigestMismatch);
            }
            (None, Some(_)) => return Err(ArchiveRefusal::DigestMismatch),
            _ => {}
        }

        self.active_path = None;
        Ok(())
    }

    pub(crate) fn finish_archive(&mut self, trailing_bytes: u64) -> Result<(), ArchiveRefusal> {
        if self.refused {
            return Err(ArchiveRefusal::ArchiveAlreadyRefused);
        }
        let result = self.finish_archive_inner(trailing_bytes);
        if result.is_err() {
            self.refused = true;
        }
        result
    }

    fn finish_archive_inner(&mut self, trailing_bytes: u64) -> Result<(), ArchiveRefusal> {
        if self.finished {
            return Err(ArchiveRefusal::ArchiveAlreadyFinished);
        }
        if self.active_path.is_some() {
            return Err(ArchiveRefusal::MemberInProgress);
        }
        if trailing_bytes != 0 {
            return Err(ArchiveRefusal::TrailingData);
        }
        if self.seen.len() != self.policy.members.len()
            || self
                .policy
                .members
                .keys()
                .any(|path| !self.seen.contains(path))
        {
            return Err(ArchiveRefusal::MissingMember);
        }

        self.finished = true;
        Ok(())
    }
}

fn normalize_member_path(path: &str, max_path_bytes: usize) -> Result<String, PathRefusal> {
    if path.is_empty() {
        return Err(PathRefusal::Empty);
    }
    if path.len() > max_path_bytes {
        return Err(PathRefusal::TooLong);
    }
    if path.contains('\0') {
        return Err(PathRefusal::Nul);
    }
    if path.contains('\\') {
        return Err(PathRefusal::Backslash);
    }
    let bytes = path.as_bytes();
    if path.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
    {
        return Err(PathRefusal::Absolute);
    }

    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(PathRefusal::Parent),
            segment => segments.push(segment),
        }
    }
    if segments.is_empty() {
        return Err(PathRefusal::Empty);
    }
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Sha256Digest = [0x11; 32];
    const B: Sha256Digest = [0x22; 32];

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_members: 4,
            max_path_bytes: 64,
            max_member_bytes: 64,
            max_expanded_bytes: 96,
        }
    }

    fn policy() -> ArchivePolicy {
        ArchivePolicy::new(
            limits(),
            [
                AllowedMember::directory("bin"),
                AllowedMember::file("bin/flux-exchange", 12, A),
                AllowedMember::file("LICENSE", 7, B),
            ],
        )
        .unwrap()
    }

    fn finish(
        validator: &mut ArchiveValidator<'_>,
        path: &str,
        kind: MemberKind,
        size: u64,
        digest: Option<Sha256Digest>,
    ) -> Result<(), ArchiveRefusal> {
        let plan = validator.begin_member(path, kind, size)?;
        assert!(!plan.path().is_empty());
        assert_eq!(plan.declared_size(), size);
        validator.finish_member(plan, size, digest)
    }

    #[test]
    fn exact_closed_archive_is_accepted() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        finish(&mut validator, "bin", MemberKind::Directory, 0, None).unwrap();
        finish(
            &mut validator,
            "bin/flux-exchange",
            MemberKind::File,
            12,
            Some(A),
        )
        .unwrap();
        finish(&mut validator, "LICENSE", MemberKind::File, 7, Some(B)).unwrap();
        validator.finish_archive(0).unwrap();
    }

    #[test]
    fn unsafe_paths_refuse_before_body_admission() {
        for (path, reason) in [
            ("", PathRefusal::Empty),
            (
                "this-path-is-deliberately-longer-than-sixty-four-bytes/flux-exchange",
                PathRefusal::TooLong,
            ),
            ("/bin/exchange", PathRefusal::Absolute),
            ("C:/bin/exchange", PathRefusal::Absolute),
            ("bin/../exchange", PathRefusal::Parent),
            ("bin\\exchange", PathRefusal::Backslash),
            ("bin\0exchange", PathRefusal::Nul),
        ] {
            let policy = policy();
            let mut validator = ArchiveValidator::new(&policy);
            assert_eq!(
                validator.begin_member(path, MemberKind::File, 12),
                Err(ArchiveRefusal::InvalidMemberPath { reason }),
                "{path:?}"
            );
        }
    }

    #[test]
    fn duplicate_normalized_member_names_refuse() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        finish(
            &mut validator,
            "./bin//flux-exchange",
            MemberKind::File,
            12,
            Some(A),
        )
        .unwrap();
        assert_eq!(
            validator.begin_member("bin/flux-exchange", MemberKind::File, 12),
            Err(ArchiveRefusal::DuplicateMember)
        );
    }

    #[test]
    fn links_devices_and_other_special_members_refuse() {
        for kind in [
            MemberKind::Symlink,
            MemberKind::HardLink,
            MemberKind::BlockDevice,
            MemberKind::CharacterDevice,
            MemberKind::Fifo,
            MemberKind::Socket,
            MemberKind::Other,
        ] {
            let policy = policy();
            let mut validator = ArchiveValidator::new(&policy);
            assert_eq!(
                validator.begin_member("bin/flux-exchange", kind, 12),
                Err(ArchiveRefusal::UnsupportedMemberKind),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn closed_policy_refuses_extra_and_missing_members() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        assert_eq!(
            validator.begin_member("README", MemberKind::File, 1),
            Err(ArchiveRefusal::UndeclaredMember)
        );

        let mut validator = ArchiveValidator::new(&policy);
        finish(&mut validator, "bin", MemberKind::Directory, 0, None).unwrap();
        assert_eq!(
            validator.finish_archive(0),
            Err(ArchiveRefusal::MissingMember)
        );
    }

    #[test]
    fn size_and_digest_mismatches_refuse() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        assert_eq!(
            validator.begin_member("bin/flux-exchange", MemberKind::File, 11),
            Err(ArchiveRefusal::DeclaredSizeMismatch)
        );

        let mut validator = ArchiveValidator::new(&policy);
        let plan = validator
            .begin_member("bin/flux-exchange", MemberKind::File, 12)
            .unwrap();
        assert_eq!(
            validator.finish_member(plan, 11, Some(A)),
            Err(ArchiveRefusal::ObservedSizeMismatch)
        );

        let mut validator = ArchiveValidator::new(&policy);
        let plan = validator
            .begin_member("bin/flux-exchange", MemberKind::File, 12)
            .unwrap();
        assert_eq!(plan.expected_sha256(), Some(A));
        assert_eq!(
            validator.finish_member(plan, 12, None),
            Err(ArchiveRefusal::DigestMissing)
        );

        let mut validator = ArchiveValidator::new(&policy);
        let plan = validator
            .begin_member("bin/flux-exchange", MemberKind::File, 12)
            .unwrap();
        assert_eq!(
            validator.finish_member(plan, 12, Some(B)),
            Err(ArchiveRefusal::DigestMismatch)
        );
    }

    #[test]
    fn member_and_expanded_bounds_refuse_without_allocating_member_bytes() {
        let bounded = ArchivePolicy::new(
            ArchiveLimits {
                max_members: 2,
                max_path_bytes: 64,
                max_member_bytes: 8,
                max_expanded_bytes: 10,
            },
            [
                AllowedMember::file("a", 8, A),
                AllowedMember::file("b", 3, B),
            ],
        )
        .unwrap();
        let mut validator = ArchiveValidator::new(&bounded);
        finish(&mut validator, "a", MemberKind::File, 8, Some(A)).unwrap();
        assert_eq!(
            validator.begin_member("b", MemberKind::File, 3),
            Err(ArchiveRefusal::ExpandedSizeLimit)
        );

        let too_large = ArchivePolicy::new(
            ArchiveLimits {
                max_members: 1,
                max_path_bytes: 64,
                max_member_bytes: 7,
                max_expanded_bytes: 8,
            },
            [AllowedMember::file("a", 8, A)],
        )
        .unwrap();
        assert_eq!(
            ArchiveValidator::new(&too_large).begin_member("a", MemberKind::File, 8),
            Err(ArchiveRefusal::MemberTooLarge)
        );
    }

    #[test]
    fn member_count_and_expanded_integer_overflow_refuse() {
        let overflow_policy = ArchivePolicy::new(
            ArchiveLimits {
                max_members: 2,
                max_path_bytes: 64,
                max_member_bytes: u64::MAX,
                max_expanded_bytes: u64::MAX,
            },
            [
                AllowedMember::file("a", u64::MAX, A),
                AllowedMember::file("b", 1, B),
            ],
        )
        .unwrap();
        let mut validator = ArchiveValidator::new(&overflow_policy);
        finish(&mut validator, "a", MemberKind::File, u64::MAX, Some(A)).unwrap();
        assert_eq!(
            validator.begin_member("b", MemberKind::File, 1),
            Err(ArchiveRefusal::ExpandedSizeOverflow)
        );

        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        finish(&mut validator, "bin", MemberKind::Directory, 0, None).unwrap();
        finish(
            &mut validator,
            "bin/flux-exchange",
            MemberKind::File,
            12,
            Some(A),
        )
        .unwrap();
        finish(&mut validator, "LICENSE", MemberKind::File, 7, Some(B)).unwrap();
        assert_eq!(
            validator.begin_member("LICENSE", MemberKind::File, 7),
            Err(ArchiveRefusal::DuplicateMember)
        );

        assert_eq!(
            ArchivePolicy::new(
                ArchiveLimits {
                    max_members: 1,
                    ..limits()
                },
                [
                    AllowedMember::file("a", 1, A),
                    AllowedMember::file("b", 1, B),
                ],
            )
            .unwrap_err(),
            ArchiveRefusal::PolicyExceedsMemberLimit
        );
    }

    #[test]
    fn directories_cannot_carry_data_or_file_digests() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        assert_eq!(
            validator.begin_member("bin", MemberKind::Directory, 1),
            Err(ArchiveRefusal::DirectoryHasData)
        );

        let mut validator = ArchiveValidator::new(&policy);
        let plan = validator
            .begin_member("bin", MemberKind::Directory, 0)
            .unwrap();
        assert_eq!(
            validator.finish_member(plan, 0, Some(A)),
            Err(ArchiveRefusal::DigestMismatch)
        );
    }

    #[test]
    fn trailing_bytes_and_incomplete_streams_refuse() {
        let policy = policy();
        let mut validator = ArchiveValidator::new(&policy);
        validator
            .begin_member("bin", MemberKind::Directory, 0)
            .unwrap();
        assert_eq!(
            validator.finish_archive(0),
            Err(ArchiveRefusal::MemberInProgress)
        );

        let mut validator = ArchiveValidator::new(&policy);
        finish(&mut validator, "bin", MemberKind::Directory, 0, None).unwrap();
        finish(
            &mut validator,
            "bin/flux-exchange",
            MemberKind::File,
            12,
            Some(A),
        )
        .unwrap();
        finish(&mut validator, "LICENSE", MemberKind::File, 7, Some(B)).unwrap();
        assert_eq!(
            validator.finish_archive(1),
            Err(ArchiveRefusal::TrailingData)
        );
    }

    #[test]
    fn any_refusal_is_terminal_and_member_plans_are_validator_bound() {
        let policy = policy();
        let mut refused = ArchiveValidator::new(&policy);
        assert_eq!(
            refused.begin_member("../escape", MemberKind::File, 1),
            Err(ArchiveRefusal::InvalidMemberPath {
                reason: PathRefusal::Parent
            })
        );
        assert_eq!(
            refused.begin_member("bin", MemberKind::Directory, 0),
            Err(ArchiveRefusal::ArchiveAlreadyRefused)
        );

        let mut first = ArchiveValidator::new(&policy);
        let mut second = ArchiveValidator::new(&policy);
        let first_plan = first.begin_member("bin", MemberKind::Directory, 0).unwrap();
        assert_eq!(
            second.finish_member(first_plan, 0, None),
            Err(ArchiveRefusal::NoMemberInProgress)
        );

        let mut first = ArchiveValidator::new(&policy);
        let mut second = ArchiveValidator::new(&policy);
        let first_plan = first.begin_member("bin", MemberKind::Directory, 0).unwrap();
        second.begin_member("LICENSE", MemberKind::File, 7).unwrap();
        assert_eq!(
            second.finish_member(first_plan, 0, None),
            Err(ArchiveRefusal::PlanMismatch)
        );
    }

    #[test]
    fn kind_mismatch_invalid_limits_and_finished_archive_refuse() {
        assert_eq!(
            ArchivePolicy::new(
                ArchiveLimits {
                    max_members: 0,
                    ..limits()
                },
                [AllowedMember::file("a", 1, A)],
            )
            .unwrap_err(),
            ArchiveRefusal::InvalidLimits
        );

        let policy = policy();
        assert_eq!(
            ArchiveValidator::new(&policy).begin_member("bin", MemberKind::File, 0),
            Err(ArchiveRefusal::MemberKindMismatch)
        );

        let empty = ArchivePolicy::new(limits(), []).unwrap();
        let mut validator = ArchiveValidator::new(&empty);
        validator.finish_archive(0).unwrap();
        assert_eq!(
            validator.finish_archive(0),
            Err(ArchiveRefusal::ArchiveAlreadyFinished)
        );
    }

    #[test]
    fn policy_paths_are_canonical_and_unique_after_normalization() {
        assert_eq!(
            ArchivePolicy::new(
                limits(),
                [
                    AllowedMember::file("./bin//flux-exchange", 12, A),
                    AllowedMember::file("bin/flux-exchange", 12, A),
                ],
            )
            .unwrap_err(),
            ArchiveRefusal::DuplicatePolicyMember
        );
        assert_eq!(
            ArchivePolicy::new(limits(), [AllowedMember::file("../escape", 1, A)]).unwrap_err(),
            ArchiveRefusal::InvalidPolicyPath {
                reason: PathRefusal::Parent,
            }
        );
    }
}
