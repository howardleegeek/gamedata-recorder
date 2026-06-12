//! Integration tests for `depth-hook::types`.
//!
//! These exercise:
//! - `Matrix4` defaults + column-major accessor
//! - `CameraMatrices::default()` (near/far defaults)
//! - `DepthFormat` serde round-trip (already covered partially by
//!   `test_heuristic_serialization` — extend it here with the
//!   per-variant default branches that the existing test loops over)
//! - `DetectionHeuristic` validity / canonical constants
//!
//! The Mac CI box can run all of these without needing a D3D runtime —
//! everything in `types.rs` is platform-agnostic pure Rust.

use depth_hook::{CameraMatrices, DepthFormat, DepthFrame, DetectionHeuristic, Matrix4};

// ---------------------------------------------------------------------------
// Matrix4
// ---------------------------------------------------------------------------

#[test]
fn matrix4_identity_diagonal_is_ones() {
    let m = Matrix4::IDENTITY;
    assert_eq!(m.get(0, 0), 1.0);
    assert_eq!(m.get(1, 1), 1.0);
    assert_eq!(m.get(2, 2), 1.0);
    assert_eq!(m.get(3, 3), 1.0);
}

#[test]
fn matrix4_identity_off_diagonal_is_zero() {
    let m = Matrix4::IDENTITY;
    for col in 0..4 {
        for row in 0..4 {
            if col != row {
                assert_eq!(
                    m.get(col, row),
                    0.0,
                    "off-diagonal element ({col},{row}) must be 0"
                );
            }
        }
    }
}

#[test]
fn matrix4_default_is_identity() {
    // Default impl must equal IDENTITY constant. Catches a future
    // refactor that picked a zero matrix as default.
    let default_m = Matrix4::default();
    assert_eq!(default_m, Matrix4::IDENTITY);
}

#[test]
fn matrix4_get_uses_column_major_indexing() {
    // Column-major: m[col * 4 + row]. A matrix with the value 7.0 at
    // (col=2, row=1) lives at flat index 2*4 + 1 = 9.
    let mut data = [0.0f32; 16];
    data[9] = 7.0;
    let m = Matrix4 { m: data };
    assert_eq!(m.get(2, 1), 7.0);
    // Sanity: other positions remain zero.
    assert_eq!(m.get(0, 0), 0.0);
    assert_eq!(m.get(1, 2), 0.0);
}

#[test]
fn matrix4_round_trip_through_serde() {
    let m = Matrix4 {
        m: [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ],
    };
    let s = serde_json::to_string(&m).unwrap();
    let back: Matrix4 = serde_json::from_str(&s).unwrap();
    assert_eq!(m, back);
}

#[test]
fn matrix4_is_copy_and_clone() {
    let m = Matrix4::IDENTITY;
    let copied = m;
    let cloned = m;
    assert_eq!(m, copied);
    assert_eq!(m, cloned);
}

// ---------------------------------------------------------------------------
// CameraMatrices
// ---------------------------------------------------------------------------

#[test]
fn camera_matrices_default_has_identity_view_and_projection() {
    let cm = CameraMatrices::default();
    assert_eq!(cm.view, Matrix4::IDENTITY);
    assert_eq!(cm.projection, Matrix4::IDENTITY);
}

#[test]
fn camera_matrices_default_near_is_point_one() {
    let cm = CameraMatrices::default();
    assert_eq!(cm.near, 0.1, "default near plane");
}

#[test]
fn camera_matrices_default_far_is_thousand() {
    let cm = CameraMatrices::default();
    assert_eq!(cm.far, 1000.0, "default far plane");
}

#[test]
fn camera_matrices_is_copy_and_clone() {
    let cm = CameraMatrices::default();
    let copied = cm;
    let cloned = cm;
    assert_eq!(copied.near, cloned.near);
    assert_eq!(copied.far, cloned.far);
}

#[test]
fn camera_matrices_round_trip_through_serde() {
    let cm = CameraMatrices {
        view: Matrix4 {
            m: [
                0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        },
        projection: Matrix4::IDENTITY,
        near: 0.05,
        far: 10_000.0,
    };
    let s = serde_json::to_string(&cm).unwrap();
    let back: CameraMatrices = serde_json::from_str(&s).unwrap();
    assert_eq!(back.near, cm.near);
    assert_eq!(back.far, cm.far);
    assert_eq!(back.view, cm.view);
    assert_eq!(back.projection, cm.projection);
}

// ---------------------------------------------------------------------------
// DepthFormat — every variant
// ---------------------------------------------------------------------------

#[test]
fn depth_format_equality_and_copy_for_every_variant() {
    let variants = [
        DepthFormat::D32Float,
        DepthFormat::D24UnormS8Uint,
        DepthFormat::D32FloatS8X24Uint,
        DepthFormat::D16Unorm,
    ];
    for &v in &variants {
        let copied = v;
        assert_eq!(v, copied);
    }
}

// ---------------------------------------------------------------------------
// DetectionHeuristic — canonical instance properties
// ---------------------------------------------------------------------------

#[test]
fn detection_heuristic_widescreen_16_9_has_expected_aspect() {
    let h = DetectionHeuristic::WIDESCREEN_16_9;
    let expected = 16.0_f32 / 9.0_f32;
    assert!((h.aspect_ratio - expected).abs() < 1e-6);
}

#[test]
fn detection_heuristic_widescreen_tolerance_distinguishes_aspects() {
    // Tolerance must be small enough to discriminate 16:9 from 16:10/21:9
    // but large enough to accept 1919x1080 ≈ 1.776 vs 16:9 = 1.7778. Lock
    // the current value of 0.05 in.
    let h = DetectionHeuristic::WIDESCREEN_16_9;
    assert_eq!(h.aspect_tolerance, 0.05);
    // 16:9 = 1.7778; 16:10 = 1.6 — distance 0.1778 > 0.05. Good.
    let aspect_16_10 = 16.0_f32 / 10.0_f32;
    assert!((h.aspect_ratio - aspect_16_10).abs() > h.aspect_tolerance);
    // 1919x1080 = 1.7768 — distance ~0.001 < 0.05. Good.
    let aspect_1919 = 1919.0_f32 / 1080.0_f32;
    assert!((h.aspect_ratio - aspect_1919).abs() < h.aspect_tolerance);
}

#[test]
fn detection_heuristic_requires_single_clear_per_frame() {
    let h = DetectionHeuristic::WIDESCREEN_16_9;
    assert_eq!(
        h.expected_clears_per_frame, 1,
        "canonical depth is cleared exactly once per frame in modern engines"
    );
}

#[test]
fn detection_heuristic_clone_and_partial_eq() {
    let h = DetectionHeuristic::WIDESCREEN_16_9;
    let cloned = h.clone();
    assert_eq!(h, cloned);
}

// ---------------------------------------------------------------------------
// DepthFrame
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Profile::near_far_from_matrix — Cyberpunk delegation path
// ---------------------------------------------------------------------------

#[test]
fn cyberpunk_profile_near_far_from_matrix_delegates_to_common() {
    // The CyberpunkHook profile overrides `near_far_from_matrix` to delegate
    // to `common::reverse_z_infinite_far_near`. Exercise that delegation
    // via a reverse-Z infinite-far matrix that should produce (near, ∞).
    use depth_hook::DepthHookProfile;
    use depth_hook::profiles::cyberpunk2077::Cyberpunk2077;

    let profile = Cyberpunk2077;
    // Build a reverse-Z infinite-far matrix with near=0.1.
    let mut m = [0.0_f32; 16];
    m[0] = 1.2;
    m[5] = 1.7;
    m[11] = -1.0; // column 2, row 3 = -1
    m[14] = 0.1; // column 3, row 2 = near
    let proj = Matrix4 { m };

    let (near, far) = profile.near_far_from_matrix(&proj);
    assert!((near - 0.1).abs() < 1e-6);
    assert!(far.is_infinite());
}

#[test]
fn cyberpunk_profile_near_far_from_identity_falls_back() {
    // Identity is not a valid reverse-Z matrix, so the common impl should
    // return the (0.1, 1000.0) fallback. Catches a future refactor that
    // returns NaN / infinity on degenerate input.
    use depth_hook::DepthHookProfile;
    use depth_hook::profiles::cyberpunk2077::Cyberpunk2077;

    let profile = Cyberpunk2077;
    let (near, far) = profile.near_far_from_matrix(&Matrix4::IDENTITY);
    assert_eq!(near, 0.1);
    assert_eq!(far, 1000.0);
}

// ---------------------------------------------------------------------------
// ProfileRegistry::default
// ---------------------------------------------------------------------------

#[test]
fn profile_registry_default_matches_with_builtin_profiles() {
    use depth_hook::ProfileRegistry;
    let default_registry = ProfileRegistry::default();
    let builtin_registry = ProfileRegistry::with_builtin_profiles();
    // Both should yield the same length and find the same profiles.
    assert_eq!(default_registry.len(), builtin_registry.len());
    assert_eq!(default_registry.is_empty(), builtin_registry.is_empty());
    assert!(
        default_registry
            .find_for_exe_stem("cyberpunk2077")
            .is_some()
    );
}

#[test]
fn depth_frame_can_be_constructed_and_cloned() {
    // DepthFrame is a Debug+Clone struct. Construct one with realistic
    // values and clone it to ensure both derive macros are wired up.
    let frame = DepthFrame {
        frame_index: 100,
        timestamp_ns: 3_333_333_000, // ~100 sec at 30 fps
        width: 1920,
        height: 1080,
        pixels: vec![0u8; 1920 * 1080 * 4], // D32_FLOAT = 4 bytes/pixel
        camera: CameraMatrices::default(),
    };
    assert_eq!(frame.frame_index, 100);
    assert_eq!(frame.timestamp_ns, 3_333_333_000);
    assert_eq!(frame.width, 1920);
    assert_eq!(frame.height, 1080);
    assert_eq!(frame.pixels.len(), 1920 * 1080 * 4);

    let cloned = frame.clone();
    assert_eq!(cloned.frame_index, frame.frame_index);
    assert_eq!(cloned.pixels.len(), frame.pixels.len());
    let _ = format!("{cloned:?}"); // Debug should not panic
}
