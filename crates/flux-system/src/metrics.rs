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
//!   machine cannot turn one metrics read into an unbounded allocation.
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
    /// Per-mount capacity, at most [`MAX_MOUNTS`] entries.
    Disk(Vec<MountUsage>),
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

/// Reduce an instrument label to a bounded, control-character-free identity.
///
/// Public because [`MAX_LABEL_BYTES`] is part of the contract, not an implementation detail of the
/// native reader: a backend that builds readings from somewhere else — a remote host's report, a
/// Kubernetes node listing — has to honour the same bound, and it should do so through the same
/// code rather than by re-deriving it.
pub fn bounded_label(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_BYTES)
        .collect();
    match cleaned
        .char_indices()
        .find(|(index, c)| index + c.len_utf8() > MAX_LABEL_BYTES)
    {
        Some((index, _)) => cleaned[..index].to_string(),
        None => cleaned,
    }
}

#[cfg(target_os = "linux")]
mod linux {
    //! Reading the local machine: procfs text, sysfs hwmon files, and `statvfs`.
    //!
    //! Every failure here is an *answer* rather than an error: a kernel interface this machine does
    //! not expose is [`MetricUnavailable::NoInstrument`], and one that exists but would not parse is
    //! [`MetricUnavailable::ReadFailed`]. Reporting zero in either case would be a lie a projection
    //! could not distinguish from a genuinely idle machine.

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
        "autofs",
        "bpf",
        "binfmt_misc",
        "cgroup",
        "cgroup2",
        "cifs",
        "configfs",
        "debugfs",
        "devpts",
        "devtmpfs",
        "efivarfs",
        "fuse.sshfs",
        "fusectl",
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
                aggregate = Some((values.iter().sum::<u64>(), values[3] + values[4]));
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
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(MetricUnavailable::ReadFailed);
        }
        Ok(MetricReading::Uptime(Duration::from_secs_f64(seconds)))
    }

    // -- memory and swap ------------------------------------------------------------------------

    /// `meminfo` reports kibibytes; every reading is in bytes.
    fn parse_meminfo(text: &str) -> BTreeMap<&str, u64> {
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let Some((key, rest)) = line.split_once(':') else {
                continue;
            };
            if let Some(value) = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
            {
                fields.insert(key, value.saturating_mul(1024));
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

    /// `(mount point, filesystem type)` for every mount that is real disk capacity.
    fn parse_mounts(text: &str) -> Vec<(String, String)> {
        let mut mounts: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(_device), Some(point), Some(filesystem)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if NON_DISK_FILESYSTEMS.contains(&filesystem) {
                continue;
            }
            mounts.push((
                unescape_mount_field(point),
                bounded_label(&unescape_mount_field(filesystem)),
            ));
        }
        mounts.sort();
        mounts.dedup_by(|a, b| a.0 == b.0);
        mounts.truncate(MAX_MOUNTS);
        mounts
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
            mount_point: bounded_label(point),
            filesystem: filesystem.to_string(),
            total_bytes: blocks.saturating_mul(frsize),
            available_bytes: widen(stat.f_bavail).saturating_mul(frsize),
            used_bytes: blocks
                .saturating_sub(widen(stat.f_bfree))
                .saturating_mul(frsize),
        })
    }

    fn read_disk(roots: &MetricsRoots) -> std::result::Result<MetricReading, MetricUnavailable> {
        let text = read_proc(roots, "mounts")?;
        let mounts: Vec<MountUsage> = parse_mounts(&text)
            .into_iter()
            .filter_map(|(point, filesystem)| mount_usage(&point, &filesystem))
            .collect();
        if mounts.is_empty() {
            return Err(MetricUnavailable::NoInstrument);
        }
        Ok(MetricReading::Disk(mounts))
    }

    // -- hwmon ----------------------------------------------------------------------------------

    /// `(index, chip label prefix, value text)` for every `<prefix><N>_input` in one hwmon chip,
    /// ordered by index so the reported list is stable across reads.
    fn chip_inputs(chip: &Path, prefix: &str) -> Vec<(u32, String, String)> {
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

        let mut inputs = Vec::new();
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
            let Some(value) = read_capped(&entry.path(), SYSFS_FILE_CAP) else {
                continue;
            };
            let label = read_capped(&chip.join(format!("{prefix}{index}_label")), SYSFS_FILE_CAP)
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| format!("{prefix}{index}"));
            inputs.push((index, bounded_label(&format!("{chip_name}/{label}")), value));
        }
        inputs.sort_by_key(|(index, _, _)| *index);
        inputs
    }

    /// Every hwmon chip directory, in a stable order.
    fn chips(roots: &MetricsRoots) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(roots.sys_root().join("class/hwmon")) else {
            return Vec::new();
        };
        let mut chips: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        chips.sort();
        chips
    }

    fn read_temperature(
        roots: &MetricsRoots,
    ) -> std::result::Result<MetricReading, MetricUnavailable> {
        let mut sensors = Vec::new();
        for chip in chips(roots) {
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
            MetricReading::Disk(mounts) => {
                assert_eq!(
                    mounts.len(),
                    1,
                    "the pseudo filesystems beside the real one must be dropped: {mounts:?}"
                );
                assert_eq!(mounts[0].filesystem, "ext4");
                assert_eq!(mounts[0].mount_point, root.display().to_string());
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
            MetricReading::Disk(mounts) => assert_eq!(mounts.len(), MAX_MOUNTS),
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
            MetricReading::Disk(Vec::new()),
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
