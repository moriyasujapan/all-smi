// Copyright 2025 Lablup Inc., Jeongkyu Shin and DaeHyun Sung
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

//! Unit tests for the vendor-neutral Windows GPU metrics layer. Split out
//! of `windows_gpu_perf.rs` to keep that file within the 500-line budget.
//!
//! These run on every host, not just Windows: the parent module is gated
//! `cfg(any(target_os = "windows", test))` precisely so the pairing,
//! aggregation, and field-application logic reaches the Linux CI runner.

use super::*;
use crate::device::types::GpuInfo;

fn blank_gpu() -> GpuInfo {
    let mut detail = HashMap::new();
    detail.insert("Metrics Source".to_string(), "WMI".to_string());
    detail.insert("Source: Utilization".to_string(), "unavailable".to_string());
    detail.insert("Source: Memory".to_string(), "WMI".to_string());
    GpuInfo {
        uuid: "PCI\\VEN_1002&DEV_744C".to_string(),
        time: String::new(),
        name: "AMD Radeon RX 7900 XTX".to_string(),
        device_type: "GPU".to_string(),
        host_id: String::new(),
        hostname: String::new(),
        instance: String::new(),
        utilization: 0.0,
        ane_utilization: 0.0,
        dla_utilization: None,
        tensorcore_utilization: None,
        temperature: 0,
        used_memory: 0,
        total_memory: 4_294_967_295,
        frequency: 0,
        power_consumption: 0.0,
        gpu_core_count: None,
        temperature_threshold_slowdown: None,
        temperature_threshold_shutdown: None,
        temperature_threshold_max_operating: None,
        temperature_threshold_acoustic: None,
        performance_state: None,
        fan_speed_rpm: None,
        numa_node_id: None,
        gsp_firmware_mode: None,
        gsp_firmware_version: None,
        nvlink_remote_devices: Vec::new(),
        gpm_metrics: None,
        detail,
    }
}

fn metrics(total: Option<u64>, used: Option<u64>, utilization: Option<f64>) -> AdapterMetrics {
    AdapterMetrics {
        identity: AdapterIdentity {
            luid: AdapterLuid::new(0, 0xD3F5),
            vendor_id: 0x1002,
            device_id: 0x744C,
            description: "AMD Radeon RX 7900 XTX".to_string(),
        },
        total_memory: total,
        memory_is_shared: false,
        dedicated_video_memory: total.unwrap_or(0),
        shared_system_memory: 0,
        used_memory: used,
        used_memory_shared: None,
        utilization,
        process_budget: None,
        process_current_usage: None,
    }
}

/// The Panther Lake / AMD-APU shape: a small dedicated carve-out behind a
/// much larger shared aperture. Numbers default to the ones measured on a
/// real Arc B390 host.
fn shared_metrics(
    dedicated: u64,
    aperture: u64,
    used_dedicated: Option<u64>,
    used_shared: Option<u64>,
) -> AdapterMetrics {
    let (total_memory, memory_is_shared) = classify_adapter_memory(dedicated, aperture);
    AdapterMetrics {
        identity: AdapterIdentity {
            luid: AdapterLuid::new(0, 0x13000),
            vendor_id: 0x8086,
            device_id: 0xB080,
            description: "Intel(R) Arc(TM) B390 GPU".to_string(),
        },
        total_memory,
        memory_is_shared,
        dedicated_video_memory: dedicated,
        shared_system_memory: aperture,
        used_memory: used_dedicated,
        used_memory_shared: used_shared,
        utilization: None,
        process_budget: None,
        process_current_usage: None,
    }
}

#[test]
fn dxgi_total_replaces_the_truncated_wmi_value() {
    let mut gpu = blank_gpu();
    // 24 GB, well beyond what Win32_VideoController.AdapterRAM can
    // represent.
    apply_to_gpu_info(&mut gpu, &metrics(Some(25_769_803_776), None, None));
    assert_eq!(gpu.total_memory, 25_769_803_776);
    assert_eq!(gpu.detail["Source: Memory"], "DXGI");
    assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI");
}

#[test]
fn pdh_fields_are_recorded_with_their_source() {
    let mut gpu = blank_gpu();
    apply_to_gpu_info(
        &mut gpu,
        &metrics(Some(8_589_934_592), Some(2_147_483_648), Some(42.5)),
    );
    assert_eq!(gpu.utilization, 42.5);
    assert_eq!(gpu.used_memory, 2_147_483_648);
    assert_eq!(gpu.detail["Source: Utilization"], "PDH");
    assert_eq!(gpu.detail["Source: Memory Used"], "PDH");
    assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
}

#[test]
fn absent_fields_leave_the_baseline_untouched() {
    // The shape of a GitHub-hosted Windows runner: DXGI answers, no
    // GPU counter instances exist. VRAM must upgrade while
    // utilization stays at its baseline and is not falsely
    // attributed to PDH.
    let mut gpu = blank_gpu();
    gpu.utilization = 0.0;
    apply_to_gpu_info(&mut gpu, &metrics(Some(1_073_741_824), None, None));
    assert_eq!(gpu.total_memory, 1_073_741_824);
    assert_eq!(gpu.utilization, 0.0);
    assert_eq!(gpu.detail["Source: Utilization"], "unavailable");
    assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI");
}

#[test]
fn applying_twice_does_not_grow_the_source_string() {
    let mut gpu = blank_gpu();
    let m = metrics(Some(1), Some(2), Some(3.0));
    apply_to_gpu_info(&mut gpu, &m);
    apply_to_gpu_info(&mut gpu, &m);
    apply_to_gpu_info(&mut gpu, &m);
    assert_eq!(gpu.detail["Metrics Source"], "WMI + DXGI + PDH");
}

#[test]
fn metrics_source_starts_clean_when_absent() {
    let mut detail = HashMap::new();
    note_metrics_source(&mut detail, "DXGI");
    assert_eq!(detail["Metrics Source"], "DXGI");
    note_metrics_source(&mut detail, "PDH");
    assert_eq!(detail["Metrics Source"], "DXGI + PDH");
}

fn snapshot_with(adapters: Vec<AdapterMetrics>, processes: Vec<ProcessGpuMemory>) -> Snapshot {
    Snapshot {
        adapters,
        processes,
    }
}

#[test]
fn pairs_gpus_to_adapters_and_returns_the_luid_index() {
    let mut gpus = vec![blank_gpu()];
    let snapshot = snapshot_with(
        vec![metrics(Some(25_769_803_776), Some(1024), Some(77.0))],
        vec![],
    );

    let index = pair_and_apply(&mut gpus, &snapshot);

    assert_eq!(gpus[0].total_memory, 25_769_803_776);
    assert_eq!(gpus[0].utilization, 77.0);
    assert_eq!(gpus[0].used_memory, 1024);
    // The uuid is the PNPDeviceID, and it is what the per-process
    // attribution keys on.
    assert_eq!(
        index.get(&AdapterLuid::new(0, 0xD3F5)),
        Some(&(0usize, "PCI\\VEN_1002&DEV_744C".to_string()))
    );
}

#[test]
fn pairing_an_empty_snapshot_changes_nothing() {
    let mut gpus = vec![blank_gpu()];
    let before = gpus[0].total_memory;
    let index = pair_and_apply(&mut gpus, &Snapshot::default());
    assert!(index.is_empty());
    assert_eq!(gpus[0].total_memory, before);
    assert_eq!(gpus[0].detail["Metrics Source"], "WMI");
}

#[test]
fn unmatched_gpus_keep_the_wmi_baseline() {
    // A DXGI adapter for a different vendor entirely, and a name
    // that shares no substring, so only the ordinal fallback could
    // pair them.
    let mut gpus = vec![blank_gpu(), blank_gpu()];
    gpus[1].uuid = "PCI\\VEN_10DE&DEV_2684".to_string();
    gpus[1].name = "NVIDIA GeForce RTX 4090".to_string();

    let snapshot = snapshot_with(vec![metrics(Some(8_589_934_592), None, Some(10.0))], vec![]);
    let index = pair_and_apply(&mut gpus, &snapshot);

    // First GPU matches on PCI ids.
    assert_eq!(gpus[0].total_memory, 8_589_934_592);
    // Second has ordinal 1, which is out of range for a one-adapter
    // snapshot, so it is left alone rather than mis-attributed.
    assert_eq!(gpus[1].total_memory, 4_294_967_295);
    assert_eq!(gpus[1].detail["Metrics Source"], "WMI");
    assert_eq!(index.len(), 1);
}

#[test]
fn process_rows_carry_the_gpu_identity_and_leave_the_rest_to_the_merge() {
    let row = gpu_process_row(2, "PCI\\VEN_1002&DEV_744C", 4242, 536_870_912);
    assert_eq!(row.pid, 4242);
    assert_eq!(row.device_id, 2);
    assert_eq!(row.device_uuid, "PCI\\VEN_1002&DEV_744C");
    assert_eq!(row.used_memory, 536_870_912);
    assert!(row.uses_gpu);
    // Deliberately blank: merge_gpu_processes fills these from the
    // system process table.
    assert!(row.process_name.is_empty());
    assert!(row.user.is_empty());
}

#[test]
fn process_rows_are_attributed_to_the_matching_adapter() {
    let known = AdapterLuid::new(0, 0xD3F5);
    let mut adapter_index = AdapterIndex::new();
    adapter_index.insert(known, (0, "PCI\\VEN_1002&DEV_744C".to_string()));

    let snapshot = snapshot_with(
        vec![],
        vec![
            ProcessGpuMemory {
                pid: 4242,
                luid: known,
                dedicated_bytes: 536_870_912,
            },
            // A card this reader never paired, for example an NVIDIA
            // GPU sitting alongside the AMD one. Its processes must
            // be dropped, not attributed to the wrong device.
            ProcessGpuMemory {
                pid: 99,
                luid: AdapterLuid::new(0, 0xFFFF),
                dedicated_bytes: 1,
            },
        ],
    );

    let rows = process_rows_from(&snapshot, &adapter_index);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pid, 4242);
    assert_eq!(rows[0].used_memory, 536_870_912);
    assert_eq!(rows[0].device_uuid, "PCI\\VEN_1002&DEV_744C");
    assert_eq!(rows[0].device_id, 0);
}

#[test]
fn process_scoped_dxgi_figures_are_labelled_and_kept_out_of_used_memory() {
    let mut gpu = blank_gpu();
    let mut m = metrics(Some(8_589_934_592), None, None);
    m.process_budget = Some(7_000_000_000);
    m.process_current_usage = Some(123_456);

    apply_to_gpu_info(&mut gpu, &m);

    // Neither DXGI figure may leak into the device-level number.
    assert_eq!(gpu.used_memory, 0);
    assert_eq!(gpu.detail["VRAM Budget (this process)"], "7000000000 bytes");
    assert_eq!(gpu.detail["VRAM Usage (this process)"], "123456 bytes");
    assert!(!gpu.detail.contains_key("Source: Memory Used"));
}

#[test]
fn the_platform_entry_points_never_panic() {
    // The readers that call these are Windows-gated, but the entry
    // points compile everywhere. On a non-Windows host each must be
    // an inert no-op so the surrounding logic can be tested; on
    // Windows they touch real hardware, so only assert they return.
    let mut gpus = vec![blank_gpu()];
    let index = augment_gpus(&mut gpus);
    let _ = process_rows_with(&index, AdapterIndex::new);
    let _ = pdh_query_available();
    let _ = latest();

    #[cfg(not(target_os = "windows"))]
    {
        assert!(snapshot().is_empty());
        assert!(latest().is_empty());
        assert!(!pdh_query_available());
        assert!(index.is_empty());
        // An empty index triggers the refresh closure; when that
        // also comes back empty the result must be no rows rather
        // than a panic or a wasted system-process enumeration.
        assert!(process_rows_with(&index, AdapterIndex::new).is_empty());
        // An inert layer must leave the WMI baseline exactly as it
        // found it.
        assert_eq!(gpus[0].total_memory, 4_294_967_295);
        assert_eq!(gpus[0].detail["Metrics Source"], "WMI");
    }
}

#[test]
fn an_empty_index_consults_the_refresh_closure_exactly_once() {
    use std::cell::Cell;

    // The one-shot path (`all-smi snapshot --include process`) never
    // calls `get_gpu_info`, so the index starts empty and the
    // closure is what populates it.
    let calls = Cell::new(0);
    let rows = process_rows_with(&AdapterIndex::new(), || {
        calls.set(calls.get() + 1);
        AdapterIndex::new()
    });
    assert_eq!(calls.get(), 1);
    assert!(rows.is_empty());

    // A populated index must not pay for the refresh at all.
    let calls = Cell::new(0);
    let mut populated = AdapterIndex::new();
    populated.insert(AdapterLuid::new(0, 1), (0, "uuid".to_string()));
    let _ = process_rows_with(&populated, || {
        calls.set(calls.get() + 1);
        AdapterIndex::new()
    });
    assert_eq!(calls.get(), 0);
}

#[test]
fn snapshot_helpers_behave_on_an_empty_snapshot() {
    let snapshot = Snapshot::default();
    assert!(snapshot.is_empty());
    assert!(snapshot.identities().is_empty());
    assert!(snapshot.adapter(AdapterLuid::new(0, 1)).is_none());
}

// ---------- Integrated-GPU memory semantics ----------
//
// Fixtures use the figures measured on a real Intel Arc B390 (Panther
// Lake) host: 128 MiB dedicated carve-out, ~16.8 GiB shared aperture,
// 31.4 GiB of system RAM.

const PTL_CARVE_OUT: u64 = 134_217_728; // 128 MiB
const PTL_APERTURE: u64 = 16_844_224_512; // ~16.8 GiB

#[test]
fn an_igpu_carve_out_is_superseded_by_the_shared_aperture() {
    // The bug this fixes: a non-zero carve-out short-circuited the shared
    // fallback, so capacity read as 128 MiB.
    assert_eq!(
        classify_adapter_memory(PTL_CARVE_OUT, PTL_APERTURE),
        (Some(PTL_APERTURE), true)
    );

    // No dedicated pool at all — the original integrated case, unchanged.
    assert_eq!(
        classify_adapter_memory(0, PTL_APERTURE),
        (Some(PTL_APERTURE), true)
    );

    // A 24 GiB discrete card keeps its own VRAM as capacity.
    assert_eq!(
        classify_adapter_memory(25_769_803_776, PTL_APERTURE),
        (Some(25_769_803_776), false)
    );

    // An AMD APU with an 8 GiB BIOS UMA carve-out has genuinely reserved
    // that memory: above the threshold, so it stays the honest capacity.
    assert_eq!(
        classify_adapter_memory(8_589_934_592, PTL_APERTURE),
        (Some(8_589_934_592), false)
    );

    // A small-VRAM discrete card on a small-RAM host: the aperture is not
    // >= 2x the pool, so it is not reclassified.
    assert_eq!(
        classify_adapter_memory(512 * 1024 * 1024, 768 * 1024 * 1024),
        (Some(512 * 1024 * 1024), false)
    );

    // Nothing known either way.
    assert_eq!(classify_adapter_memory(0, 0), (None, false));
}

#[test]
fn shared_adapters_report_shared_usage_as_used_memory() {
    let mut gpu = blank_gpu();
    // Dedicated Usage reads a flat 0 on this hardware; the truth is in
    // Shared Usage.
    apply_to_gpu_info(
        &mut gpu,
        &shared_metrics(PTL_CARVE_OUT, PTL_APERTURE, Some(0), Some(14_052_360_192)),
    );

    assert_eq!(gpu.total_memory, PTL_APERTURE);
    assert_eq!(gpu.used_memory, 14_052_360_192);
    assert_eq!(gpu.detail["Source: Memory"], "DXGI (shared)");
    assert_eq!(gpu.detail["Source: Memory Used"], "PDH (shared)");
}

#[test]
fn a_shared_adapter_without_a_shared_counter_keeps_the_carve_out_label() {
    // A host that publishes no Shared Usage instances must keep its
    // previous reading and its previous, exact provenance string.
    let mut gpu = blank_gpu();
    apply_to_gpu_info(
        &mut gpu,
        &shared_metrics(PTL_CARVE_OUT, PTL_APERTURE, Some(4_194_304), None),
    );

    assert_eq!(gpu.used_memory, 4_194_304);
    assert_eq!(
        gpu.detail["Source: Memory Used"],
        "PDH (dedicated carve-out only)"
    );
}

#[test]
fn carve_out_and_aperture_detail_keys_follow_the_convention() {
    let mut gpu = blank_gpu();
    apply_to_gpu_info(
        &mut gpu,
        &shared_metrics(PTL_CARVE_OUT, PTL_APERTURE, None, Some(1)),
    );

    // Key carries the quantity, value carries the unit.
    assert_eq!(gpu.detail["VRAM Dedicated Carve-out"], "134217728 bytes");
    assert_eq!(gpu.detail["VRAM Shared Aperture"], "16844224512 bytes");
    // The units must NOT migrate into the key.
    assert!(!gpu.detail.contains_key("VRAM Dedicated Carve-out (bytes)"));
    assert!(!gpu.detail.contains_key("VRAM Shared Aperture (bytes)"));
}

#[test]
fn a_dedicated_adapter_publishes_no_carve_out_keys() {
    let mut gpu = blank_gpu();
    apply_to_gpu_info(&mut gpu, &metrics(Some(25_769_803_776), Some(1024), None));
    assert!(!gpu.detail.contains_key("VRAM Dedicated Carve-out"));
    assert!(!gpu.detail.contains_key("VRAM Shared Aperture"));
    assert_eq!(gpu.detail["Source: Memory"], "DXGI");
    assert_eq!(gpu.detail["Source: Memory Used"], "PDH");
}

#[test]
fn an_amd_apu_carve_out_is_treated_the_same_as_an_intel_one() {
    // This layer is shared with the AMD Windows reader, so the fix
    // deliberately changes AMD APU reporting too. Pin that it does.
    let mut apu = shared_metrics(
        512 * 1024 * 1024, // typical Radeon 780M carve-out
        12_884_901_888,    // ~12 GiB aperture
        Some(0),
        Some(2_147_483_648),
    );
    apu.identity.vendor_id = 0x1002;
    apu.identity.device_id = 0x15BF;
    apu.identity.description = "AMD Radeon 780M Graphics".to_string();

    let mut gpu = blank_gpu();
    apply_to_gpu_info(&mut gpu, &apu);

    assert_eq!(gpu.total_memory, 12_884_901_888);
    assert_eq!(gpu.used_memory, 2_147_483_648);
    assert_eq!(gpu.detail["Source: Memory"], "DXGI (shared)");
    assert_eq!(gpu.detail["VRAM Dedicated Carve-out"], "536870912 bytes");
}
