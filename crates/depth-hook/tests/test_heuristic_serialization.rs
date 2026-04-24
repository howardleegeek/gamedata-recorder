//! Serialisation round-trip tests for types that will eventually live in
//! config files.
//!
//! The detection heuristic is the thing we most expect to tune per-user
//! and per-game-version (every time CD Projekt ships a CP2077 patch, the
//! expected clear count could change). So we want it to round-trip
//! through JSON cleanly today, before anyone starts wiring up a config
//! surface around it.

use depth_hook::{DepthFormat, DetectionHeuristic};

#[test]
fn detection_heuristic_roundtrips_through_json() {
    let original = DetectionHeuristic::WIDESCREEN_16_9;
    let json = serde_json::to_string(&original).expect("serialise");
    let decoded: DetectionHeuristic = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(original, decoded);
}

#[test]
fn detection_heuristic_json_is_human_editable() {
    // Config surfaces will want to let users hand-edit these files, so
    // the field names must be stable and descriptive. If this test
    // regresses (field renamed / removed) any committed config file
    // shipped in the wild stops parsing — treat that as a breaking
    // change and bump the profile-file schema version accordingly.
    let json = serde_json::to_value(DetectionHeuristic::WIDESCREEN_16_9).unwrap();
    let obj = json.as_object().expect("top-level should be an object");
    assert!(obj.contains_key("aspect_ratio"));
    assert!(obj.contains_key("aspect_tolerance"));
    assert!(obj.contains_key("expected_clears_per_frame"));
    assert!(obj.contains_key("require_typed_depth"));
    assert!(obj.contains_key("prefer_highest_draw_count"));
}

#[test]
fn depth_format_roundtrips_through_json() {
    for fmt in [
        DepthFormat::D32Float,
        DepthFormat::D24UnormS8Uint,
        DepthFormat::D32FloatS8X24Uint,
        DepthFormat::D16Unorm,
    ] {
        let json = serde_json::to_string(&fmt).expect("serialise");
        let decoded: DepthFormat = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(fmt, decoded, "round-trip failed for {fmt:?}");
    }
}
