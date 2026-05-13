//! Basic clamp behaviour for `clamp_recording_bitrate_kbps` (PRD R2.10).
//!
//! Asserts the pure return-value contract without observing the one-shot
//! warning side-effect. The warning is covered by the dedicated
//! `warn_low.rs` / `warn_high.rs` test binaries, each of which gets its own
//! process (and therefore its own `OnceLock` latch) thanks to `cargo test`
//! compiling each file in `tests/` as a separate binary.

use bitrate_precision_tests::{
    BITRATE_BAND_MAX_KBPS, BITRATE_BAND_MIN_KBPS, DEFAULT_RECORDING_BITRATE_KBPS, PreferencesShim,
    clamp_recording_bitrate_kbps, default_recording_bitrate_kbps,
};

#[test]
fn default_is_8000_kbps() {
    // PRD R2.10 verbatim: "target 8 Mbps ± 2 Mbps at 1080p30". The default
    // must land on 8 Mbps exactly so a fresh `config.json` (no
    // `recording_bitrate_kbps` field) produces recordings at the spec
    // target rather than the lower bound.
    assert_eq!(default_recording_bitrate_kbps(), 8_000);
    assert_eq!(DEFAULT_RECORDING_BITRATE_KBPS, 8_000);
}

#[test]
fn default_lies_inside_buyer_band() {
    // Defense-in-depth: if someone bumps DEFAULT_RECORDING_BITRATE_KBPS
    // outside the band, this fails before users see clamped recordings.
    // Wrapped in `const { ... }` so clippy doesn't flag the constant-
    // expression assertions (the values are all `const u32`, so the check
    // is statically known to clippy; we still want the assertion as
    // documentation + compile-time guarantee).
    const _: () = {
        assert!(DEFAULT_RECORDING_BITRATE_KBPS >= BITRATE_BAND_MIN_KBPS);
        assert!(DEFAULT_RECORDING_BITRATE_KBPS <= BITRATE_BAND_MAX_KBPS);
    };
}

#[test]
fn band_min_is_6000_kbps() {
    // PRD R2.10 "buyer accepts 6–12 Mbps band" — lock the lower bound at
    // 6 Mbps so a buyer-spec change is a one-line edit with a failing test.
    assert_eq!(BITRATE_BAND_MIN_KBPS, 6_000);
}

#[test]
fn band_max_is_12000_kbps() {
    // PRD R2.10 "buyer accepts 6–12 Mbps band" — lock the upper bound at
    // 12 Mbps so a buyer-spec change is a one-line edit with a failing test.
    assert_eq!(BITRATE_BAND_MAX_KBPS, 12_000);
}

#[test]
fn passthrough_inside_band_does_not_clamp() {
    // 6500 sits strictly inside [6000, 12000] — should round-trip untouched.
    // This is the happy-path case: user picks any value in band, encoder
    // gets exactly that value, no warning fires.
    let out = clamp_recording_bitrate_kbps(6_500);
    assert_eq!(out, 6_500);
}

#[test]
fn passthrough_at_lower_boundary_does_not_clamp() {
    // Boundary value 6000 is INCLUSIVE — exactly the min is in-band.
    let out = clamp_recording_bitrate_kbps(BITRATE_BAND_MIN_KBPS);
    assert_eq!(out, BITRATE_BAND_MIN_KBPS);
}

#[test]
fn passthrough_at_upper_boundary_does_not_clamp() {
    // Boundary value 12000 is INCLUSIVE — exactly the max is in-band.
    let out = clamp_recording_bitrate_kbps(BITRATE_BAND_MAX_KBPS);
    assert_eq!(out, BITRATE_BAND_MAX_KBPS);
}

#[test]
fn passthrough_at_default_does_not_clamp() {
    // The 8 Mbps default itself must always be in band — sanity check the
    // contract that `default()` produces a value the clamp accepts.
    let out = clamp_recording_bitrate_kbps(DEFAULT_RECORDING_BITRATE_KBPS);
    assert_eq!(out, DEFAULT_RECORDING_BITRATE_KBPS);
}

#[test]
fn clamp_5000_returns_6000() {
    // Below-band value — clamps UP to the lower bound. Warning side-effect
    // is exercised by the `warn_low.rs` test binary; this test only checks
    // the return value so it stays stable regardless of OnceLock state.
    let out = clamp_recording_bitrate_kbps(5_000);
    assert_eq!(out, BITRATE_BAND_MIN_KBPS);
}

#[test]
fn clamp_15000_returns_12000() {
    // Above-band value — clamps DOWN to the upper bound. Warning
    // side-effect covered by `warn_high.rs`.
    let out = clamp_recording_bitrate_kbps(15_000);
    assert_eq!(out, BITRATE_BAND_MAX_KBPS);
}

#[test]
fn clamp_zero_returns_min() {
    // Pathological "I set it to zero" case — clamps up to 6000 rather than
    // sending 0 to the encoder (which OBS would silently treat as "use
    // encoder default", masking the misconfiguration).
    let out = clamp_recording_bitrate_kbps(0);
    assert_eq!(out, BITRATE_BAND_MIN_KBPS);
}

#[test]
fn clamp_u32_max_returns_band_max() {
    // Worst-case overflow guard — u32::MAX is far above the band; must
    // clamp down rather than panic on widening to i64 in the encoder
    // wiring. (i64::from(u32) always fits, but this also documents that
    // the encoder will never see anything above 12000.)
    let out = clamp_recording_bitrate_kbps(u32::MAX);
    assert_eq!(out, BITRATE_BAND_MAX_KBPS);
}

#[test]
fn shim_default_uses_8000() {
    // Constructs `PreferencesShim::default()` (the analogue of
    // `Preferences::default()` in the production crate) and verifies the
    // field lands on 8000. Locks in the wiring between
    // `default_recording_bitrate_kbps` and the `Preferences` default impl
    // so a future refactor that breaks one but not the other is caught.
    let prefs = PreferencesShim::default();
    assert_eq!(prefs.recording_bitrate_kbps, 8_000);
}

#[test]
fn backward_compat_missing_field_deserializes_to_default() {
    // PRD R2.10 backward-compat contract: a `config.json` written by a
    // previous build (no `recording_bitrate_kbps` field at all) must parse
    // cleanly and pick up the 8 Mbps default — NOT 0 or fail-to-parse.
    //
    // This is the contract that `#[serde(default = "...")]` on the field
    // provides, but we lock it down with a test because the JSON shape
    // here is exactly what users with older configs have on disk.
    let legacy_json = r#"{}"#;
    let parsed: PreferencesShim = serde_json::from_str(legacy_json)
        .expect("legacy config.json with no bitrate field must parse");
    assert_eq!(parsed.recording_bitrate_kbps, 8_000);
}

#[test]
fn backward_compat_unrelated_fields_parse_cleanly() {
    // Same as above but with an unrelated camelCase field present —
    // exercises `serde(rename_all = "camelCase")` interaction with the
    // default-on-missing-field path. (Real `config.json` files have many
    // other fields; the shim only knows about `recording_bitrate_kbps`,
    // and serde's default behaviour is to ignore unknown fields.)
    let legacy_json = r#"{"someOtherField": 42, "stillIgnored": "hi"}"#;
    let parsed: PreferencesShim =
        serde_json::from_str(legacy_json).expect("config with unrelated fields must parse");
    assert_eq!(parsed.recording_bitrate_kbps, 8_000);
}

#[test]
fn forward_value_overrides_default() {
    // Positive control for the previous two tests: when the field IS
    // present, its value (not the default) wins. Belt-and-suspenders to
    // catch a future `#[serde(default)]` typo that would silently always
    // return the default and never honour the user's stored choice.
    let json = r#"{"recordingBitrateKbps": 9500}"#;
    let parsed: PreferencesShim = serde_json::from_str(json).expect("must parse");
    assert_eq!(parsed.recording_bitrate_kbps, 9_500);
}
