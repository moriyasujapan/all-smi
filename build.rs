// Copyright 2025 Lablup Inc.
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_level_zero_cfg();

    // Only compile proto files on Linux (TPU is Linux-only)
    //
    // NOTE: `#[cfg(target_os)]` in a build script describes the *host*,
    // not the build target. That is benign here (a Linux host is the only
    // one that cross-compiles the TPU protos anyway) but it is the wrong
    // idiom in general — see `emit_level_zero_cfg` for the correct form.
    #[cfg(target_os = "linux")]
    {
        let proto_file = "proto/tpu_metric_service.proto";

        // Check if proto file exists before trying to compile
        if std::path::Path::new(proto_file).exists() {
            let include_paths = ["proto/", "/usr/include"];
            tonic_prost_build::configure()
                .build_server(false) // We only need the client
                .protoc_arg("--experimental_allow_proto3_optional")
                // Suppress clippy warnings on generated protobuf code
                .type_attribute(".", "#[allow(clippy::enum_variant_names)]")
                .compile_protos(&[proto_file], &include_paths)?;
        }
    }

    Ok(())
}

/// Emit the `all_smi_level_zero` cfg alias.
///
/// The Intel Level Zero backend is opt-in on Linux and macOS (via the
/// `level_zero` cargo feature) but **always compiled on Windows**: it
/// pulls in no extra crates — it `dlopen`s `ze_loader.dll` through
/// `libloading`, already an unconditional Windows dependency — and that
/// loader ships with the Intel graphics driver. Without it, Windows hosts
/// get no GPU temperature, power, or frequency at all.
///
/// Cargo cannot express "this feature defaults on for one target", hence
/// the cfg alias. Every consumer then writes a single uniform
/// `#[cfg(all_smi_level_zero)]` instead of repeating the disjunction.
///
/// Note `CARGO_CFG_TARGET_OS`, not `#[cfg(target_os = ...)]`: inside a
/// build script the latter describes the *host*, which would silently do
/// the wrong thing when cross-compiling to Windows from Linux (as the
/// `cargo xwin` check and the release workflow both do).
fn emit_level_zero_cfg() {
    println!("cargo::rustc-check-cfg=cfg(all_smi_level_zero)");

    let targets_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let feature_requested = std::env::var_os("CARGO_FEATURE_LEVEL_ZERO").is_some();
    if targets_windows || feature_requested {
        println!("cargo::rustc-cfg=all_smi_level_zero");
    }
}
