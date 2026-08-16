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

//! Unit tests for the Intel client GPU name / architecture / variant
//! classifiers. Split out of `intel_gpu_names.rs` to keep that file under
//! the 500-line budget, mirroring the `intel_gpu_windows/tests.rs` split.

use super::*;

#[test]
fn known_families_resolve() {
    assert!(intel_gpu_marketing_name(0x56A0).contains("Arc A770"));
    assert!(intel_gpu_marketing_name(0x56A2).contains("Arc A750"));
    assert!(intel_gpu_marketing_name(0xE20B).contains("Battlemage"));
    assert!(intel_gpu_marketing_name(0x7D40).contains("Meteor Lake"));
    assert!(intel_gpu_marketing_name(0x9A49).contains("Tiger Lake"));
    assert!(intel_gpu_marketing_name(0x46A6).contains("Alder/Raptor Lake"));
    assert!(intel_gpu_marketing_name(0x4C8A).contains("Rocket Lake"));
    assert!(intel_gpu_marketing_name(0x8A50).contains("Ice Lake"));
    assert!(intel_gpu_marketing_name(0xA780).contains("Arrow/Lunar Lake"));
}

#[test]
fn unknown_falls_back_to_generic() {
    let n = resolve_intel_gpu_name(0x1234);
    assert!(n.starts_with("Intel Graphics (device"));
    assert!(n.contains("0x1234"));
}

#[test]
fn high_bits_ignored() {
    // Some lspci output reports IDs with the upper 16 bits set;
    // we mask to the device portion before matching.
    assert!(resolve_intel_gpu_name(0x0000_56A0).contains("Arc A770"));
    assert!(resolve_intel_gpu_name(0xFFFF_56A0).contains("Arc A770"));
}

// ---------- Architecture classification tests ----------
//
// The fixtures below mirror lablup/backend.ai-go's `INTEL_GPU_PATTERNS`
// and `check_intel_sycl_support` tests so the two projects stay in
// agreement about what each marketing name means.

#[test]
fn classifies_arc_a_series_as_alchemist() {
    for name in &[
        "Intel Arc A770 Graphics",
        "Intel Arc A750",
        "Intel Arc A580",
        "Intel Arc A380",
        "Intel Arc A310",
        "Intel(R) Arc(TM) A770 Graphics",
    ] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::Alchemist,
            "mis-classified: {name}"
        );
        assert!(IntelArchitecture::Alchemist.is_sycl_capable());
    }
}

#[test]
fn classifies_battlemage_b_series() {
    for name in &[
        "Intel Battlemage Graphics",
        "Intel(R) Battlemage(TM) Graphics",
        "Intel Arc B580",
        "Intel(R) Arc(TM) B580 Graphics",
    ] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::Battlemage,
            "mis-classified: {name}"
        );
        assert!(IntelArchitecture::Battlemage.is_sycl_capable());
    }
}

#[test]
fn classifies_core_ultra_integrated_arc_as_xe_lpg() {
    // Arc integrated graphics on Core Ultra (Meteor Lake, no A-series
    // model number) is Xe-LPG, not Alchemist.
    assert_eq!(
        classify_intel_architecture("Intel Arc Graphics"),
        IntelArchitecture::XeLpg,
    );
    assert_eq!(
        classify_intel_architecture("Intel(R) Arc(TM) Graphics"),
        IntelArchitecture::XeLpg,
    );
    assert!(IntelArchitecture::XeLpg.is_sycl_capable());
}

#[test]
fn classifies_lunar_lake_arc_140v() {
    // Arc 140V / 130V on Lunar Lake — should map to XeLpgPlus, not
    // Alchemist. "140V" contains "a" in "140V Graphics" but no A3/A5/A7
    // token, so the Alchemist matcher must not fire.
    let result = classify_intel_architecture("Intel Arc 140V Graphics");
    assert!(
        matches!(
            result,
            IntelArchitecture::XeLpgPlus | IntelArchitecture::XeLpg
        ),
        "Arc 140V should classify as a Lunar Lake / Xe-LPG-family part, got {result:?}",
    );
    assert!(result.is_sycl_capable());

    // Lunar Lake's other iGPU SKU.
    let result_130v = classify_intel_architecture("Intel Arc 130V Graphics");
    assert!(
        matches!(
            result_130v,
            IntelArchitecture::XeLpgPlus | IntelArchitecture::XeLpg
        ),
        "Arc 130V should classify as a Lunar Lake / Xe-LPG-family part, got {result_130v:?}",
    );
}

#[test]
fn classifies_iris_xe_as_iris_xe() {
    for name in &["Intel Iris Xe Graphics", "Intel(R) Iris(R) Xe Graphics"] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::IrisXe,
            "mis-classified: {name}"
        );
        assert!(IntelArchitecture::IrisXe.is_sycl_capable());
    }
}

#[test]
fn classifies_xe_lpg_meteor_lake() {
    assert_eq!(
        classify_intel_architecture("Intel Xe-LPG Graphics"),
        IntelArchitecture::XeLpg,
    );
}

#[test]
fn classifies_lunar_lake_explicit() {
    for name in &[
        "Intel LunarLake Graphics",
        "Intel(R) LunarLake(TM) Graphics",
        "Intel Lunar Lake Graphics",
    ] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::XeLpgPlus,
            "mis-classified: {name}"
        );
        assert!(IntelArchitecture::XeLpgPlus.is_sycl_capable());
    }
}

#[test]
fn older_integrated_is_not_sycl_capable() {
    for name in &[
        "Intel HD Graphics 630",
        "Intel UHD Graphics 770",
        "Intel HD Graphics 520",
        "Intel UHD Graphics 620",
    ] {
        let arch = classify_intel_architecture(name);
        assert_eq!(
            arch,
            IntelArchitecture::OlderIntegrated,
            "mis-classified: {name}"
        );
        assert!(!arch.is_sycl_capable(), "{name} should not be SYCL capable");
    }
}

#[test]
fn unknown_names_classified_as_unknown() {
    let arch = classify_intel_architecture("Definitely Not An Intel GPU");
    assert_eq!(arch, IntelArchitecture::Unknown);
    assert!(!arch.is_sycl_capable());

    // An empty name is also unknown.
    assert_eq!(classify_intel_architecture(""), IntelArchitecture::Unknown);
}

#[test]
fn architecture_labels_are_stable() {
    // Lock in the label strings so downstream consumers (which embed
    // them in `detail["Architecture"]`) can rely on them.
    assert_eq!(
        IntelArchitecture::Alchemist.label(),
        "Alchemist (Xe-HPG, A-series)"
    );
    assert_eq!(
        IntelArchitecture::Battlemage.label(),
        "Battlemage (Xe2, B-series)"
    );
    assert_eq!(IntelArchitecture::XeLpg.label(), "Xe-LPG (Meteor Lake)");
    assert_eq!(IntelArchitecture::XeLpgPlus.label(), "Xe-LPG+ (Lunar Lake)");
    assert_eq!(
        IntelArchitecture::IrisXe.label(),
        "Iris Xe (Tiger/Alder/Raptor Lake)"
    );
    assert_eq!(
        IntelArchitecture::OlderIntegrated.label(),
        "Pre-Xe (HD/UHD Graphics)"
    );
    assert_eq!(IntelArchitecture::Unknown.label(), "Unknown");
}

#[test]
fn sycl_capability_matches_backend_ai_go() {
    // The five SYCL-capable architectures, mirrored from
    // lablup/backend.ai-go's check_intel_sycl_support.
    assert!(IntelArchitecture::Alchemist.is_sycl_capable());
    assert!(IntelArchitecture::Battlemage.is_sycl_capable());
    assert!(IntelArchitecture::XeLpg.is_sycl_capable());
    assert!(IntelArchitecture::XeLpgPlus.is_sycl_capable());
    assert!(IntelArchitecture::IrisXe.is_sycl_capable());
    assert!(!IntelArchitecture::OlderIntegrated.is_sycl_capable());
    assert!(!IntelArchitecture::Unknown.is_sycl_capable());
}

#[test]
fn sycl_capable_label_distinguishes_unknown_from_no() {
    // The map-entry label must not collapse Unknown into "No" —
    // downstream consumers need to know whether the GPU is *known*
    // not to be SYCL-capable vs. unrecognised.
    assert_eq!(IntelArchitecture::Alchemist.sycl_capable_label(), "Yes");
    assert_eq!(IntelArchitecture::Battlemage.sycl_capable_label(), "Yes");
    assert_eq!(IntelArchitecture::XeLpg.sycl_capable_label(), "Yes");
    assert_eq!(IntelArchitecture::XeLpgPlus.sycl_capable_label(), "Yes");
    assert_eq!(IntelArchitecture::IrisXe.sycl_capable_label(), "Yes");
    assert_eq!(
        IntelArchitecture::OlderIntegrated.sycl_capable_label(),
        "No"
    );
    assert_eq!(IntelArchitecture::Unknown.sycl_capable_label(), "Unknown");
}

// ---------- Xe3 / Panther Lake (issue: Intel Arc B390) ----------
//
// Fixtures are the real strings this machine reports, so a regression
// here is a regression against actual hardware, not against a guess.

#[test]
fn classifies_panther_lake_as_xe3() {
    for name in &[
        // Verbatim from `Win32_VideoController.Name` on a Panther Lake host.
        "Intel(R) Arc(TM) B390 GPU",
        "Intel(R) Arc(TM) B370 GPU",
        "Intel Arc B390",
        "Intel Xe3 Graphics",
        "Intel Panther Lake Graphics",
        "Intel(R) PantherLake(TM) Graphics",
    ] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::Xe3,
            "mis-classified: {name}"
        );
    }
}

#[test]
fn battlemage_b_series_survives_the_xe3_rule() {
    // The Xe3 rule runs BEFORE the Battlemage rule, so this pins that it
    // only claims the Panther Lake tokens. `B380` is one digit from the
    // integrated `B390` and must stay discrete Battlemage.
    for name in &[
        "Intel Arc B580",
        "Intel(R) Arc(TM) B580 Graphics",
        "Intel Arc B570",
        "Intel Arc B380 Graphics",
        "Intel Battlemage Graphics",
    ] {
        assert_eq!(
            classify_intel_architecture(name),
            IntelArchitecture::Battlemage,
            "mis-classified: {name}"
        );
    }
}

#[test]
fn xe3_label_and_sycl_capability_are_stable() {
    assert_eq!(IntelArchitecture::Xe3.label(), "Xe3 (Panther Lake)");
    assert!(IntelArchitecture::Xe3.is_sycl_capable());
    assert_eq!(IntelArchitecture::Xe3.sycl_capable_label(), "Yes");
}

#[test]
fn an_arc_gpu_suffix_is_not_unknown() {
    // Panther Lake introduced a "GPU" suffix where every earlier
    // generation used "Graphics". An unrecognised Arc iGPU with that
    // suffix must still land in the Xe family rather than `Unknown`,
    // otherwise `SYCL Capable` reads "Unknown" for a capable device.
    assert_eq!(
        classify_intel_architecture("Intel(R) Arc(TM) GPU"),
        IntelArchitecture::XeLpg,
    );
}

#[test]
fn marketing_name_covers_panther_lake() {
    assert!(intel_gpu_marketing_name(0xB080).contains("Panther Lake"));
    assert!(intel_gpu_marketing_name(0xB08F).contains("Panther Lake"));
    // Just outside the range — must fall through to the generic form
    // rather than silently claiming a family it does not know.
    assert!(intel_gpu_marketing_name(0xB07F).is_empty());
    assert!(resolve_intel_gpu_name(0xB07F).contains("0xb07f"));
}

// ---------- Discrete vs. integrated ----------

#[test]
fn variant_prefers_the_pci_device_id() {
    // The whole point of the device-ID path: the NAME says "Arc B390",
    // which the model-number heuristic would read as a discrete B-series
    // card. The device ID is what knows better.
    assert_eq!(
        intel_variant(Some(0xB080), "Intel(R) Arc(TM) B390 GPU"),
        "Integrated"
    );
    assert_eq!(
        intel_variant(Some(0xE20B), "Intel(R) Arc(TM) B580 Graphics"),
        "Discrete"
    );
    assert_eq!(
        intel_variant(Some(0x56A0), "Intel(R) Arc(TM) A770 Graphics"),
        "Discrete"
    );
    // Meteor Lake iGPU.
    assert_eq!(
        intel_variant(Some(0x7D55), "Intel(R) Arc(TM) Graphics"),
        "Integrated"
    );
    // Upper 16 bits are ignored, as in `resolve_intel_gpu_name`.
    assert_eq!(
        intel_variant(Some(0xFFFF_56A0), "Intel Arc A770"),
        "Discrete"
    );
}

#[test]
fn variant_falls_back_to_architecture_without_an_id() {
    assert_eq!(
        intel_variant(None, "Intel(R) Arc(TM) B390 GPU"),
        "Integrated"
    );
    assert_eq!(
        intel_variant(None, "Intel(R) Arc(TM) A770 Graphics"),
        "Discrete"
    );
    assert_eq!(
        intel_variant(None, "Intel(R) Arc(TM) B580 Graphics"),
        "Discrete"
    );
    assert_eq!(
        intel_variant(None, "Intel(R) Iris(R) Xe Graphics"),
        "Integrated"
    );
    assert_eq!(
        intel_variant(None, "Intel(R) UHD Graphics 770"),
        "Integrated"
    );
    assert_eq!(
        intel_variant(None, "Intel(R) Arc(TM) Graphics"),
        "Integrated"
    );
}

#[test]
fn variant_name_heuristic_excludes_the_xe3_igpu_tokens() {
    // Reached only when the architecture is Unknown — an Arc name with a
    // model token but no recognised family. A hypothetical future
    // discrete SKU stays discrete; the known Xe3 iGPU tokens do not.
    assert_eq!(intel_variant(None, "Intel Arc B999"), "Discrete");
    assert_eq!(intel_variant(None, "Intel Arc B390"), "Integrated");
    // No `arc` token at all ⇒ integrated, unchanged from the old rule.
    assert_eq!(
        intel_variant(None, "Definitely Not An Intel GPU"),
        "Integrated"
    );
}

#[test]
fn arc_model_token_shape_is_unchanged() {
    // The shape test moved here from `intel_gpu_windows.rs`; its contract
    // is untouched. Note `b390` matches the SHAPE — form factor is
    // decided by `intel_variant`, not by this.
    assert!(is_arc_model_token("a770"));
    assert!(is_arc_model_token("b580"));
    assert!(is_arc_model_token("b390"));
    assert!(!is_arc_model_token("arc"));
    assert!(!is_arc_model_token("graphics"));
    assert!(!is_arc_model_token("a77"));
}
