// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Vendor-neutral Windows GPU metrics, shared by the AMD and Intel
//! Windows readers.
//!
//! Both readers were WMI-only baselines: they could name a card and
//! little else. `Win32_VideoController` publishes no utilization, no
//! temperature, no per-process data, and its `AdapterRAM` field is a
//! `uint32` that saturates at 4 GB. This module closes the gaps that do
//! not need a vendor SDK, using two facilities every WDDM driver feeds:
//!
//! - **DXGI** for the true dedicated VRAM size and the adapter identity
//!   (LUID, PCI vendor / device).
//! - **PDH** for device utilization, system-wide used VRAM, and
//!   per-process VRAM. This is Task Manager's data source.
//!
//! Temperature, power, and fan speed are deliberately absent: WDDM does
//! not publish them, and they remain the job of the vendor backends
//! (Level Zero for Intel, ADL for AMD).
//!
//! ## Precedence
//!
//! A vendor backend, when it produces a reading, outranks this layer,
//! which in turn outranks the WMI baseline. Callers enforce that by
//! applying this layer first and letting the vendor augmentation
//! overwrite afterwards. Each field records where it came from in the
//! `Source: *` detail keys the Intel reader already established.
//!
//! ## Known limitation: one-shot invocations report no utilization
//!
//! Utilization is a PDH rate counter, so it only exists once two
//! collections can be differenced. Any caller that samples exactly once
//! and exits, `all-smi snapshot` at its default `--samples 1` or a
//! library consumer doing a single `get_gpu_info`, therefore sees the
//! WMI baseline utilization rather than a real figure. The polling
//! paths (`view` and `api`) are unaffected from their second poll
//! onward. Memory figures are gauges and are correct from the first
//! collection either way.
//!
//! ## Platform gating
//!
//! The DXGI and PDH FFI submodules are Windows-only. Everything else
//! here, the identifier parsing in [`ids`], the adapter pairing, and the
//! field application, is compiled under `cfg(any(target_os = "windows",
//! test))` so that a `cargo test` run on any host builds and exercises
//! it.
//!
//! That is deliberate rather than incidental. No CI job builds all-smi
//! for Windows at all, so logic reachable only on Windows ships with no
//! automated coverage whatsoever. Keeping the parsing and the
//! arithmetic testable on the Linux runner is the only coverage this
//! code can actually get; the FFI beneath it is verified by a
//! cross-compile check and by `all-smi doctor` output from real
//! machines.

pub mod ids;

#[cfg(target_os = "windows")]
mod dxgi;
#[cfg(target_os = "windows")]
mod pdh;

use crate::device::types::{GpuInfo, ProcessInfo};
use ids::{AdapterIdentity, AdapterLuid};
use std::collections::HashMap;

/// Everything the shared layer learned about one adapter.
#[derive(Clone, Debug)]
pub struct AdapterMetrics {
    pub identity: AdapterIdentity,
    /// Adapter capacity in bytes, from DXGI.
    pub total_memory: Option<u64>,
    /// Whether [`Self::total_memory`] is a dedicated VRAM pool or the
    /// shared system-memory aperture an integrated GPU uses.
    ///
    /// Recorded so the provenance the reader publishes does not claim
    /// more than the number delivers: "DXGI" and "DXGI (shared)" mean
    /// materially different things to someone reading a memory gauge.
    pub memory_is_shared: bool,
    /// `DXGI_ADAPTER_DESC1::DedicatedVideoMemory` verbatim, in bytes.
    ///
    /// Kept alongside [`Self::total_memory`] because on a shared-memory
    /// adapter the two differ: the total becomes the aperture while this
    /// stays the small stolen carve-out, which is still worth publishing
    /// as a detail key.
    pub dedicated_video_memory: u64,
    /// `DXGI_ADAPTER_DESC1::SharedSystemMemory` verbatim, in bytes.
    pub shared_system_memory: u64,
    /// System-wide dedicated VRAM in use, in bytes, from the PDH
    /// `GPU Adapter Memory\Dedicated Usage` counter.
    pub used_memory: Option<u64>,
    /// System-wide *shared* GPU memory in use, in bytes, from the PDH
    /// `GPU Adapter Memory\Shared Usage` counter.
    ///
    /// This is the only meaningful used-memory figure on an integrated
    /// GPU: the dedicated counter reads a flat 0 there, because nothing
    /// is allocated out of the tiny carve-out. It is what Task Manager
    /// shows as "Shared GPU memory".
    pub used_memory_shared: Option<u64>,
    /// Device utilization, 0..=100, from the PDH `GPU Engine` counters.
    pub utilization: Option<f64>,
    /// Process-scoped DXGI budget, in bytes. Diagnostics only.
    pub process_budget: Option<u64>,
    /// Process-scoped DXGI current usage, in bytes. Diagnostics only.
    pub process_current_usage: Option<u64>,
}

/// Per-process dedicated GPU memory, keyed by adapter.
#[derive(Clone, Debug)]
pub struct ProcessGpuMemory {
    pub pid: u32,
    pub luid: AdapterLuid,
    pub dedicated_bytes: u64,
}

/// One poll's worth of vendor-neutral GPU data.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub adapters: Vec<AdapterMetrics>,
    pub processes: Vec<ProcessGpuMemory>,
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty() && self.processes.is_empty()
    }

    /// Adapter identities in enumeration order, for
    /// [`ids::match_adapter`].
    pub fn identities(&self) -> Vec<AdapterIdentity> {
        self.adapters
            .iter()
            .map(|adapter| adapter.identity.clone())
            .collect()
    }

    /// Look up the metrics for a specific adapter.
    pub fn adapter(&self, luid: AdapterLuid) -> Option<&AdapterMetrics> {
        self.adapters
            .iter()
            .find(|adapter| adapter.identity.luid == luid)
    }
}

/// How long a snapshot stays fresh enough to be reused instead of
/// taking another PDH collection.
///
/// This is not an optimisation, it is a correctness requirement.
/// `Utilization Percentage` is a rate computed between consecutive
/// collections, so two collections microseconds apart yield a rate over
/// a microsecond, which reads as 0 or as a clamped 100 rather than as
/// the load over the poll interval.
///
/// More than one reader can be registered at once: `get_gpu_readers`
/// tests for AMD and Intel adapters with independent `if`s, and a laptop
/// with an Intel iGPU beside a Radeon dGPU (or an AMD APU beside an Arc
/// card) registers both. The collectors then call `get_gpu_info` on each
/// in turn within the same poll. Without coalescing, whichever reader
/// runs second would always see a near-zero interval, deterministically,
/// and its utilization would be useless. `get_gpu_info_by_uuid`'s
/// default body has the same shape when called in a loop.
///
/// 500 ms is comfortably below all-smi's fastest poll interval (1 s for
/// local monitoring) so consecutive polls still each get a fresh
/// collection, and comfortably above the time it takes to walk a
/// handful of readers.
#[cfg(target_os = "windows")]
const SNAPSHOT_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Take a sample, reusing the cached one when it is younger than
/// [`SNAPSHOT_COALESCE_WINDOW`].
///
/// Safe to call from every registered reader in the same poll: only the
/// first call collects, and the rest share its result.
#[cfg(target_os = "windows")]
pub fn snapshot() -> Snapshot {
    if let Some(cached) = cached_snapshot(SNAPSHOT_COALESCE_WINDOW) {
        return cached;
    }
    let dxgi_adapters = dxgi::enumerate();
    let sample = pdh::sample();

    let adapters = dxgi_adapters
        .into_iter()
        .map(|adapter| {
            let luid = adapter.identity.luid;
            let (total_memory, memory_is_shared) = classify_adapter_memory(
                adapter.dedicated_video_memory,
                adapter.shared_system_memory,
            );
            AdapterMetrics {
                identity: adapter.identity,
                total_memory,
                memory_is_shared,
                dedicated_video_memory: adapter.dedicated_video_memory,
                shared_system_memory: adapter.shared_system_memory,
                used_memory: sample.adapter_memory.get(&luid).copied(),
                used_memory_shared: sample.adapter_shared_memory.get(&luid).copied(),
                utilization: sample.utilization.get(&luid).copied(),
                process_budget: adapter.process_budget,
                process_current_usage: adapter.process_current_usage,
            }
        })
        .collect();

    let processes = sample
        .process_memory
        .into_iter()
        .filter(|(_, bytes)| *bytes > 0)
        .map(|(instance, bytes)| ProcessGpuMemory {
            pid: instance.pid,
            luid: instance.luid,
            dedicated_bytes: bytes,
        })
        .collect();

    let snapshot = Snapshot {
        adapters,
        processes,
    };
    store_snapshot(&snapshot);
    snapshot
}

/// Non-Windows builds have nothing to sample. The readers that call this
/// are themselves Windows-gated; the stub exists so the surrounding
/// logic and its tests compile on every platform.
#[cfg(not(target_os = "windows"))]
pub fn snapshot() -> Snapshot {
    Snapshot::default()
}

#[cfg(target_os = "windows")]
type SnapshotCache = std::sync::Mutex<Option<(std::time::Instant, Snapshot)>>;

#[cfg(target_os = "windows")]
static LAST_SNAPSHOT: once_cell::sync::OnceCell<SnapshotCache> = once_cell::sync::OnceCell::new();

#[cfg(target_os = "windows")]
fn snapshot_cache() -> &'static SnapshotCache {
    LAST_SNAPSHOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// The cached snapshot, if it is younger than `max_age`.
#[cfg(target_os = "windows")]
fn cached_snapshot(max_age: std::time::Duration) -> Option<Snapshot> {
    let guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .as_ref()
        .and_then(|(taken_at, snapshot)| (taken_at.elapsed() < max_age).then(|| snapshot.clone()))
}

#[cfg(target_os = "windows")]
fn store_snapshot(snapshot: &Snapshot) {
    let mut guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some((std::time::Instant::now(), snapshot.clone()));
}

/// The most recent [`snapshot`], without consuming a PDH collection.
///
/// `get_process_info` uses this so a poll that already sampled from
/// `get_gpu_info` does not disturb the utilization rate. Unlike
/// [`snapshot`] this never collects, and returns an empty snapshot when
/// nothing has been sampled yet.
#[cfg(target_os = "windows")]
pub fn latest() -> Snapshot {
    let guard = match snapshot_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .as_ref()
        .map(|(_, snapshot)| snapshot.clone())
        .unwrap_or_default()
}

#[cfg(not(target_os = "windows"))]
pub fn latest() -> Snapshot {
    Snapshot::default()
}

/// Whether the PDH GPU counter query could be opened.
#[cfg(target_os = "windows")]
pub fn pdh_query_available() -> bool {
    pdh::query_available()
}

#[cfg(not(target_os = "windows"))]
pub fn pdh_query_available() -> bool {
    false
}

/// Largest dedicated pool still treated as an integrated GPU's stolen
/// carve-out rather than as real VRAM.
///
/// iGPU carve-outs run 64 MiB – 512 MiB (Panther Lake reports 128 MiB).
/// An AMD APU configured with a BIOS `UMA Buffer Size` of 4 or 8 GiB has
/// genuinely reserved that memory and its dedicated figure *is* the
/// honest capacity, so the threshold must sit below those.
const SHARED_CARVE_OUT_MAX: u64 = 1024 * 1024 * 1024;

/// Decide an adapter's capacity and whether it is shared-memory.
///
/// Returns `(total, memory_is_shared)`.
///
/// An adapter is shared-memory when it reports no dedicated pool at all,
/// or when its dedicated pool is small enough to be a stolen carve-out
/// while a much larger aperture sits behind it. In both cases the
/// aperture — what Task Manager calls "Shared GPU memory" — is the number
/// a capacity gauge should show.
///
/// The carve-out case is not hypothetical: Panther Lake's Arc B390
/// reports 128 MiB dedicated against a ~16.8 GiB aperture, and reading
/// the 128 MiB as capacity made all-smi's memory gauge useless. The
/// `shared >= 2 * dedicated` conjunct keeps a genuinely small-VRAM
/// discrete card on a small-RAM host from being reclassified.
///
/// Split out of `snapshot()` (which is Windows-only) so the Linux test
/// runner — the only always-on CI runner — actually exercises this.
pub fn classify_adapter_memory(dedicated: u64, shared: u64) -> (Option<u64>, bool) {
    let is_carve_out = dedicated > 0 && dedicated < SHARED_CARVE_OUT_MAX && shared >= 2 * dedicated;
    if (dedicated == 0 || is_carve_out) && shared > 0 {
        return (Some(shared), true);
    }
    if dedicated > 0 {
        return (Some(dedicated), false);
    }
    (None, false)
}

// Moved to `super::detail_keys` so the Level Zero backend — which is
// compiled on Linux too, where this module is not — can append to the
// same string instead of overwriting it. Re-exported here because
// callers and tests already refer to it by this path.
pub use super::detail_keys::note_metrics_source;

/// Layer this adapter's metrics onto a WMI-derived [`GpuInfo`].
///
/// Only fields that carry real data are written, so a partially
/// available adapter (DXGI present, PDH counters absent, which is the
/// shape of a GitHub-hosted Windows runner) upgrades VRAM and leaves
/// utilization at the baseline rather than zeroing anything that was
/// already known.
pub fn apply_to_gpu_info(gpu: &mut GpuInfo, metrics: &AdapterMetrics) {
    let mut touched_dxgi = false;
    let mut touched_pdh = false;

    if let Some(total) = metrics.total_memory {
        gpu.total_memory = total;
        gpu.detail.insert(
            "Source: Memory".to_string(),
            if metrics.memory_is_shared {
                "DXGI (shared)"
            } else {
                "DXGI"
            }
            .to_string(),
        );
        touched_dxgi = true;
    }

    // Both DXGI video-memory figures are scoped to the calling process.
    // They are labelled as such and kept out of `used_memory`, which
    // must stay system-wide; reading either as a device-level number
    // would understate a busy GPU by whatever other processes hold.
    if let Some(budget) = metrics.process_budget {
        gpu.detail.insert(
            "VRAM Budget (this process)".to_string(),
            format!("{budget} bytes"),
        );
        touched_dxgi = true;
    }
    if let Some(usage) = metrics.process_current_usage {
        gpu.detail.insert(
            "VRAM Usage (this process)".to_string(),
            format!("{usage} bytes"),
        );
        touched_dxgi = true;
    }

    // On a shared-memory adapter the `Dedicated Usage` counter tracks only
    // the small stolen carve-out — in practice a flat 0 — while the real
    // consumption sits in `Shared Usage`. Prefer the shared counter there,
    // and fall back to the dedicated one so a host that publishes no
    // shared instances keeps its previous (honest, if narrow) reading.
    let (used_memory, used_source) = if metrics.memory_is_shared {
        match metrics.used_memory_shared {
            Some(used) => (Some(used), "PDH (shared)"),
            // Preserved verbatim: this exact string predates the shared
            // counter and downstream consumers match on it.
            None => (metrics.used_memory, "PDH (dedicated carve-out only)"),
        }
    } else {
        (metrics.used_memory, "PDH")
    };
    if let Some(used) = used_memory {
        gpu.used_memory = used;
        gpu.detail
            .insert("Source: Memory Used".to_string(), used_source.to_string());
        touched_pdh = true;
    }

    // Publish both raw DXGI figures on a shared adapter. `total_memory`
    // now carries the aperture, so the carve-out would otherwise be lost —
    // and it is what someone comparing against Task Manager's "Dedicated
    // GPU memory" line will be looking for.
    if metrics.memory_is_shared {
        if metrics.dedicated_video_memory > 0 {
            gpu.detail.insert(
                "VRAM Dedicated Carve-out".to_string(),
                format!("{} bytes", metrics.dedicated_video_memory),
            );
        }
        if metrics.shared_system_memory > 0 {
            gpu.detail.insert(
                "VRAM Shared Aperture".to_string(),
                format!("{} bytes", metrics.shared_system_memory),
            );
        }
    }

    if let Some(utilization) = metrics.utilization {
        gpu.utilization = utilization;
        gpu.detail
            .insert("Source: Utilization".to_string(), "PDH".to_string());
        touched_pdh = true;
    }

    if touched_dxgi {
        note_metrics_source(&mut gpu.detail, "DXGI");
    }
    if touched_pdh {
        note_metrics_source(&mut gpu.detail, "PDH");
    }
}

/// Map from adapter LUID to the `(index, uuid)` of the GPU it was
/// paired with. Readers keep the most recent one so per-process PDH rows
/// can be attributed to a card.
pub type AdapterIndex = HashMap<AdapterLuid, (usize, String)>;

/// Pair each WMI-derived GPU with a DXGI adapter, apply that adapter's
/// metrics, and return the LUID mapping.
///
/// Split from [`augment_gpus`] so the pairing and application logic can
/// be exercised with a synthetic snapshot on any platform. The Windows
/// FFI is only reachable through `snapshot()`, which the thin wrapper
/// calls.
pub fn pair_and_apply(gpus: &mut [GpuInfo], snapshot: &Snapshot) -> AdapterIndex {
    let mut adapter_index = AdapterIndex::new();
    if snapshot.adapters.is_empty() {
        return adapter_index;
    }
    let identities = snapshot.identities();

    for (ordinal, gpu) in gpus.iter_mut().enumerate() {
        // The reader stores `PNPDeviceID` as the GPU uuid, and that is
        // what carries the PCI vendor / device ids the matcher prefers.
        let Some(identity) =
            ids::match_adapter(&identities, Some(gpu.uuid.as_str()), &gpu.name, ordinal)
        else {
            continue;
        };
        let luid = identity.luid;
        if let Some(metrics) = snapshot.adapter(luid) {
            apply_to_gpu_info(gpu, metrics);
        }
        adapter_index.insert(luid, (ordinal, gpu.uuid.clone()));
    }

    adapter_index
}

/// Take a fresh sample and layer it onto `gpus`.
///
/// Call once per poll from `get_gpu_info`; the returned index feeds
/// [`process_rows`].
pub fn augment_gpus(gpus: &mut [GpuInfo]) -> AdapterIndex {
    let snapshot = snapshot();
    pair_and_apply(gpus, &snapshot)
}

/// Build the merged per-process rows a reader's `get_process_info`
/// should return.
///
/// Two things happen here beyond reading the snapshot:
///
/// 1. If `adapter_index` is empty, nothing has paired adapters yet. That
///    is the case for one-shot entry points such as
///    `all-smi snapshot --include process`, which never call
///    `get_gpu_info`. Rather than reporting an empty list forever, the
///    caller is given a chance to populate the index first via
///    `refresh`.
/// 2. The GPU rows are merged against the system process table, matching
///    what `nvidia`, `nvidia_jetson`, and `tenstorrent` do inside their
///    own `get_process_info`. The API and snapshot collectors consume
///    `get_process_info` directly without merging, so returning bare
///    skeleton rows would export processes with empty names and zeroed
///    CPU and RSS.
pub fn process_rows_with<F>(adapter_index: &AdapterIndex, refresh: F) -> Vec<ProcessInfo>
where
    F: FnOnce() -> AdapterIndex,
{
    let owned;
    let adapter_index = if adapter_index.is_empty() {
        owned = refresh();
        &owned
    } else {
        adapter_index
    };
    if adapter_index.is_empty() {
        return Vec::new();
    }

    let gpu_rows = process_rows_from(&latest(), adapter_index);
    if gpu_rows.is_empty() {
        return Vec::new();
    }

    let gpu_pids: std::collections::HashSet<u32> = gpu_rows.iter().map(|row| row.pid).collect();
    let all_processes = crate::utils::system::with_global_system(|system| {
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::everything().with_user(sysinfo::UpdateKind::Always),
        );
        system.refresh_memory();
        crate::device::process_list::get_all_processes(system, &gpu_pids)
    });

    crate::device::process_list::merge_gpu_processes(all_processes, gpu_rows)
}

/// Attribute each per-process sample to the card its LUID names.
///
/// Split from [`process_rows`] for the same reason as
/// [`pair_and_apply`]: it makes the attribution testable without
/// Windows. Rows whose adapter was never paired are dropped rather than
/// guessed at, so a card the WMI vendor filter excluded (an NVIDIA GPU
/// alongside an AMD one, say) does not have its processes reported
/// against the wrong device.
pub fn process_rows_from(snapshot: &Snapshot, adapter_index: &AdapterIndex) -> Vec<ProcessInfo> {
    snapshot
        .processes
        .iter()
        .filter_map(|process| {
            let (device_id, uuid) = adapter_index.get(&process.luid)?;
            Some(gpu_process_row(
                *device_id,
                uuid,
                process.pid,
                process.dedicated_bytes,
            ))
        })
        .collect()
}

/// Build the GPU-attributed process row for a PDH per-process sample.
///
/// Only the GPU-specific fields are populated.
/// [`crate::device::process_list::merge_gpu_processes`] joins these rows
/// against the system process table by pid and supplies the name, user,
/// CPU time, and system-memory figures, so filling them here would be
/// both wasted work and a second source of truth that could disagree.
pub fn gpu_process_row(
    device_id: usize,
    device_uuid: &str,
    pid: u32,
    used_memory: u64,
) -> ProcessInfo {
    ProcessInfo {
        device_id,
        device_uuid: device_uuid.to_string(),
        pid,
        process_name: String::new(),
        used_memory,
        cpu_percent: 0.0,
        memory_percent: 0.0,
        memory_rss: 0,
        memory_vms: 0,
        user: String::new(),
        state: String::new(),
        start_time: String::new(),
        cpu_time: 0,
        command: String::new(),
        ppid: 0,
        threads: 0,
        uses_gpu: true,
        priority: 0,
        nice_value: 0,
        // PDH publishes GPU *memory* per process. Per-process engine
        // utilization would need the GPU Engine counters keyed by pid,
        // which report per-engine shares that do not reduce to a single
        // per-process figure the way memory does. Left at zero rather
        // than guessed.
        gpu_utilization: 0.0,
    }
}

#[cfg(test)]
#[path = "windows_gpu_perf/tests.rs"]
mod tests;
