//! Host metrics — the typed, closed vocabulary a substrate reports about **itself**, and the
//! native Linux reader behind it.
//!
//! Dispatch telemetry, substrate provenance and the usage observatory all describe *work flowing
//! through* a substrate. This module is the other axis: the substrate's own condition. Decision
//! 0018 rule 6 fixes the shape of it, and three properties are the whole contract:
//!
//! - **The vocabulary is closed.** [`MetricKind`] enumerates every kind a host may report and
//!   [`MetricReading`] enumerates the payloads. There is no `Custom(String, String)` escape hatch,
//!   so a backend cannot invent a dimension a consumer has no way to interpret. Every *value* is a
//!   number carrying its unit in the field name (`total_bytes`, `celsius`, `rpm`); the only strings
//!   are bounded identities naming *which instrument* answered.
//! - **Readings are bounded.** A mount table, a sensor list and an instrument label all have hard
//!   caps ([`MAX_MOUNTS`], [`MAX_SENSORS`], [`MAX_LABEL_BYTES`]), so a hostile or merely unusual
//!   machine cannot turn one metrics read into an unbounded allocation. A bound bites while a
//!   listing is *collected* rather than after it is finished, and where one drops something the
//!   answer says so ([`DiskUsage::omitted_mounts`]): a cap that truncates silently reports a
//!   machine with a hundred filesystems as a machine with thirty-two.
//! - **Unsupported is explicitly unavailable, never zero.** There are two distinct negatives and
//!   they are never collapsed: `Err(Unserved)` means *this substrate does not serve metrics at
//!   all* (the fail-closed default on [`crate::port::GuardedMetrics`]), while
//!   [`MetricAnswer::Unavailable`] means *it serves the family and this machine has no such
//!   instrument*. Neither is ever a zero reading — the same convention the board statistics
//!   contract fixes with its `absent` schema for a dimension it cannot measure.
//!
//! The parsing lives here, in `flux-system`, because reading `/proc`, `/sys` and `statvfs` **is**
//! filesystem IO and this crate is the one place that happens. A consumer crate receives typed
//! readings, never a file path.
//!
//! Which roots the native reader reads is held as a value ([`MetricsRoots`]) rather than baked into
//! the call sites, so a test pins procfs and sysfs fixtures on the [`System`](crate::System) it
//! drives — the same value-held-environment shape as [`WorktreeBase`](crate::WorktreeBase).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use flux_core::Result;
#[cfg(not(target_os = "linux"))]
use flux_core::{Error, GuardedIoError, GuardedIoFailure};

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// The most mounted filesystems one [`MetricReading::Disk`] reports.
pub const MAX_MOUNTS: usize = 32;

/// The most sensors one [`MetricReading::Temperature`] or [`MetricReading::FanSpeed`] reports.
pub const MAX_SENSORS: usize = 64;

/// The most bytes an instrument label carries. Labels come from `hwmon` chip files and mount
/// tables, so their length is a property of the machine rather than of this code.
pub const MAX_LABEL_BYTES: usize = 64;

// ---------------------------------------------------------------------------
// The closed vocabulary
// ---------------------------------------------------------------------------

/// The closed set of metric kinds a host substrate may report about its own substrate.
///
/// Closed on purpose: every consumer — an operator surface, a monitoring projection, a remote
/// protocol frame — can enumerate the whole space and render each member with the units it knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricKind {
    /// Fraction of a sampled window the processors spent outside idle.
    CpuUsage,
    /// The kernel's 1/5/15-minute run-queue averages.
    LoadAverage,
    /// Physical memory occupancy.
    Memory,
    /// Swap occupancy. Unavailable where no swap area is configured.
    Swap,
    /// Per-mount capacity and usage.
    Disk,
    /// How long the substrate has been up.
    Uptime,
    /// Temperature sensors, where the substrate exposes any.
    Temperature,
    /// Fan tachometers, where the substrate exposes any.
    FanSpeed,
}

impl MetricKind {
    /// Every kind, in the canonical order a snapshot reports them. A `read_metrics` result is
    /// bounded by this array's length by construction.
    pub const ALL: [MetricKind; 8] = [
        MetricKind::CpuUsage,
        MetricKind::LoadAverage,
        MetricKind::Memory,
        MetricKind::Swap,
        MetricKind::Disk,
        MetricKind::Uptime,
        MetricKind::Temperature,
        MetricKind::FanSpeed,
    ];

    /// The stable operator- and wire-facing token for this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            MetricKind::CpuUsage => "cpu",
            MetricKind::LoadAverage => "load",
            MetricKind::Memory => "memory",
            MetricKind::Swap => "swap",
            MetricKind::Disk => "disk",
            MetricKind::Uptime => "uptime",
            MetricKind::Temperature => "temperature",
            MetricKind::FanSpeed => "fan",
        }
    }

    /// The kind a token names, or `None` where nothing does.
    ///
    /// The inverse of [`as_str`](Self::as_str), and the *only* way a token from outside this
    /// process becomes a kind: the vocabulary is closed, so an unrecognized token is not a metric
    /// this build has never heard of — it is a token to refuse. A wire decoder that mapped it onto
    /// a neighbouring kind would report one instrument's measurement under another's name.
    pub fn from_token(token: &str) -> Option<MetricKind> {
        MetricKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == token)
    }
}

impl std::fmt::Display for MetricKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Processor occupancy measured over a real elapsed window.
///
/// The window is measured rather than assumed, and reported, so a consumer can tell a busy machine
/// from one whose accounting simply did not advance.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuUsage {
    /// Logical processors the substrate accounts for.
    pub logical_cores: u32,
    /// Fraction of `window` spent outside idle, in `0.0..=1.0`.
    pub busy_ratio: f64,
    /// The wall-clock window `busy_ratio` was measured over.
    pub window: Duration,
}

/// The kernel's run-queue averages. Dimensionless by definition — a count of runnable tasks, not a
/// percentage — so nothing here carries a byte or time unit.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadAverage {
    /// One-minute average.
    pub one_minute: f64,
    /// Five-minute average.
    pub five_minute: f64,
    /// Fifteen-minute average.
    pub fifteen_minute: f64,
}

/// Occupancy of one memory pool — physical memory or swap — in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryUsage {
    /// Total capacity.
    pub total_bytes: u64,
    /// Capacity a new allocation could still use.
    pub available_bytes: u64,
    /// `total_bytes - available_bytes`, carried rather than derived so a projection cannot
    /// accidentally subtract the wrong pair.
    pub used_bytes: u64,
}

/// Capacity and usage of one mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountUsage {
    /// Where the filesystem is mounted. A bounded identity, not a measurement.
    pub mount_point: String,
    /// The filesystem type as the substrate names it (`ext4`, `xfs`, `overlay`). Bounded identity.
    pub filesystem: String,
    /// Total capacity.
    pub total_bytes: u64,
    /// Capacity available to an unprivileged writer.
    pub available_bytes: u64,
    /// Capacity in use, including reserved blocks an unprivileged writer cannot reach.
    pub used_bytes: u64,
}

/// Every mounted filesystem a substrate reports capacity for, and how many it left out.
///
/// The count exists because [`MAX_MOUNTS`] is a real cap on a real machine: a container host or a
/// build agent routinely mounts more filesystems than a bounded reading can carry. Dropping the
/// excess and saying nothing produces an answer indistinguishable from a machine that genuinely has
/// thirty-two — the same class of lie as reporting zero for an instrument that does not exist, and
/// the one the `Unavailable`/zero distinction exists to refuse one level up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskUsage {
    /// The mounts this reading carries, at most [`MAX_MOUNTS`] of them.
    pub mounts: Vec<MountUsage>,
    /// How many mounted filesystems this reading does **not** carry: those the cap dropped, and
    /// those the substrate listed but could not measure at sample time. Counted per *filesystem*,
    /// so a mount point listed several times is one.
    ///
    /// Non-zero is load-bearing — it means the list is short by that many. Zero means whatever
    /// built this reading left nothing out, which is not quite the same as "the list is complete":
    /// a reading decoded from a peer too old to report the field also arrives as zero, because a
    /// substrate that cannot count its omissions cannot tell you about them either. Filesystems
    /// that are not disk capacity at all (`proc`, `tmpfs`, a network mount) are outside the family
    /// and are never counted here; they were not candidates for the list.
    pub omitted_mounts: u32,
}

/// One temperature instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct TemperatureSensor {
    /// `<chip>/<sensor>` — a bounded identity naming which instrument answered.
    pub label: String,
    /// Degrees Celsius.
    pub celsius: f64,
}

/// One fan tachometer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanSensor {
    /// `<chip>/<sensor>` — a bounded identity naming which instrument answered.
    pub label: String,
    /// Revolutions per minute. Zero is a real measurement here: a fan that is not spinning.
    pub rpm: u32,
}

/// One typed, unit-bearing measurement.
///
/// Closed, and deliberately free of a string-valued variant: a metric a consumer cannot interpret
/// is worse than a metric that is absent, because it looks like data.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricReading {
    /// See [`CpuUsage`].
    CpuUsage(CpuUsage),
    /// See [`LoadAverage`].
    LoadAverage(LoadAverage),
    /// Physical memory, see [`MemoryUsage`].
    Memory(MemoryUsage),
    /// Swap, see [`MemoryUsage`].
    Swap(MemoryUsage),
    /// Per-mount capacity, at most [`MAX_MOUNTS`] entries, plus what the cap left out.
    Disk(DiskUsage),
    /// How long the substrate has been up.
    Uptime(Duration),
    /// Temperature instruments, at most [`MAX_SENSORS`] entries.
    Temperature(Vec<TemperatureSensor>),
    /// Fan instruments, at most [`MAX_SENSORS`] entries.
    FanSpeed(Vec<FanSensor>),
}

impl MetricReading {
    /// The kind this reading answers.
    pub fn kind(&self) -> MetricKind {
        match self {
            MetricReading::CpuUsage(_) => MetricKind::CpuUsage,
            MetricReading::LoadAverage(_) => MetricKind::LoadAverage,
            MetricReading::Memory(_) => MetricKind::Memory,
            MetricReading::Swap(_) => MetricKind::Swap,
            MetricReading::Disk(_) => MetricKind::Disk,
            MetricReading::Uptime(_) => MetricKind::Uptime,
            MetricReading::Temperature(_) => MetricKind::Temperature,
            MetricReading::FanSpeed(_) => MetricKind::FanSpeed,
        }
    }
}

/// A reading and the instant it was taken.
///
/// The timestamp is part of the reading rather than of the transport: a remote host's reading is a
/// report of *its* sample time, and a consumer that renders it as "now" would be wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSnapshot {
    /// When the substrate took this measurement.
    pub sampled_at: SystemTime,
    /// The measurement.
    pub reading: MetricReading,
    /// Whether some *other* process measured this and reported it here (C-654).
    ///
    /// The same provenance bit [`SubstrateIdentity`](crate::port::SubstrateIdentity) carries, and
    /// it means the same thing: nothing in this reading was observed locally. A locally-read
    /// number is evidence; a remotely-reported one is a claim from a substrate that may be
    /// measuring a different machine, a different kernel, or nothing at all. Collapsing the two
    /// would let an operator act on "this host is at 95 % memory" without knowing which host.
    ///
    /// Stamped by the backend that crossed the boundary, never by the reader: the native reader
    /// records `false` because it just read `/proc`, and a delegating backend rewrites it to `true`
    /// on the way out regardless of what the far side claimed.
    pub remotely_reported: bool,
}

impl MetricSnapshot {
    /// The kind this snapshot answers.
    pub fn kind(&self) -> MetricKind {
        self.reading.kind()
    }
}

/// Why a substrate that *serves* a metric family has no reading for it.
///
/// Typed rather than a free-form message for the same reason the host probe taxonomy is: an
/// operator surface has to render the difference, and a projection has to be able to branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnavailable {
    /// The substrate has no instrument of this kind — no hwmon fan tachometer, no swap area.
    NoInstrument,
    /// This platform has no reader for the kind at all.
    UnsupportedPlatform,
    /// The instrument exists and answering it failed at sample time.
    ReadFailed,
}

impl MetricUnavailable {
    /// The stable operator- and wire-facing token for this reason.
    ///
    /// Lives beside [`MetricKind::as_str`] and for the same reason: a wire frame, a CLI's JSON and
    /// a monitoring projection all have to agree on the spelling, and three copies of a match arm
    /// would eventually disagree about one of them.
    pub const fn as_str(self) -> &'static str {
        match self {
            MetricUnavailable::NoInstrument => "no_instrument",
            MetricUnavailable::UnsupportedPlatform => "unsupported_platform",
            MetricUnavailable::ReadFailed => "read_failed",
        }
    }

    /// The reason a token names, or `None` where nothing does.
    pub fn from_token(token: &str) -> Option<MetricUnavailable> {
        match token {
            "no_instrument" => Some(MetricUnavailable::NoInstrument),
            "unsupported_platform" => Some(MetricUnavailable::UnsupportedPlatform),
            "read_failed" => Some(MetricUnavailable::ReadFailed),
            _ => None,
        }
    }

    /// One sentence an operator can act on, completing "…, so nothing was measured".
    pub const fn explain(self) -> &'static str {
        match self {
            MetricUnavailable::NoInstrument => "this substrate has no such instrument",
            MetricUnavailable::UnsupportedPlatform => "this platform has no reader for the kind",
            MetricUnavailable::ReadFailed => "the instrument failed at sample time",
        }
    }
}

/// What a substrate answers for one requested metric kind.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricAnswer {
    /// The substrate measured it.
    Served(MetricSnapshot),
    /// The substrate serves this family but has nothing to measure — explicitly unavailable,
    /// never a zero reading.
    Unavailable {
        /// The kind that was asked for.
        kind: MetricKind,
        /// Why nothing was measured.
        reason: MetricUnavailable,
    },
}

impl MetricAnswer {
    /// The kind this answer is about, served or not.
    pub fn kind(&self) -> MetricKind {
        match self {
            MetricAnswer::Served(snapshot) => snapshot.kind(),
            MetricAnswer::Unavailable { kind, .. } => *kind,
        }
    }

    /// The snapshot, if one was measured.
    pub fn served(&self) -> Option<&MetricSnapshot> {
        match self {
            MetricAnswer::Served(snapshot) => Some(snapshot),
            MetricAnswer::Unavailable { .. } => None,
        }
    }

    /// Why nothing was measured, if nothing was.
    pub fn unavailable(&self) -> Option<MetricUnavailable> {
        match self {
            MetricAnswer::Served(_) => None,
            MetricAnswer::Unavailable { reason, .. } => Some(*reason),
        }
    }

    /// The explicitly-unavailable answer for `kind`.
    pub fn unavailable_for(kind: MetricKind, reason: MetricUnavailable) -> Self {
        MetricAnswer::Unavailable { kind, reason }
    }
}

// ---------------------------------------------------------------------------
// Where the native reader reads
// ---------------------------------------------------------------------------

/// The procfs and sysfs roots the native reader consults — held as a **value** so a test pins
/// fixtures on the system under test instead of depending on what the developer's machine exposes.
///
/// Nothing here is read from the process environment: these are kernel mount points, not
/// configuration, and inventing an override variable would add a public configuration surface for
/// a value no operator sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetricsRoots {
    proc_root: PathBuf,
    sys_root: PathBuf,
}

impl MetricsRoots {
    /// The running machine's roots — `/proc` and `/sys`. What every production entry point uses.
    pub fn native() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            sys_root: PathBuf::from("/sys"),
        }
    }

    /// Pin both roots explicitly. For tests, and for a substrate whose kernel interfaces are
    /// mounted somewhere else.
    pub fn pinned(proc_root: impl Into<PathBuf>, sys_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            sys_root: sys_root.into(),
        }
    }

    /// The procfs root.
    pub fn proc_root(&self) -> &Path {
        &self.proc_root
    }

    /// The sysfs root.
    pub fn sys_root(&self) -> &Path {
        &self.sys_root
    }
}

impl Default for MetricsRoots {
    fn default() -> Self {
        Self::native()
    }
}

// ---------------------------------------------------------------------------
// The native reader
// ---------------------------------------------------------------------------

/// The kinds the native backend can attempt on this platform. Empty off Linux, which is what makes
/// [`crate::port::GuardedMetrics`] fail closed there rather than approximate.
pub(crate) fn native_served_kinds() -> Vec<MetricKind> {
    #[cfg(target_os = "linux")]
    {
        MetricKind::ALL.to_vec()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// Read one metric kind from `roots`.
pub(crate) async fn read_native(roots: &MetricsRoots, kind: MetricKind) -> Result<MetricAnswer> {
    #[cfg(target_os = "linux")]
    {
        linux::read(roots, kind).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        // The same denial `port.rs` produces, classified structurally through
        // `GuardedIoFailure::Unserved` rather than by its text.
        let _ = (roots, kind);
        Err(Error::GuardedIo(GuardedIoError::new(
            GuardedIoFailure::Unserved,
            "read a host metric",
        )))
    }
}

/// Re-apply every bound in this module to a reading that was **built somewhere else** — decoded
/// from a remote host's report, mapped out of a Kubernetes node listing, replayed from a fixture.
///
/// This exists because the caps are a *construction-site convention over public `String` and `Vec`
/// fields*, not a type invariant: nothing in [`MountUsage`] or [`TemperatureSensor`] prevents a
/// caller from writing a megabyte label, and a decoder that trusted its input would carry an
/// unbounded allocation straight through the seam while every local reader stayed honest. So a
/// backend that did not do the measuring re-bounds rather than re-derives, through this one
/// function — the same reason [`bounded_label`] is public.
///
/// Truncation never *refuses*. A reading is a measurement, not a message: rejecting the whole
/// snapshot because a machine has 40 mounts would turn a cosmetic excess into an outage, and the
/// caps are already the documented contract every consumer renders against. It is not silent
/// either — what the mount cap drops here is added to [`DiskUsage::omitted_mounts`], so a far side
/// that over-reports is still described accurately after the re-bounding.
pub fn bounded_reading(reading: MetricReading) -> MetricReading {
    match reading {
        MetricReading::Disk(mut disk) => {
            let dropped = disk.mounts.len().saturating_sub(MAX_MOUNTS);
            disk.omitted_mounts = disk
                .omitted_mounts
                .saturating_add(u32::try_from(dropped).unwrap_or(u32::MAX));
            disk.mounts.truncate(MAX_MOUNTS);
            for mount in &mut disk.mounts {
                mount.mount_point = bounded_mount_point(&mount.mount_point);
                mount.filesystem = bounded_label(&mount.filesystem);
            }
            MetricReading::Disk(disk)
        }
        MetricReading::Temperature(mut sensors) => {
            sensors.truncate(MAX_SENSORS);
            for sensor in &mut sensors {
                sensor.label = bounded_label(&sensor.label);
            }
            MetricReading::Temperature(sensors)
        }
        MetricReading::FanSpeed(mut sensors) => {
            sensors.truncate(MAX_SENSORS);
            for sensor in &mut sensors {
                sensor.label = bounded_label(&sensor.label);
            }
            MetricReading::FanSpeed(sensors)
        }
        // Every other reading is a fixed number of numbers, so there is nothing to bound: the
        // arms are spelled out rather than caught by `other =>` so a future variant carrying a
        // list has to come here and decide.
        reading @ (MetricReading::CpuUsage(_)
        | MetricReading::LoadAverage(_)
        | MetricReading::Memory(_)
        | MetricReading::Swap(_)
        | MetricReading::Uptime(_)) => reading,
    }
}

/// Reduce an instrument label to a bounded, control-character-free identity.
///
/// Public because [`MAX_LABEL_BYTES`] is part of the contract, not an implementation detail of the
/// native reader: a backend that builds readings from somewhere else — a remote host's report, a
/// Kubernetes node listing — has to honour the same bound, and it should do so through the same
/// code rather than by re-deriving it.
pub fn bounded_label(raw: &str) -> String {
    bounded_prefix(raw, MAX_LABEL_BYTES)
}

/// Reduce a mount point to a bounded identity that stays **distinct** from its siblings.
///
/// A mount point is a path, not a chip name, and paths agree for a long time before they differ:
/// `/var/lib/docker/overlay2/<64 hex>/merged` and the container next to it share their first ninety
/// bytes. Cutting both at [`MAX_LABEL_BYTES`] the way an instrument label is cut reports one
/// mount's capacity under another's name — two rows a consumer cannot tell apart, which is worse
/// than one row, because it still looks like data.
///
/// So a mount point that does not fit spends the tail of its budget on a digest of the *whole*
/// path instead of on more of a prefix its sibling also has. The digest is a disambiguator, not an
/// identifier: it says that two readings are about different filesystems, never what either path
/// was. The result is at most [`MAX_LABEL_BYTES`] and is idempotent, so re-bounding a reading that
/// already crossed this seam leaves it alone.
pub fn bounded_mount_point(raw: &str) -> String {
    // A label budget too small to hold the disambiguator would make the identity a bare digest,
    // which is bounded but tells an operator nothing. Fail at compile time rather than there.
    const { assert!(MAX_LABEL_BYTES > MOUNT_POINT_DIGEST_BYTES * 2) };
    // Measured rather than collected: `raw` may be a megabyte from a decoder, and nothing here may
    // allocate proportionally to it.
    let length = sanitised_len(raw);
    if length <= MAX_LABEL_BYTES {
        return bounded_prefix(raw, MAX_LABEL_BYTES);
    }
    let head = bounded_prefix(raw, MAX_LABEL_BYTES - MOUNT_POINT_DIGEST_BYTES);
    format!("{head}~{:016x}", path_digest(raw))
}

/// The bytes a truncated mount point spends on its disambiguator: `~` and sixteen hex digits.
const MOUNT_POINT_DIGEST_BYTES: usize = 17;

/// A label's characters with the noise removed: control characters that are an injection into an
/// operator's terminal rather than part of an identity, then any leading whitespace.
///
/// Order matters, and the reverse order is a bug this had (C-673 review): trimming *first* leaves
/// `"\u{7}  /mnt/data"` as `"  /mnt/data"`, because the control character was what stopped the trim
/// — and a second pass then returns something different, so the bounded identity is not a fixed
/// point. A far side's mount point crosses [`bounded_reading`] again on every hop, so an identity
/// that moves under re-bounding is an identity a consumer cannot store.
fn sanitised(raw: &str) -> impl Iterator<Item = char> + '_ {
    raw.chars()
        .filter(|c| !c.is_control())
        .skip_while(|c| c.is_whitespace())
}

/// The byte length [`sanitised`] would produce, trailing whitespace excluded, without building it.
fn sanitised_len(raw: &str) -> usize {
    let mut length = 0;
    let mut trailing = 0;
    for c in sanitised(raw) {
        if c.is_whitespace() {
            trailing += c.len_utf8();
        } else {
            length += trailing + c.len_utf8();
            trailing = 0;
        }
    }
    length
}

/// The longest run of [`sanitised`] characters fitting in `max` bytes, cut on a character boundary
/// rather than mid-codepoint and with trailing whitespace removed.
fn bounded_prefix(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(max);
    for c in sanitised(raw) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    // After the cut, not before: a value that lost its tail to the bound must not keep the space
    // that happened to sit on the boundary, or bounding it again would remove it.
    out.truncate(out.trim_end().len());
    out
}

/// FNV-1a over a whole sanitised path, for the disambiguator [`bounded_mount_point`] appends.
///
/// Spelled out rather than reached for. This is not a hash *table*, so `DefaultHasher` — whose
/// output is explicitly not stable between toolchains — would make the same mount read differently
/// after a compiler upgrade, and an operator comparing two readings could not tell that from two
/// different filesystems. It is not a security boundary either, so a cryptographic digest would be
/// a dependency bought for sixty-four bits of "these two differ".
fn path_digest(raw: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut digest = OFFSET;
    let mut buffer = [0u8; 4];
    for c in sanitised(raw) {
        for byte in c.encode_utf8(&mut buffer).as_bytes() {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
    }
    digest
}

#[cfg(target_os = "linux")]
mod linux {
    //! Reading the local machine: procfs text, sysfs hwmon files, and `statvfs`.
    //!
    //! Every failure here is an *answer* rather than an error: a kernel interface this machine does
    //! not expose is [`MetricUnavailable::NoInstrument`], and one that exists but would not parse is
    //! [`MetricUnavailable::ReadFailed`]. Reporting zero in either case would be a lie a projection
    //! could not distinguish from a genuinely idle machine.
    //!
    //! "Would not parse" includes *arithmetic* on what was parsed, which is the half C-673's review
    //! found still open. Every counter here is text a substrate handed this process, so every sum,
    //! product and scaling of one is checked: a value that does not fit answers `ReadFailed`. The
    //! two alternatives are both worse than an unavailable answer — with `overflow-checks` the read
    //! panics and takes the caller with it, and without them it wraps into a small number that
    //! looks exactly like a measurement.

    use super::*;
    use std::collections::BTreeMap;
    use std::io::Read;

    /// The window the CPU busy fraction is measured over. Short enough that a metrics read stays
    /// interactive, long enough that jiffie accounting has advanced on a busy machine.
    const CPU_SAMPLE_WINDOW: Duration = Duration::from_millis(100);

    /// Cap on a single procfs read. `mounts` is the only one that grows with the machine.
    const PROC_FILE_CAP: u64 = 1 << 20;

    /// Cap on a single sysfs read. These files hold one short scalar.
    const SYSFS_FILE_CAP: u64 = 4096;

    /// Filesystem types that are not disk capacity: kernel interfaces, RAM-backed pools, and
    /// network mounts. The network entries are excluded for a second reason — `statvfs` on a dead
    /// NFS or CIFS mount blocks, and a metrics read must not hang on one.
    const NON_DISK_FILESYSTEMS: &[&str] = &[
        "9p",
        "autofs",
        "bpf",
        "binfmt_misc",
        "ceph",
        "cgroup",
        "cgroup2",
        "cifs",
        "configfs",
        "davfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "efivarfs",
        "fuse",
        "fusectl",
        "glusterfs",
        "hugetlbfs",
        "mqueue",
        "nfs",
        "nfs4",
        "nsfs",
        "proc",
        "pstore",
        "ramfs",
        "rpc_pipefs",
        "securityfs",
        "smbfs",
        "sysfs",
        "tracefs",
        "tmpfs",
    ];

    /// Whether a mount is outside the disk-capacity family.
    ///
    /// `fuse.*` is a *family*, not a name. The list used to hold `fuse.sshfs` alone, which named
    /// one member of an open set: `fuse.rclone`, `fuse.s3fs` and whatever a machine mounts next
    /// week all answer `statvfs` through a userspace process that can simply stop answering — the
    /// same hazard as the network filesystems beside them, and the reason those are excluded is
    /// that a metrics read must not hang, not that the numbers would be wrong.
    ///
    /// `fuseblk` is deliberately absent, and not because it is safe — ntfs-3g serves it through
    /// exactly such a userspace process, and a disconnected USB NTFS volume is the classic wedge.
    /// It stays because it is the one FUSE type that is real local disk capacity an operator asked
    /// about, so excluding it would drop a disk to avoid a hang. That is a trade, not an
    /// exemption: if a metrics read is ever observed hanging on `fuseblk`, the fix is to move it
    /// into the list and lose the reading, not to add a timeout to a synchronous syscall.
    fn is_non_disk_filesystem(filesystem: &str) -> bool {
        filesystem.starts_with("fuse.") || NON_DISK_FILESYSTEMS.contains(&filesystem)
    }

    /// One metric kind, read from `roots`.
    pub(super) async fn read(roots: &MetricsRoots, kind: MetricKind) -> Result<MetricAnswer> {
        let reading = match kind {
            MetricKind::CpuUsage => read_cpu(roots).await,
            MetricKind::LoadAverage => read_load(roots),
            MetricKind::Memory => read_memory(roots),
            MetricKind::Swap => read_swap(roots),
            MetricKind::Disk => read_disk(roots),
            MetricKind::Uptime => read_uptime(roots),
            MetricKind::Temperature => read_temperature(roots),
            MetricKind::FanSpeed => read_fan(roots),
        };
        Ok(match reading {
            Ok(reading) => MetricAnswer::Served(MetricSnapshot {
                sampled_at: SystemTime::now(),
                reading,
                // This process just read this machine's own `/proc` and `/sys`. Nothing was
                // reported to it, so nothing is remotely reported.
                remotely_reported: false,
            }),
            Err(reason) => MetricAnswer::unavailable_for(kind, reason),
        })
    }

    /// At most `max` bytes of `path`, or `None` if it cannot be opened or decoded. procfs and
    /// sysfs files report a size of zero, so the cap is applied to the read rather than to a stat.
    fn read_capped(path: &Path, max: u64) -> Option<String> {
        let file = std::fs::File::open(path).ok()?;
        let mut text = String::new();
        file.take(max).read_to_string(&mut text).ok()?;
        Some(text)
    }

    fn read_proc(
        roots: &MetricsRoots,
        name: &str,
    ) -> std::result::Result<String, MetricUnavailable> {
        read_capped(&roots.proc_root().join(name), PROC_FILE_CAP)
            .ok_or(MetricUnavailable::NoInstrument)
    }

    // -- cpu ------------------------------------------------------------------------------------

    /// Aggregate processor accounting, in kernel jiffies.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct CpuTimes {
        pub(super) logical_cores: u32,
        /// Every accounted state summed.
        pub(super) total: u64,
        /// `idle + iowait` — a processor waiting on IO is not executing.
        pub(super) idle: u64,
    }

    /// The aggregate `cpu` line of a procfs `stat`, plus the number of `cpuN` lines beside it.
    pub(super) fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
        let mut aggregate = None;
        let mut logical_cores = 0u32;
        for line in stat.lines() {
            let mut fields = line.split_whitespace();
            let Some(name) = fields.next() else {
                continue;
            };
            if name == "cpu" {
                // user nice system idle iowait irq softirq steal — guest time is already counted
                // inside user/nice, so summing past `steal` would double-count it.
                let values: Vec<u64> = fields
                    .take(8)
                    .filter_map(|field| field.parse().ok())
                    .collect();
                if values.len() < 5 {
                    return None;
                }
                // Checked, because these are counters a substrate hands us rather than numbers
                // this process computed: `cpu 18446744073709551615 18446744073709551615 …`
                // overflows the sum. With `overflow-checks` on that aborts the caller, and
                // without them it wraps — which is worse, because a wrapped total makes
                // `busy_ratio` return a confident `0.0` that reads as an idle machine. A counter
                // that does not fit is an unreadable instrument, and this module answers those.
                let total = values
                    .iter()
                    .try_fold(0u64, |sum, value| sum.checked_add(*value))?;
                aggregate = Some((total, values[3].checked_add(values[4])?));
            } else if let Some(index) = name.strip_prefix("cpu") {
                if !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) {
                    logical_cores = logical_cores.saturating_add(1);
                }
            }
        }
        let (total, idle) = aggregate?;
        Some(CpuTimes {
            logical_cores: logical_cores.max(1),
            total,
            idle,
        })
    }

    /// The busy fraction between two samples. A window in which nothing was accounted is
    /// truthfully idle rather than an error — the machine really did record no processor time.
    pub(super) fn busy_ratio(first: &CpuTimes, second: &CpuTimes) -> f64 {
        let total = second.total.saturating_sub(first.total);
        if total == 0 {
            return 0.0;
        }
        let idle = second.idle.saturating_sub(first.idle);
        (total.saturating_sub(idle) as f64 / total as f64).clamp(0.0, 1.0)
    }

    async fn read_cpu(
        roots: &MetricsRoots,
    ) -> std::result::Result<MetricReading, MetricUnavailable> {
        let first =
            parse_cpu_times(&read_proc(roots, "stat")?).ok_or(MetricUnavailable::ReadFailed)?;
        let started = std::time::Instant::now();
        tokio::time::sleep(CPU_SAMPLE_WINDOW).await;
        let second =
            parse_cpu_times(&read_proc(roots, "stat")?).ok_or(MetricUnavailable::ReadFailed)?;
        Ok(MetricReading::CpuUsage(CpuUsage {
            logical_cores: second.logical_cores,
            busy_ratio: busy_ratio(&first, &second),
            window: started.elapsed(),
        }))
    }

    // -- load and uptime ------------------------------------------------------------------------

    fn read_load(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "loadavg")?;
        let mut fields = text.split_whitespace();
        let mut next = || {
            fields
                .next()
                .and_then(|field| field.parse::<f64>().ok())
                .ok_or(MetricUnavailable::ReadFailed)
        };
        Ok(MetricReading::LoadAverage(LoadAverage {
            one_minute: next()?,
            five_minute: next()?,
            fifteen_minute: next()?,
        }))
    }

    fn read_uptime(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "uptime")?;
        let seconds: f64 = text
            .split_whitespace()
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or(MetricUnavailable::ReadFailed)?;
        // `Duration::from_secs_f64` *panics* on a value it cannot represent, and this number came
        // out of a file this process was handed rather than one it computed — reachable through
        // `MetricsRoots::pinned`, which the docs offer to any substrate whose kernel interfaces are
        // mounted elsewhere. A finite `1e300` is as much an unreadable instrument as `NaN` is, and
        // this module answers unreadable instruments; it does not abort the caller. The negative,
        // infinite and NaN cases fall out of the same conversion, so one call decides all four.
        Duration::try_from_secs_f64(seconds)
            .map(MetricReading::Uptime)
            .map_err(|_| MetricUnavailable::ReadFailed)
    }

    // -- memory and swap ------------------------------------------------------------------------

    /// `meminfo` reports kibibytes; every reading is in bytes.
    fn parse_meminfo(text: &str) -> BTreeMap<&str, u64> {
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            // Scaled with a *checked* multiply: a kibibyte count that does not survive the
            // conversion is not a pool this reader can describe, and saturating it would report a
            // fabricated sixteen exbibytes a consumer could not tell from a real measurement. The
            // field is dropped instead, which the callers already read as `ReadFailed`.
            if let Some(bytes) = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .and_then(|value| value.checked_mul(1024))
            {
                fields.insert(key, bytes);
            }
        }
        fields
    }

    /// `total`/`available` into a pool reading, with `used` carried rather than left to a consumer.
    fn pool(total: u64, available: u64) -> MemoryUsage {
        let available = available.min(total);
        MemoryUsage {
            total_bytes: total,
            available_bytes: available,
            used_bytes: total.saturating_sub(available),
        }
    }

    fn read_memory(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "meminfo")?;
        let fields = parse_meminfo(&text);
        let total = *fields
            .get("MemTotal")
            .ok_or(MetricUnavailable::ReadFailed)?;
        if total == 0 {
            return Err(MetricUnavailable::ReadFailed);
        }
        // `MemAvailable` is the kernel's own estimate and the right number; `MemFree` is the
        // fallback for a kernel too old to publish it.
        let available = fields
            .get("MemAvailable")
            .or_else(|| fields.get("MemFree"))
            .copied()
            .ok_or(MetricUnavailable::ReadFailed)?;
        Ok(MetricReading::Memory(pool(total, available)))
    }

    fn read_swap(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "meminfo")?;
        let fields = parse_meminfo(&text);
        let total = *fields
            .get("SwapTotal")
            .ok_or(MetricUnavailable::ReadFailed)?;
        // No swap area is no *instrument*: `0 / 0` bytes would read as a measured, empty pool.
        if total == 0 {
            return Err(MetricUnavailable::NoInstrument);
        }
        let free = *fields
            .get("SwapFree")
            .ok_or(MetricUnavailable::ReadFailed)?;
        Ok(MetricReading::Swap(pool(total, free)))
    }

    // -- disk -----------------------------------------------------------------------------------

    /// Undo the octal escapes a mount table uses for whitespace and backslashes.
    fn unescape_mount_field(field: &str) -> String {
        let mut out = String::with_capacity(field.len());
        let mut rest = field;
        while let Some(index) = rest.find('\\') {
            out.push_str(&rest[..index]);
            let escape = rest.get(index + 1..index + 4);
            match escape.and_then(|digits| u8::from_str_radix(digits, 8).ok()) {
                Some(byte) => {
                    out.push(byte as char);
                    rest = &rest[index + 4..];
                }
                None => {
                    out.push('\\');
                    rest = &rest[index + 1..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// `(mount point, filesystem type)` for every mount that is real disk capacity, and how many
    /// the cap left out.
    ///
    /// The reading's cap is applied to the map as it is built rather than to a finished `Vec`:
    /// sorting a hostile mount table before truncating it would already have paid for every entry.
    /// What the count needs, though, is the number of *filesystems* left out, and that cannot be
    /// derived from evictions — a mount point past the cap listed three times evicts three times
    /// and is one mount (C-673 review). So the distinct points are tracked alongside, bounded by
    /// `PROC_FILE_CAP`: `seen` holds a subset of bytes already in `text`, which `read_capped`
    /// bounded before this function was called.
    ///
    /// Keyed by mount point, last entry winning, because that is what the kernel means: a mount
    /// stacked over an earlier one shadows it, and `statvfs` on the path answers about the
    /// *visible* filesystem. Pairing those numbers with the shadowed entry's type would label a
    /// reading with a filesystem it does not describe.
    fn parse_mounts(text: &str) -> (Vec<(String, String)>, u32) {
        let mut mounts: BTreeMap<String, String> = BTreeMap::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(_device), Some(point), Some(filesystem)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if is_non_disk_filesystem(filesystem) {
                continue;
            }
            let point = unescape_mount_field(point);
            seen.insert(point.clone());
            mounts.insert(point, bounded_label(&unescape_mount_field(filesystem)));
            if mounts.len() > MAX_MOUNTS {
                mounts.pop_last();
            }
        }
        let omitted = seen.len().saturating_sub(mounts.len());
        (
            mounts.into_iter().collect(),
            u32::try_from(omitted).unwrap_or(u32::MAX),
        )
    }

    /// Widen a libc counter. The concrete widths differ by target, so this is a conversion rather
    /// than a cast that would be redundant on some of them and lossy on others.
    fn widen(value: impl TryInto<u64>) -> u64 {
        value.try_into().unwrap_or(0)
    }

    fn mount_usage(point: &str, filesystem: &str) -> Option<MountUsage> {
        let path = std::ffi::CString::new(point).ok()?;
        // SAFETY: `path` is a NUL-terminated C string that outlives the call, and `stat` is a
        // caller-owned `statvfs` the kernel fills in. Nothing is retained past the call.
        let stat = unsafe {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
                return None;
            }
            stat
        };
        let frsize = match widen(stat.f_frsize) {
            0 => widen(stat.f_bsize),
            size => size,
        };
        let blocks = widen(stat.f_blocks);
        if frsize == 0 || blocks == 0 {
            return None;
        }
        Some(MountUsage {
            // Bounded *after* the syscall: `point` is the real path `statvfs` was asked about, and
            // the bounded form is only the identity the answer carries.
            mount_point: bounded_mount_point(point),
            filesystem: filesystem.to_string(),
            // Checked rather than saturating, for the same reason the procfs counters are: block
            // counts and a fragment size come from the mounted filesystem, and a hostile one can
            // name a capacity that does not fit. Saturating would answer `u64::MAX` bytes as if it
            // had measured that; `None` makes this a mount the reading does not carry, which
            // `read_disk` counts.
            total_bytes: blocks.checked_mul(frsize)?,
            available_bytes: widen(stat.f_bavail).checked_mul(frsize)?,
            used_bytes: blocks
                .saturating_sub(widen(stat.f_bfree))
                .checked_mul(frsize)?,
        })
    }

    fn read_disk(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "mounts")?;
        let (listed, capped) = parse_mounts(&text);
        let mut mounts = Vec::with_capacity(listed.len());
        let mut omitted_mounts = capped;
        for (point, filesystem) in listed {
            match mount_usage(&point, &filesystem) {
                Some(usage) => mounts.push(usage),
                // A filesystem the kernel lists but `statvfs` will not answer for is still a mount
                // this reading does not carry, and the count is the only place that can say so.
                None => omitted_mounts = omitted_mounts.saturating_add(1),
            }
        }
        if mounts.is_empty() {
            return Err(MetricUnavailable::NoInstrument);
        }
        Ok(MetricReading::Disk(DiskUsage {
            mounts,
            omitted_mounts,
        }))
    }

    // -- hwmon ----------------------------------------------------------------------------------

    /// `(index, chip label prefix, value text)` for every `<prefix><N>_input` in one hwmon chip,
    /// ordered by index so the reported list is stable across reads.
    ///
    /// Bounded to the [`MAX_SENSORS`] lowest indices **as it is collected**. The callers already
    /// stop pushing at the cap, but that is a bound on the answer, not on this walk: a sysfs tree
    /// with a hundred thousand `temp<N>_input` files would build a hundred thousand entries and a
    /// hundred thousand labels before any of them were discarded. Keeping the lowest indices is
    /// what the sorted-then-capped listing reported anyway, so the bound is not a reordering.
    pub(super) fn chip_inputs(chip: &Path, prefix: &str) -> Vec<(u32, String, String)> {
        let Ok(entries) = std::fs::read_dir(chip) else {
            return Vec::new();
        };
        let chip_name = read_capped(&chip.join("name"), SYSFS_FILE_CAP)
            .map(|name| bounded_label(&name))
            .filter(|name| !name.is_empty())
            .or_else(|| {
                chip.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "hwmon".to_string());

        let mut inputs: BTreeMap<u32, (String, String)> = BTreeMap::new();
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Some(index) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix("_input"))
                .and_then(|index| index.parse::<u32>().ok())
            else {
                continue;
            };
            // An index above everything already held cannot earn a slot in a full listing, and
            // finding that out afterwards would cost two sysfs reads per hostile file.
            if inputs.len() == MAX_SENSORS
                && inputs
                    .last_key_value()
                    .is_some_and(|(last, _)| *last < index)
            {
                continue;
            }
            let Some(value) = read_capped(&entry.path(), SYSFS_FILE_CAP) else {
                continue;
            };
            let label = read_capped(&chip.join(format!("{prefix}{index}_label")), SYSFS_FILE_CAP)
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| format!("{prefix}{index}"));
            inputs.insert(
                index,
                (bounded_label(&format!("{chip_name}/{label}")), value),
            );
            if inputs.len() > MAX_SENSORS {
                inputs.pop_last();
            }
        }
        inputs
            .into_iter()
            .map(|(index, (label, value))| (index, label, value))
            .collect()
    }

    /// Every hwmon chip directory, in a stable order and bounded as the directory is walked.
    ///
    /// [`MAX_SENSORS`] chips, because a listing is an allocation and `class/hwmon` is a directory
    /// a substrate controls. The trade is explicit: a machine exposing more than sixty-four hwmon
    /// chips has its later ones ignored in path order, where the alternative is a walk whose cost
    /// is whatever that directory holds. Real machines expose a handful.
    pub(super) fn chips(roots: &MetricsRoots) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(roots.sys_root().join("class/hwmon")) else {
            return Vec::new();
        };
        let mut chips: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for entry in entries.flatten() {
            chips.insert(entry.path());
            if chips.len() > MAX_SENSORS {
                chips.pop_last();
            }
        }
        chips.into_iter().collect()
    }

    fn read_temperature(
        roots: &MetricsRoots,
    ) -> std::result::Result<MetricReading, MetricUnavailable> {
        let mut sensors = Vec::new();
        for chip in chips(roots) {
            // Stop walking chips once the answer is full, rather than listing each remaining one
            // and discarding it.
            if sensors.len() == MAX_SENSORS {
                break;
            }
            for (_, label, value) in chip_inputs(&chip, "temp") {
                if sensors.len() == MAX_SENSORS {
                    break;
                }
                // hwmon reports millidegrees Celsius.
                let Ok(millidegrees) = value.trim().parse::<i64>() else {
                    continue;
                };
                sensors.push(TemperatureSensor {
                    label,
                    celsius: millidegrees as f64 / 1000.0,
                });
            }
        }
        if sensors.is_empty() {
            return Err(MetricUnavailable::NoInstrument);
        }
        Ok(MetricReading::Temperature(sensors))
    }

    fn read_fan(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let mut sensors = Vec::new();
        for chip in chips(roots) {
            if sensors.len() == MAX_SENSORS {
                break;
            }
            for (_, label, value) in chip_inputs(&chip, "fan") {
                if sensors.len() == MAX_SENSORS {
                    break;
                }
                let Ok(rpm) = value.trim().parse::<u32>() else {
                    continue;
                };
                sensors.push(FanSensor { label, rpm });
            }
        }
        if sensors.is_empty() {
            return Err(MetricUnavailable::NoInstrument);
        }
        Ok(MetricReading::FanSpeed(sensors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::port::GuardedMetrics;
    #[cfg(target_os = "linux")]
    use crate::{sandbox, System, Workspace};
    #[cfg(target_os = "linux")]
    use std::path::Path;

    #[cfg(target_os = "linux")]
    use super::linux::{busy_ratio, parse_cpu_times};

    /// Two logical CPUs, 800 idle + 20 iowait jiffies against 160 busy ones.
    const STAT: &str = "\
cpu  100 0 50 800 20 0 10 0 0 0
cpu0 50 0 25 400 10 0 5 0 0 0
cpu1 50 0 25 400 10 0 5 0 0 0
intr 12345
ctxt 6789
";

    #[cfg(target_os = "linux")]
    const MEMINFO: &str = "\
MemTotal:       16384000 kB
MemFree:         4096000 kB
MemAvailable:    8192000 kB
SwapTotal:       2048000 kB
SwapFree:        1024000 kB
";

    #[cfg(target_os = "linux")]
    const LOADAVG: &str = "0.75 0.50 0.25 2/512 9001\n";

    #[cfg(target_os = "linux")]
    const UPTIME: &str = "3600.50 7200.00\n";

    /// A procfs fixture. `mounts` names the fixture root itself, so the real `statvfs` behind the
    /// disk reading answers about a filesystem that actually exists — the pseudo entries beside it
    /// are the ones the reader must drop.
    #[cfg(target_os = "linux")]
    fn write_procfs(root: &Path, meminfo: &str) -> PathBuf {
        let proc = root.join("proc");
        std::fs::create_dir_all(&proc).unwrap();
        std::fs::write(proc.join("stat"), STAT).unwrap();
        std::fs::write(proc.join("meminfo"), meminfo).unwrap();
        std::fs::write(proc.join("loadavg"), LOADAVG).unwrap();
        std::fs::write(proc.join("uptime"), UPTIME).unwrap();
        std::fs::write(
            proc.join("mounts"),
            format!(
                "/dev/fixture {} ext4 rw,relatime 0 0\n\
                 proc /proc proc rw,nosuid 0 0\n\
                 tmpfs /run tmpfs rw,nosuid 0 0\n",
                root.display()
            ),
        )
        .unwrap();
        proc
    }

    /// One hwmon chip exposing a temperature and a fan tachometer.
    #[cfg(target_os = "linux")]
    fn write_hwmon(root: &Path) {
        let chip = root.join("sys/class/hwmon/hwmon0");
        std::fs::create_dir_all(&chip).unwrap();
        std::fs::write(chip.join("name"), "coretemp\n").unwrap();
        std::fs::write(chip.join("temp1_input"), "42500\n").unwrap();
        std::fs::write(chip.join("temp1_label"), "Package id 0\n").unwrap();
        std::fs::write(chip.join("fan1_input"), "1200\n").unwrap();
    }

    /// A `System` whose metric roots are the fixture's, so both faces are reachable without
    /// depending on what the developer's machine happens to expose (the `WorktreeBase` idiom: pin
    /// the value on the system under test, never `std::env::set_var`).
    #[cfg(target_os = "linux")]
    fn fixture_system(root: &Path) -> System {
        System::new(Workspace::new(root).unwrap())
            .with_metrics_roots(MetricsRoots::pinned(root.join("proc"), root.join("sys")))
    }

    #[cfg(target_os = "linux")]
    fn served(answer: MetricAnswer) -> MetricSnapshot {
        match answer {
            MetricAnswer::Served(snapshot) => snapshot,
            MetricAnswer::Unavailable { kind, reason } => {
                panic!("{kind} must be served here, but answered unavailable ({reason:?})")
            }
        }
    }

    /// Acceptance 2, served face: cpu, memory, disk, load and uptime all answer on Linux, with
    /// typed unit-bearing values and a sampled-at timestamp inside the window of the call.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_native_system_serves_cpu_memory_disk_load_and_uptime() {
        let root = sandbox::fixture_dir("metrics-served");
        write_procfs(&root, MEMINFO);
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        let before = SystemTime::now();

        let cpu = served(port.read_metric(MetricKind::CpuUsage).await.unwrap());
        match &cpu.reading {
            MetricReading::CpuUsage(usage) => {
                assert_eq!(usage.logical_cores, 2, "two `cpuN` lines are two cores");
                assert!(
                    (0.0..=1.0).contains(&usage.busy_ratio),
                    "busy ratio out of range: {usage:?}"
                );
                assert!(usage.window > Duration::ZERO, "a window must be measured");
            }
            other => panic!("cpu answered {other:?}"),
        }

        let load = served(port.read_metric(MetricKind::LoadAverage).await.unwrap());
        assert_eq!(
            load.reading,
            MetricReading::LoadAverage(LoadAverage {
                one_minute: 0.75,
                five_minute: 0.50,
                fifteen_minute: 0.25,
            })
        );

        let memory = served(port.read_metric(MetricKind::Memory).await.unwrap());
        assert_eq!(
            memory.reading,
            MetricReading::Memory(MemoryUsage {
                total_bytes: 16_384_000 * 1024,
                available_bytes: 8_192_000 * 1024,
                used_bytes: 8_192_000 * 1024,
            }),
            "meminfo is in kB and the reading is in bytes"
        );

        let swap = served(port.read_metric(MetricKind::Swap).await.unwrap());
        assert_eq!(
            swap.reading,
            MetricReading::Swap(MemoryUsage {
                total_bytes: 2_048_000 * 1024,
                available_bytes: 1_024_000 * 1024,
                used_bytes: 1_024_000 * 1024,
            })
        );

        let uptime = served(port.read_metric(MetricKind::Uptime).await.unwrap());
        assert_eq!(
            uptime.reading,
            MetricReading::Uptime(Duration::from_millis(3_600_500))
        );

        let disk = served(port.read_metric(MetricKind::Disk).await.unwrap());
        match &disk.reading {
            MetricReading::Disk(reading) => {
                let mounts = &reading.mounts;
                assert_eq!(
                    mounts.len(),
                    1,
                    "the pseudo filesystems beside the real one must be dropped: {mounts:?}"
                );
                assert_eq!(reading.omitted_mounts, 0, "nothing was left out here");
                assert_eq!(mounts[0].filesystem, "ext4");
                // Compared through the bound rather than against the raw path: a fixture root under
                // a long `TMPDIR` is itself longer than the bound, and asserting the raw path would
                // make this test pass or fail on the developer's temporary directory.
                assert_eq!(
                    mounts[0].mount_point,
                    bounded_mount_point(&root.display().to_string())
                );
                assert!(mounts[0].total_bytes > 0, "statvfs answered nothing");
                assert!(mounts[0].used_bytes <= mounts[0].total_bytes);
            }
            other => panic!("disk answered {other:?}"),
        }

        let after = SystemTime::now();
        for snapshot in [cpu, load, memory, swap, uptime, disk] {
            assert!(
                snapshot.sampled_at >= before && snapshot.sampled_at <= after,
                "{} carried a sampled-at outside the call: {snapshot:?}",
                snapshot.kind()
            );
            // C-654: this process read this machine. Anything claiming otherwise would let a
            // consumer attribute a local measurement to some other host.
            assert!(
                !snapshot.remotely_reported,
                "{} was read locally and must not claim remote provenance",
                snapshot.kind()
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 2, served face for the optional instruments: temperature and fan answer where
    /// hwmon exposes them, in degrees Celsius and RPM.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn hwmon_temperature_and_fan_serve_where_the_sensors_exist() {
        let root = sandbox::fixture_dir("metrics-hwmon");
        write_procfs(&root, MEMINFO);
        write_hwmon(&root);
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        let temperature = served(port.read_metric(MetricKind::Temperature).await.unwrap());
        assert_eq!(
            temperature.reading,
            MetricReading::Temperature(vec![TemperatureSensor {
                label: "coretemp/Package id 0".into(),
                celsius: 42.5,
            }]),
            "hwmon reports millidegrees; the reading is degrees Celsius"
        );

        let fan = served(port.read_metric(MetricKind::FanSpeed).await.unwrap());
        assert_eq!(
            fan.reading,
            MetricReading::FanSpeed(vec![FanSensor {
                label: "coretemp/fan1".into(),
                rpm: 1200,
            }])
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 2, explicitly-unavailable face: a machine with no hwmon tree answers
    /// `Unavailable`, never a served zero — the board statistics contract's `absent` convention.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_absent_hwmon_answers_explicitly_unavailable_never_zero() {
        let root = sandbox::fixture_dir("metrics-no-hwmon");
        write_procfs(&root, MEMINFO);
        std::fs::create_dir_all(root.join("sys/class")).unwrap();
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        for kind in [MetricKind::Temperature, MetricKind::FanSpeed] {
            let answer = port.read_metric(kind).await.unwrap();
            assert_eq!(
                answer,
                MetricAnswer::Unavailable {
                    kind,
                    reason: MetricUnavailable::NoInstrument,
                },
                "{kind} on a machine with no sensor must be explicitly unavailable"
            );
            assert!(
                answer.served().is_none(),
                "{kind} must not fabricate a zero reading"
            );
            assert_eq!(answer.unavailable(), Some(MetricUnavailable::NoInstrument));
        }

        // The kinds this machine *does* have are unaffected by the missing ones.
        assert!(port
            .read_metric(MetricKind::Uptime)
            .await
            .unwrap()
            .served()
            .is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    /// The same convention one level down: a machine with no swap area has no swap *instrument*,
    /// so it says so rather than reporting `0 / 0` bytes as if it had measured an empty one.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_machine_without_swap_answers_explicitly_unavailable() {
        let root = sandbox::fixture_dir("metrics-no-swap");
        write_procfs(
            &root,
            "MemTotal:       16384000 kB\n\
             MemFree:         4096000 kB\n\
             MemAvailable:    8192000 kB\n\
             SwapTotal:             0 kB\n\
             SwapFree:              0 kB\n",
        );
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        assert_eq!(
            port.read_metric(MetricKind::Swap).await.unwrap(),
            MetricAnswer::Unavailable {
                kind: MetricKind::Swap,
                reason: MetricUnavailable::NoInstrument,
            }
        );
        assert!(port
            .read_metric(MetricKind::Memory)
            .await
            .unwrap()
            .served()
            .is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 1: the defaults are `Unserved`. A substrate that has not opted in serves nothing
    /// — and that is a *different* answer from an instrument this substrate does not have.
    #[tokio::test]
    async fn a_substrate_that_serves_no_metrics_denies_every_kind_as_unserved() {
        struct Bare;
        impl crate::port::GuardedMetrics for Bare {}

        let port: &dyn crate::port::GuardedMetrics = &Bare;
        assert!(
            port.served_metric_kinds().is_empty(),
            "deny by default: a fresh substrate claims no metric kind"
        );

        for kind in MetricKind::ALL {
            let error = port
                .read_metric(kind)
                .await
                .expect_err("an unserved metric must fail closed, not report zero");
            assert!(
                error.to_string().starts_with(crate::port::UNSERVED),
                "{kind} denied with an off-contract message: {error}"
            );
        }

        let error = port
            .read_metrics()
            .await
            .expect_err("a whole-snapshot read must fail closed too");
        assert!(error.to_string().starts_with(crate::port::UNSERVED));
    }

    /// Acceptance 3: readings are bounded. Neither an unbounded sensor list, an unbounded mount
    /// table, nor an unbounded label can travel out of this seam.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn metric_readings_are_bounded_in_size() {
        let root = sandbox::fixture_dir("metrics-bounded");
        let mut mounts = String::new();
        for index in 0..(MAX_MOUNTS + 8) {
            let point = root.join(format!("mount{index}"));
            std::fs::create_dir_all(&point).unwrap();
            mounts.push_str(&format!(
                "/dev/fixture{index} {} ext4 rw 0 0\n",
                point.display()
            ));
        }
        let proc = write_procfs(&root, MEMINFO);
        std::fs::write(proc.join("mounts"), &mounts).unwrap();

        let chip = root.join("sys/class/hwmon/hwmon0");
        std::fs::create_dir_all(&chip).unwrap();
        std::fs::write(chip.join("name"), "x".repeat(500)).unwrap();
        for index in 1..=(MAX_SENSORS + 16) {
            std::fs::write(chip.join(format!("temp{index}_input")), "30000\n").unwrap();
            std::fs::write(chip.join(format!("temp{index}_label")), "y".repeat(500)).unwrap();
            std::fs::write(chip.join(format!("fan{index}_input")), "900\n").unwrap();
        }

        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => assert_eq!(disk.mounts.len(), MAX_MOUNTS),
            other => panic!("disk answered {other:?}"),
        }
        match served(port.read_metric(MetricKind::Temperature).await.unwrap()).reading {
            MetricReading::Temperature(sensors) => {
                assert_eq!(sensors.len(), MAX_SENSORS);
                for sensor in &sensors {
                    assert!(
                        sensor.label.len() <= MAX_LABEL_BYTES,
                        "unbounded label: {} bytes",
                        sensor.label.len()
                    );
                }
            }
            other => panic!("temperature answered {other:?}"),
        }
        match served(port.read_metric(MetricKind::FanSpeed).await.unwrap()).reading {
            MetricReading::FanSpeed(sensors) => {
                assert_eq!(sensors.len(), MAX_SENSORS);
                for sensor in &sensors {
                    assert!(sensor.label.len() <= MAX_LABEL_BYTES);
                }
            }
            other => panic!("fan answered {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-673, acceptance 1: `/proc/uptime` is a file a substrate hands us, and
    /// [`MetricsRoots::pinned`] is documented for a kernel whose interfaces are mounted elsewhere.
    /// A finite value too large for a `Duration` is therefore reachable input, and this module's
    /// own contract says every failure here is an *answer*.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_finite_but_oversized_uptime_answers_read_failed_rather_than_panicking() {
        for value in ["1e300", "18446744073709551616", "3.4e38"] {
            let root = sandbox::fixture_dir("metrics-uptime-oversized");
            let proc = write_procfs(&root, MEMINFO);
            std::fs::write(proc.join("uptime"), format!("{value} {value}\n")).unwrap();

            let system = fixture_system(&root);
            let port: &dyn GuardedMetrics = &system;
            assert_eq!(
                port.read_metric(MetricKind::Uptime).await.unwrap(),
                MetricAnswer::Unavailable {
                    kind: MetricKind::Uptime,
                    reason: MetricUnavailable::ReadFailed,
                },
                "`{value}` seconds of uptime must be an answer, never a panic"
            );

            std::fs::remove_dir_all(&root).ok();
        }
    }

    /// C-673 review: the panic class the uptime fix closed is a *class*, and `/proc/stat` is the
    /// other end of it. Kernel counters are text this process was handed, so summing them is
    /// `u64` arithmetic over attacker-shaped input: under `overflow-checks` (on by default in this
    /// workspace's dev and test profiles) it aborts the caller, and in release it wraps — which is
    /// worse, because `busy_ratio` then returns a fabricated `0.0` that reads as an idle machine.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn procfs_counters_that_overflow_answer_read_failed_rather_than_panicking() {
        // Verbatim from the review.
        const OVERFLOWING: &str = "cpu 18446744073709551615 18446744073709551615 1 2 3 0 0 0";
        assert!(
            parse_cpu_times(OVERFLOWING).is_none(),
            "a total that does not fit a u64 is an unreadable instrument, not a measurement"
        );

        let root = sandbox::fixture_dir("metrics-cpu-overflow");
        let proc = write_procfs(&root, MEMINFO);
        std::fs::write(
            proc.join("stat"),
            format!("{OVERFLOWING}\ncpu0 1 0 1 1 0 0 0 0 0 0\n"),
        )
        .unwrap();
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        assert_eq!(
            port.read_metric(MetricKind::CpuUsage).await.unwrap(),
            MetricAnswer::Unavailable {
                kind: MetricKind::CpuUsage,
                reason: MetricUnavailable::ReadFailed,
            }
        );
        std::fs::remove_dir_all(&root).ok();

        // `meminfo` is the same shape one file over: kibibytes scaled to bytes. A total that does
        // not survive the scaling must not saturate into a fabricated sixteen exbibytes.
        let root = sandbox::fixture_dir("metrics-meminfo-overflow");
        write_procfs(
            &root,
            "MemTotal:       18446744073709551615 kB\n\
             MemFree:         4096000 kB\n\
             MemAvailable:    8192000 kB\n\
             SwapTotal:       2048000 kB\n\
             SwapFree:        1024000 kB\n",
        );
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        assert_eq!(
            port.read_metric(MetricKind::Memory).await.unwrap(),
            MetricAnswer::Unavailable {
                kind: MetricKind::Memory,
                reason: MetricUnavailable::ReadFailed,
            }
        );
        std::fs::remove_dir_all(&root).ok();
    }

    /// C-673 review: `bounded_mount_point` documents itself idempotent, and a mount point from a
    /// hostile far side re-crosses `bounded_reading` on every hop. A control character in front of
    /// whitespace used to expose that whitespace only on the *second* pass, so the identity moved
    /// under a consumer that had already stored it.
    #[test]
    fn a_bounded_identity_is_the_fixed_point_of_bounding_it_again() {
        let hostile = [
            "\u{7}  /mnt/data",
            "  \u{1b}[2J/mnt/data  ",
            "\u{7}\u{7}\t /var/lib/docker/overlay2/x/merged\n",
            &format!("\u{7}  /mnt/{}", "z".repeat(4096)),
            &"é".repeat(500),
            "",
        ];
        for raw in hostile {
            let once = bounded_mount_point(raw);
            assert_eq!(
                bounded_mount_point(&once),
                once,
                "bounding {raw:?} again moved it: {once:?}"
            );
            assert!(once.len() <= MAX_LABEL_BYTES, "{once:?}");
            let label = bounded_label(raw);
            assert_eq!(
                bounded_label(&label),
                label,
                "label {raw:?} moved: {label:?}"
            );
        }
        assert_eq!(bounded_mount_point("\u{7}  /mnt/data"), "/mnt/data");
    }

    /// C-673 review: `omitted_mounts` counts *filesystems* the reading does not carry, not lines
    /// past the cap. An overmount sorting past the cap is one mount listed twice, and counting it
    /// twice reports a machine with more filesystems than it has.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_overmount_past_the_cap_is_one_omitted_mount_not_two() {
        let root = sandbox::fixture_dir("metrics-overmount");
        let proc = write_procfs(&root, MEMINFO);

        let mut table = String::new();
        for index in 0..MAX_MOUNTS {
            let point = root.join(format!("aa-mount{index:03}"));
            std::fs::create_dir_all(&point).unwrap();
            table.push_str(&format!("/dev/a{index} {} ext4 rw 0 0\n", point.display()));
        }
        // One mount point past the cap, listed three times the way a stack of overmounts is.
        let stacked = root.join("zz-stacked");
        std::fs::create_dir_all(&stacked).unwrap();
        for filesystem in ["ext4", "xfs", "btrfs"] {
            table.push_str(&format!(
                "/dev/stacked {} {filesystem} rw 0 0\n",
                stacked.display()
            ));
        }
        std::fs::write(proc.join("mounts"), &table).unwrap();

        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => {
                assert_eq!(disk.mounts.len(), MAX_MOUNTS);
                assert_eq!(
                    disk.omitted_mounts, 1,
                    "one mount point was left out, however many lines described it"
                );
            }
            other => panic!("disk answered {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-673, acceptance 2 (identity half): two sibling mounts whose paths agree for longer than
    /// the label bound must stay two readings.
    ///
    /// The shared parent is longer than [`MAX_LABEL_BYTES`] on its own, so the collision is a
    /// property of the fixture rather than of however long this machine's `TMPDIR` happens to be —
    /// the assertion below pins that premise before the reader is asked anything.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn long_mount_points_keep_distinct_identities_rather_than_colliding() {
        let root = sandbox::fixture_dir("metrics-mount-identity");
        let proc = write_procfs(&root, MEMINFO);

        // The shape of a container overlay: a shared parent, the part that tells the two apart,
        // and a common leaf.
        let shared = root.join(format!("overlay2-{}", "0".repeat(MAX_LABEL_BYTES)));
        let first = shared.join("a".repeat(64)).join("merged");
        let second = shared.join("b".repeat(64)).join("merged");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let first = first.display().to_string();
        let second = second.display().to_string();
        assert_eq!(
            bounded_label(&first),
            bounded_label(&second),
            "the fixture must really collide under the instrument-label bound, or this test \
             passes for the wrong reason"
        );

        std::fs::write(
            proc.join("mounts"),
            format!("/dev/first {first} ext4 rw 0 0\n/dev/second {second} ext4 rw 0 0\n"),
        )
        .unwrap();

        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => {
                let mounts = &disk.mounts;
                assert_eq!(mounts.len(), 2, "two mounts were listed: {mounts:?}");
                assert_eq!(disk.omitted_mounts, 0, "both mounts fit");
                assert_ne!(
                    mounts[0].mount_point, mounts[1].mount_point,
                    "two sibling mounts collapsed into one identity: {mounts:?}"
                );
                for mount in mounts {
                    assert!(
                        mount.mount_point.len() <= MAX_LABEL_BYTES,
                        "unbounded mount point: {mount:?}"
                    );
                }
                // Re-bounding an already-bounded identity must not move it: the same reading
                // crosses `bounded_reading` again on every remote hop.
                for mount in mounts {
                    assert_eq!(
                        bounded_mount_point(&mount.mount_point),
                        mount.mount_point,
                        "the bounded identity is not idempotent"
                    );
                }
            }
            other => panic!("disk answered {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-673, acceptance 2 (cap half): a machine with more mounts than [`MAX_MOUNTS`] must say so
    /// in the answer. Dropping the excess silently reads, to every consumer, as a machine that
    /// simply has thirty-two filesystems.
    ///
    /// Built from the same overlay-length fixture as the identity half and equally independent of
    /// `TMPDIR`: the count is a property of the mount table this test writes.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn exceeding_the_mount_cap_is_visible_in_the_answer() {
        let root = sandbox::fixture_dir("metrics-mount-cap");
        let proc = write_procfs(&root, MEMINFO);

        // `aa-` sorts before `zz-`, so the two overlay siblings survive the cap and the filler is
        // what gets dropped — which makes the omitted count exact rather than incidental.
        let shared = root.join(format!("aa-overlay2-{}", "0".repeat(MAX_LABEL_BYTES)));
        let mut table = String::new();
        for leaf in ["a", "b"] {
            let point = shared.join(leaf.repeat(64)).join("merged");
            std::fs::create_dir_all(&point).unwrap();
            table.push_str(&format!("/dev/{leaf} {} ext4 rw 0 0\n", point.display()));
        }
        const EXCESS: usize = 8;
        for index in 0..(MAX_MOUNTS + EXCESS) {
            let point = root.join(format!("zz-mount{index:03}"));
            std::fs::create_dir_all(&point).unwrap();
            table.push_str(&format!(
                "/dev/filler{index} {} ext4 rw 0 0\n",
                point.display()
            ));
        }
        std::fs::write(proc.join("mounts"), &table).unwrap();

        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => {
                assert_eq!(disk.mounts.len(), MAX_MOUNTS, "the cap still binds");
                assert_eq!(
                    disk.omitted_mounts,
                    (EXCESS + 2) as u32,
                    "the mounts the cap dropped must be countable from the answer alone"
                );
                assert_ne!(
                    disk.mounts[0].mount_point,
                    disk.mounts[1].mount_point,
                    "the two overlay siblings kept their identities: {:?}",
                    &disk.mounts[..2]
                );
            }
            other => panic!("disk answered {other:?}"),
        }

        // A machine whose mounts all fit says so with a zero, not with an absent field.
        let complete = sandbox::fixture_dir("metrics-mount-cap-complete");
        write_procfs(&complete, MEMINFO);
        let port: &dyn GuardedMetrics = &fixture_system(&complete);
        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => assert_eq!(disk.omitted_mounts, 0),
            other => panic!("disk answered {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&complete).ok();
    }

    /// C-673, acceptance 3: the hwmon walk bounds what it *collects*, not only what it answers.
    ///
    /// The answer-side cap in [`read_temperature`](linux::read_temperature) is applied after
    /// `chip_inputs` has already built a `Vec` per chip and `chips` a `Vec` of every chip
    /// directory, so a sysfs tree with a million entries is a million allocations before any bound
    /// bites. Asserted on the collections themselves because it is invisible in the answer: both
    /// orderings report the same first [`MAX_SENSORS`].
    #[cfg(target_os = "linux")]
    #[test]
    fn the_hwmon_walk_bounds_its_intermediate_listing_not_only_its_answer() {
        let root = sandbox::fixture_dir("metrics-hwmon-intermediate");
        let class = root.join("sys/class/hwmon");
        for chip in 0..(MAX_SENSORS + 24) {
            std::fs::create_dir_all(class.join(format!("hwmon{chip:03}"))).unwrap();
        }
        let chip = class.join("hwmon000");
        std::fs::write(chip.join("name"), "coretemp\n").unwrap();
        for index in 1..=(MAX_SENSORS + 24) {
            std::fs::write(chip.join(format!("temp{index}_input")), "30000\n").unwrap();
            std::fs::write(chip.join(format!("fan{index}_input")), "900\n").unwrap();
        }

        let roots = MetricsRoots::pinned(root.join("proc"), root.join("sys"));
        let listed = super::linux::chips(&roots);
        assert!(
            listed.len() <= MAX_SENSORS,
            "the chip listing is unbounded: {} entries",
            listed.len()
        );
        for prefix in ["temp", "fan"] {
            let inputs = super::linux::chip_inputs(&chip, prefix);
            assert!(
                inputs.len() <= MAX_SENSORS,
                "the `{prefix}` listing is unbounded: {} entries",
                inputs.len()
            );
            // Bounded by keeping the *lowest* indices, so the answer is the same list it always
            // was — the bound must not become a reordering.
            let indices: Vec<u32> = inputs.iter().map(|(index, _, _)| *index).collect();
            let mut sorted = indices.clone();
            sorted.sort_unstable();
            assert_eq!(indices, sorted, "the bounded listing lost its index order");
            assert_eq!(
                indices.first(),
                Some(&1),
                "the bound dropped the low indices"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-673, acceptance 3: a filesystem whose `statvfs` can block — every `fuse.*` driver, and the
    /// network filesystems the exclusion list did not name — must be dropped before the
    /// synchronous call. The fixture points each of them at a directory that really exists, so a
    /// reader that let one through would report it rather than fail.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn network_and_userspace_filesystems_never_reach_the_statvfs_guard() {
        let root = sandbox::fixture_dir("metrics-non-disk");
        let proc = write_procfs(&root, MEMINFO);
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();

        let mut mounts = format!("/dev/fixture {} ext4 rw 0 0\n", real.display());
        for filesystem in [
            "fuse.sshfs",
            "fuse.rclone",
            "fuse.s3fs",
            "9p",
            "ceph",
            "glusterfs",
            "davfs",
        ] {
            let point = root.join(format!("via-{filesystem}"));
            std::fs::create_dir_all(&point).unwrap();
            mounts.push_str(&format!("remote {} {filesystem} rw 0 0\n", point.display()));
        }
        std::fs::write(proc.join("mounts"), &mounts).unwrap();

        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;
        match served(port.read_metric(MetricKind::Disk).await.unwrap()).reading {
            MetricReading::Disk(disk) => {
                let reported: Vec<&str> = disk
                    .mounts
                    .iter()
                    .map(|mount| mount.filesystem.as_str())
                    .collect();
                assert_eq!(
                    reported,
                    vec!["ext4"],
                    "a filesystem whose `statvfs` can block reached it: {:?}",
                    disk.mounts
                );
                // Excluded filesystems are outside the disk family, not mounts left out of it.
                assert_eq!(disk.omitted_mounts, 0);
            }
            other => panic!("disk answered {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// Acceptance 3: the vocabulary is closed and every measurement is a number with a unit. A
    /// free-form string metric is the failure this pin exists to catch — labels naming *which
    /// instrument* answered are bounded identities and live on the sensor structs, not in the
    /// reading enum's payloads.
    #[test]
    fn the_reading_vocabulary_stays_closed_and_free_of_free_form_strings() {
        let source = include_str!("metrics.rs");
        let (_, rest) = source
            .split_once("pub enum MetricReading {")
            .expect("MetricReading is the reading vocabulary");
        let (body, _) = rest.split_once("\n}").expect("enum body");

        assert!(
            !body.contains("String"),
            "a free-form string metric entered the closed vocabulary:\n{body}"
        );
        assert!(
            !body.contains("Custom") && !body.contains("Other"),
            "the vocabulary must stay closed — no escape-hatch variant:\n{body}"
        );
        for kind in MetricKind::ALL {
            assert!(
                body.contains(&format!("{kind:?}(")),
                "{kind} has no reading variant"
            );
        }
    }

    /// Every kind round-trips between the vocabulary and the readings that answer it.
    #[test]
    fn every_kind_has_a_token_and_a_reading_that_reports_it() {
        let readings = [
            MetricReading::CpuUsage(CpuUsage {
                logical_cores: 1,
                busy_ratio: 0.5,
                window: Duration::from_millis(100),
            }),
            MetricReading::LoadAverage(LoadAverage {
                one_minute: 0.0,
                five_minute: 0.0,
                fifteen_minute: 0.0,
            }),
            MetricReading::Memory(MemoryUsage {
                total_bytes: 1,
                available_bytes: 1,
                used_bytes: 0,
            }),
            MetricReading::Swap(MemoryUsage {
                total_bytes: 1,
                available_bytes: 1,
                used_bytes: 0,
            }),
            MetricReading::Disk(DiskUsage::default()),
            MetricReading::Uptime(Duration::ZERO),
            MetricReading::Temperature(Vec::new()),
            MetricReading::FanSpeed(Vec::new()),
        ];
        let kinds: Vec<MetricKind> = readings.iter().map(MetricReading::kind).collect();
        assert_eq!(kinds, MetricKind::ALL.to_vec());

        let mut tokens: Vec<&str> = MetricKind::ALL.iter().map(|kind| kind.as_str()).collect();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), MetricKind::ALL.len(), "tokens must be unique");
    }

    /// C-654: every kind survives a round trip through the token a wire frame carries, and a token
    /// nothing names is refused rather than mapped onto a neighbour.
    #[test]
    fn every_kind_round_trips_through_its_wire_token() {
        for kind in MetricKind::ALL {
            assert_eq!(
                MetricKind::from_token(kind.as_str()),
                Some(kind),
                "{kind} does not survive its own token"
            );
        }
        for unknown in ["", "cpu ", "CPU", "gpu", "temperature_c"] {
            assert_eq!(
                MetricKind::from_token(unknown),
                None,
                "`{unknown}` is not in the closed vocabulary and must not resolve"
            );
        }
    }

    /// C-654: the caps are a construction-site convention over public fields, so a reading built
    /// **off this machine** — a decoded wire frame, a mapped node listing — has to be re-bounded
    /// rather than trusted. This is the seam a hostile far side would otherwise walk through.
    #[test]
    fn a_reading_built_elsewhere_is_rebounded_rather_than_trusted() {
        let long = "z".repeat(4096);

        let mounts: Vec<MountUsage> = (0..(MAX_MOUNTS + 40))
            .map(|index| MountUsage {
                mount_point: format!("/mnt/{long}{index}"),
                filesystem: long.clone(),
                total_bytes: 1,
                available_bytes: 1,
                used_bytes: 0,
            })
            .collect();
        match bounded_reading(MetricReading::Disk(DiskUsage {
            mounts,
            omitted_mounts: 0,
        })) {
            MetricReading::Disk(disk) => {
                assert_eq!(
                    disk.mounts.len(),
                    MAX_MOUNTS,
                    "the mount table must be re-capped"
                );
                // C-673: what the re-cap dropped is counted, not swallowed. A far side that
                // over-reports is still described accurately after re-bounding.
                assert_eq!(disk.omitted_mounts, 40);
                for mount in &disk.mounts {
                    assert!(mount.mount_point.len() <= MAX_LABEL_BYTES, "{mount:?}");
                    assert!(mount.filesystem.len() <= MAX_LABEL_BYTES, "{mount:?}");
                }
                let identities: std::collections::BTreeSet<&str> = disk
                    .mounts
                    .iter()
                    .map(|mount| mount.mount_point.as_str())
                    .collect();
                assert_eq!(
                    identities.len(),
                    disk.mounts.len(),
                    "re-bounding collapsed distinct mount points into one identity: {:?}",
                    disk.mounts
                );
            }
            other => panic!("disk re-bounded into {other:?}"),
        }

        let temperature: Vec<TemperatureSensor> = (0..(MAX_SENSORS + 40))
            .map(|_| TemperatureSensor {
                label: long.clone(),
                celsius: 40.0,
            })
            .collect();
        match bounded_reading(MetricReading::Temperature(temperature)) {
            MetricReading::Temperature(sensors) => {
                assert_eq!(sensors.len(), MAX_SENSORS);
                assert!(sensors.iter().all(|s| s.label.len() <= MAX_LABEL_BYTES));
            }
            other => panic!("temperature re-bounded into {other:?}"),
        }

        let fans: Vec<FanSensor> = (0..(MAX_SENSORS + 40))
            .map(|_| FanSensor {
                // A control character is an injection into an operator's terminal, not a label.
                label: format!("chip\u{1b}[2J/{long}"),
                rpm: 900,
            })
            .collect();
        match bounded_reading(MetricReading::FanSpeed(fans)) {
            MetricReading::FanSpeed(sensors) => {
                assert_eq!(sensors.len(), MAX_SENSORS);
                for sensor in &sensors {
                    assert!(sensor.label.len() <= MAX_LABEL_BYTES);
                    assert!(
                        !sensor.label.chars().any(char::is_control),
                        "a control character survived re-bounding: {:?}",
                        sensor.label
                    );
                }
            }
            other => panic!("fan re-bounded into {other:?}"),
        }

        // A reading that is a fixed set of numbers has nothing to bound and must come back intact.
        let uptime = MetricReading::Uptime(Duration::from_secs(9));
        assert_eq!(bounded_reading(uptime.clone()), uptime);
    }

    /// Instrument labels are bounded identities: trimmed, control-character free, and cut on a
    /// character boundary rather than mid-codepoint.
    #[test]
    fn instrument_labels_are_bounded_and_sanitised() {
        assert_eq!(bounded_label("  coretemp\n"), "coretemp");
        assert_eq!(bounded_label("core\u{7}temp"), "coretemp");
        assert_eq!(bounded_label(&"x".repeat(500)).len(), MAX_LABEL_BYTES);

        let multibyte = bounded_label(&"é".repeat(200));
        assert!(multibyte.len() <= MAX_LABEL_BYTES);
        assert!(multibyte.chars().all(|c| c == 'é'), "cut mid-codepoint");
    }

    /// The CPU ratio is a real fraction of a real window, computed from the delta between two
    /// samples — not a cumulative counter handed to the caller to difference itself.
    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_busy_ratio_is_a_bounded_fraction_of_the_sampled_window() {
        let first = parse_cpu_times(STAT).expect("parse the aggregate cpu line");
        assert_eq!(first.logical_cores, 2);
        assert_eq!(first.total, 980, "user+nice+system+idle+iowait+irq+softirq");
        assert_eq!(first.idle, 820, "idle + iowait");

        let later = parse_cpu_times(
            "cpu  200 0 100 1400 20 0 10 0 0 0\ncpu0 0 0 0 0 0 0 0 0 0 0\ncpu1 0 0 0 0 0 0 0 0 0 0\n",
        )
        .unwrap();
        // 750 jiffies elapsed, 150 of them busy.
        assert!((busy_ratio(&first, &later) - 0.2).abs() < 1e-9);

        // A window in which nothing was recorded is truthfully idle, and never above one.
        assert_eq!(busy_ratio(&first, &first), 0.0);
        assert!((0.0..=1.0).contains(&busy_ratio(&later, &first)));

        assert!(parse_cpu_times("intr 1\nctxt 2\n").is_none(), "no cpu line");
    }

    /// `read_metrics` is the one bounded snapshot: every served kind, once, in canonical order.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn read_metrics_reports_every_served_kind_once_in_canonical_order() {
        let root = sandbox::fixture_dir("metrics-snapshot");
        write_procfs(&root, MEMINFO);
        write_hwmon(&root);
        let system = fixture_system(&root);
        let port: &dyn GuardedMetrics = &system;

        let answers = port.read_metrics().await.unwrap();
        let kinds: Vec<MetricKind> = answers.iter().map(MetricAnswer::kind).collect();
        assert_eq!(kinds, MetricKind::ALL.to_vec());
        assert!(
            answers.len() <= MetricKind::ALL.len(),
            "bounded by the vocabulary"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Re-rooting derives a system at a new workspace, not a new *machine*: the metric roots
    /// travel with it. Dropping them would silently send a re-rooted system — every context-local
    /// worktree — back to the real `/proc`, which a fixture-pinned test could not see.
    #[cfg(target_os = "linux")]
    #[test]
    fn rerooting_preserves_the_pinned_metric_roots() {
        let root = sandbox::fixture_dir("metrics-reroot");
        let elsewhere = sandbox::fixture_dir("metrics-reroot-elsewhere");
        let system = fixture_system(&root);

        let rerooted = system.rerooted(&elsewhere).unwrap();
        assert_eq!(rerooted.metrics_roots(), system.metrics_roots());
        assert_eq!(rerooted.metrics_roots().proc_root(), root.join("proc"));

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// The reader works against the running machine, not only against a fixture: the kinds the
    /// acceptance names as always-served answer through `/proc` and `/sys` too.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_native_system_reads_the_running_machine() {
        let root = sandbox::fixture_dir("metrics-native");
        let system = System::new(Workspace::new(&root).unwrap());
        let port: &dyn GuardedMetrics = &system;

        assert_eq!(port.served_metric_kinds(), MetricKind::ALL.to_vec());

        for kind in [
            MetricKind::CpuUsage,
            MetricKind::LoadAverage,
            MetricKind::Memory,
            MetricKind::Uptime,
            MetricKind::Disk,
        ] {
            let snapshot = served(port.read_metric(kind).await.unwrap());
            assert_eq!(snapshot.kind(), kind);
        }

        // Temperature and fan are honest either way — served, or explicitly unavailable, but never
        // a fabricated zero.
        for kind in [MetricKind::Temperature, MetricKind::FanSpeed] {
            match port.read_metric(kind).await.unwrap() {
                MetricAnswer::Served(snapshot) => assert_eq!(snapshot.kind(), kind),
                MetricAnswer::Unavailable {
                    kind: answered,
                    reason,
                } => {
                    assert_eq!(answered, kind);
                    assert_eq!(reason, MetricUnavailable::NoInstrument);
                }
            }
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
