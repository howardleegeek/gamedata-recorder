//! Asserts the on-disk `metadata.json` wire shape for `encoder_bitrate_kbps`
//! (PRD R2.10). The buyer ingest reads this field back to audit per-session
//! bitrate against the 6–12 Mbps band; the contract is "bare JSON int, not
//! a string, snake_case key, omitted when None for backward compat".

use bitrate_precision_tests::MetadataShim;
use serde_json::Value;

#[test]
fn encoder_bitrate_kbps_serializes_as_int() {
    let meta = MetadataShim {
        game_exe: "hl2.exe".to_string(),
        encoder_bitrate_kbps: Some(8_000),
    };
    let json = serde_json::to_string(&meta).expect("serialize");
    let parsed: Value = serde_json::from_str(&json).expect("parse back");

    let field = parsed
        .get("encoder_bitrate_kbps")
        .expect("encoder_bitrate_kbps must be present when Some(_)");
    assert!(
        field.is_u64() || field.is_i64(),
        "encoder_bitrate_kbps must serialize as a JSON int, got: {field:?}"
    );
    assert_eq!(
        field.as_u64(),
        Some(8_000),
        "encoder_bitrate_kbps value must round-trip exactly"
    );
}

#[test]
fn encoder_bitrate_kbps_uses_snake_case_key() {
    // Wire contract: the on-disk JSON key is `encoder_bitrate_kbps`
    // (snake_case), matching `output_types/mod.rs`'s `#[serde(rename_all =
    // "snake_case")]` (when applied) and the existing camel-snake-mixed
    // metadata convention. Buyer-side parsers expect this exact key.
    let meta = MetadataShim {
        game_exe: "hl2.exe".to_string(),
        encoder_bitrate_kbps: Some(8_000),
    };
    let json = serde_json::to_string(&meta).expect("serialize");
    assert!(
        json.contains("\"encoder_bitrate_kbps\""),
        "JSON must contain snake_case key, got: {json}"
    );
    assert!(
        !json.contains("encoderBitrateKbps"),
        "JSON must NOT contain camelCase key, got: {json}"
    );
}

#[test]
fn none_value_omits_field_for_backward_compat() {
    // Backward-compat contract: when `encoder_bitrate_kbps` is `None`
    // (legacy recordings from before R2.10 landed), the field must be
    // entirely absent from the serialized JSON — not `null`. This lets
    // legacy parsers (which don't know about the field) ignore it
    // cleanly, and `null`-vs-missing-vs-int parser quirks don't trip.
    let meta = MetadataShim {
        game_exe: "hl2.exe".to_string(),
        encoder_bitrate_kbps: None,
    };
    let json = serde_json::to_string(&meta).expect("serialize");
    let parsed: Value = serde_json::from_str(&json).expect("parse back");
    assert!(
        parsed.get("encoder_bitrate_kbps").is_none(),
        "None must serialize as field-absent (not null), got: {json}"
    );
}

#[test]
fn legacy_metadata_without_field_parses_as_none() {
    // Inverse of `none_value_omits_field_for_backward_compat`: a legacy
    // `metadata.json` (one generated before R2.10 added the field)
    // must parse cleanly with `encoder_bitrate_kbps == None`. Without
    // `#[serde(default)]`, this would error.
    let legacy_json = r#"{"game_exe":"hl2.exe"}"#;
    let parsed: MetadataShim =
        serde_json::from_str(legacy_json).expect("legacy metadata.json must parse");
    assert_eq!(parsed.encoder_bitrate_kbps, None);
    assert_eq!(parsed.game_exe, "hl2.exe");
}

#[test]
fn round_trip_preserves_value() {
    // End-to-end wire contract: serialize then deserialize must give back
    // exactly the same Option<u32>. Guards against a future
    // `#[serde(serialize_with = "...")]` / `deserialize_with = "..."`
    // typo that produces lossy round-trips.
    for sample in [None, Some(6_000_u32), Some(8_000), Some(12_000)] {
        let meta = MetadataShim {
            game_exe: "hl2.exe".to_string(),
            encoder_bitrate_kbps: sample,
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let back: MetadataShim = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.encoder_bitrate_kbps, sample,
            "round-trip failed for {sample:?} via JSON {json}"
        );
    }
}
