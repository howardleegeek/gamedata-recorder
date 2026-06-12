//! Heuristics shared across profiles.
//!
//! Anything specific to one title lives in that title's module
//! (e.g. `cyberpunk2077.rs`). Anything that applies to "most modern AAA
//! DX12 titles with reversed-Z and infinite far plane" lives here.
//!
//! The functions here are pure and cfg-independent so they are unit
//! testable on Mac without a D3D runtime.

use crate::types::Matrix4;

/// Extract (near, far) from a reverse-Z infinite-far DirectX projection.
///
/// Modern AAA engines (Cyberpunk 2077's REDengine 4, Unreal 5's Lumen,
/// DOOM Eternal's idTech 7, …) use reversed-Z with infinite far plane.
/// In column-major DirectX convention that matrix looks like:
///
/// ```text
///     [ sx   0     0     0 ]
///     [ 0    sy    0     0 ]
///     [ 0    0     0     n ]
///     [ 0    0    -1     0 ]
/// ```
///
/// - `sx = 1 / (aspect * tan(fov/2))`
/// - `sy = 1 / tan(fov/2)`
/// - `n`  = near plane. Appears at element `(col=3, row=2)` in
///   column-major storage. Far plane is infinity.
///
/// References:
/// - NVIDIA: "Depth Precision Visualized" (2015) on why AAA engines
///   flipped to reversed-Z with infinite far.
/// - Emil Persson / Humus: reversed-Z derivation, 2007.
/// - Microsoft D3D12 sample `D3D12Multithreading` uses this exact layout.
///
/// Returns `(near, far)` with `far = f32::INFINITY` when the matrix
/// shape matches. Returns `(0.1, 1000.0)` as a safe fallback if the
/// bottom row looks like a standard perspective matrix — profiles can
/// override [`crate::profiles::DepthHookProfile::near_far_from_matrix`]
/// when they don't fit this shape.
pub fn reverse_z_infinite_far_near(proj: &Matrix4) -> (f32, f32) {
    // Column-major accessor: element at column 3, row 2 holds `near`
    // in the reverse-Z infinite-far layout above.
    let near_candidate = proj.get(3, 2);

    // Bottom row's element at column 2 is -1 for an infinite-far reversed-Z
    // matrix. If we see something else (e.g. `-far/(far-near)` for a
    // standard perspective), the caller's assumption was wrong and we
    // should not pretend to extract near/far.
    let bottom_row_col2 = proj.get(2, 3);

    if (bottom_row_col2 + 1.0).abs() < 1e-4 && near_candidate.is_finite() && near_candidate > 0.0 {
        (near_candidate, f32::INFINITY)
    } else {
        // Safe fallback; the profile should usually detect this and
        // override `near_far_from_matrix` with its own extractor.
        (0.1, 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_z_infinite_far_extracts_near() {
        // Construct a reversed-Z infinite-far projection with near = 0.1
        let mut m = [0.0f32; 16];
        // column 0
        m[0] = 1.2; // sx
        // column 1
        m[5] = 1.7; // sy
        // column 2 (row 3 is the -1 for infinite-far reversed-Z)
        m[11] = -1.0;
        // column 3 (row 2 holds near)
        m[14] = 0.1;
        let proj = Matrix4 { m };

        let (near, far) = reverse_z_infinite_far_near(&proj);
        assert!((near - 0.1).abs() < 1e-6, "near extracted wrong: {near}");
        assert!(far.is_infinite(), "far should be infinity, was {far}");
    }

    #[test]
    fn non_reverse_z_matrix_falls_back() {
        // Identity isn't a valid reverse-Z matrix — bottom-row (2,3) is 0,
        // not -1. Should fall back to sane defaults instead of returning
        // garbage.
        let (near, far) = reverse_z_infinite_far_near(&Matrix4::IDENTITY);
        assert_eq!(near, 0.1);
        assert_eq!(far, 1000.0);
    }
}
