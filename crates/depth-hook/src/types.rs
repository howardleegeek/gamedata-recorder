//! Platform-agnostic data types used by every profile and by the DX12 hook.
//!
//! Everything in this file must compile on macOS, Linux, and Windows so that
//! the profile registry stays unit-testable on a Mac developer box. No
//! Windows-specific imports belong here — those go in `dx12::`.

use serde::{Deserialize, Serialize};

/// 4×4 matrix in column-major layout (matches DirectX / HLSL convention).
///
/// Stored as a flat 16-element `f32` array on purpose: no `cgmath` / `glam`
/// dependency pulled in by a scaffold crate, and GPU command buffers hand
/// matrices to us as `[f32; 16]` anyway.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Matrix4 {
    /// Raw 16-element column-major storage. Element `m[col * 4 + row]`
    /// gives the value at `(col, row)`. Prefer [`Matrix4::get`] in hot
    /// paths to keep the indexing convention obvious at call sites.
    pub m: [f32; 16],
}

impl Matrix4 {
    /// Identity matrix, useful for default-constructed `CameraMatrices`.
    pub const IDENTITY: Self = Self {
        m: [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0, //
        ],
    };

    /// Column-major accessor: `(col, row)`.
    #[inline]
    pub fn get(&self, col: usize, row: usize) -> f32 {
        debug_assert!(col < 4 && row < 4);
        self.m[col * 4 + row]
    }
}

impl Default for Matrix4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Depth-buffer pixel format, mirroring the subset of DXGI_FORMAT we actually
/// see in practice. Keeping this as a typed enum (not a raw `u32`) forces
/// profile authors to declare what they expect, and lets the capture layer
/// reject swapchains with a mismatched format instead of silently shipping
/// garbage bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepthFormat {
    /// `DXGI_FORMAT_D32_FLOAT`. Most modern AAA titles (Cyberpunk 2077,
    /// Alan Wake 2, Starfield). 32-bit float depth, no stencil.
    D32Float,
    /// `DXGI_FORMAT_D24_UNORM_S8_UINT`. Classic 24-bit depth + 8-bit stencil.
    D24UnormS8Uint,
    /// `DXGI_FORMAT_D32_FLOAT_S8X24_UINT`. Float depth + stencil; seen in
    /// some Unreal Engine titles with advanced stencil effects.
    D32FloatS8X24Uint,
    /// `DXGI_FORMAT_D16_UNORM`. Rare in modern titles, included for
    /// completeness (old RE engine games, some indie DX11 upconverts).
    D16Unorm,
}

/// Which heuristic the capture layer should use to pick the canonical depth
/// buffer out of the many RTV/DSVs a modern renderer binds per frame.
///
/// The strategy is borrowed from the ReShade Generic Depth addon
/// (<https://reshade.me/forum/generic-depth-addon>, BSD-licensed): modern
/// renderers clear the canonical scene-depth buffer exactly once per frame,
/// at the camera's aspect ratio, and pile the highest draw-call count onto
/// it. Post-process depth copies, shadow maps, decal masks, and downsampled
/// mip chains all fail one of those checks. That addon has been validated
/// on hundreds of titles including Cyberpunk 2077, so we inherit its
/// heuristic shape here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DetectionHeuristic {
    /// The target aspect ratio (width / height). 16:9 = `16.0 / 9.0`.
    /// The picker accepts anything within [`aspect_tolerance`] of this.
    pub aspect_ratio: f32,
    /// Absolute tolerance around `aspect_ratio`. A typical value is `0.05`
    /// so 16:9 (`1.777…`), 16:10 (`1.6`), and ultrawide 21:9 (`2.333…`) are
    /// discriminated, but a 1919×1080 viewport (`1.776…`) still matches 16:9.
    pub aspect_tolerance: f32,
    /// Expected number of `ClearDepthStencilView` calls per frame for the
    /// canonical depth buffer. Almost always exactly 1 in modern engines.
    /// We check equality against this — shadow atlases clear many times.
    pub expected_clears_per_frame: u32,
    /// Require the format to be typed depth. If false, the picker will
    /// accept typeless formats that alias as depth (rare, mostly legacy).
    pub require_typed_depth: bool,
    /// If multiple candidates survive every other check, prefer the one
    /// with the highest draw-call count. This is the ReShade tiebreaker.
    pub prefer_highest_draw_count: bool,
}

impl DetectionHeuristic {
    /// Canonical 16:9 FHD/QHD/4K heuristic. Works for every modern AAA title
    /// we've profiled (Cyberpunk 2077, Alan Wake 2, Starfield, Wukong).
    pub const WIDESCREEN_16_9: Self = Self {
        aspect_ratio: 16.0 / 9.0,
        aspect_tolerance: 0.05,
        expected_clears_per_frame: 1,
        require_typed_depth: true,
        prefer_highest_draw_count: true,
    };
}

/// A single captured depth frame with the camera it was rendered from.
///
/// Emitted by the (future) DX12 hook to the recorder; the recorder packs it
/// alongside the `recording.mp4` video track with the frame index, so the
/// training pipeline can align pixel (u, v) + depth(u, v) + camera matrices
/// into world-space 3D points.
#[derive(Debug, Clone)]
pub struct DepthFrame {
    /// Index of the color frame this depth matches. Matches the `idx` field
    /// in `frames.jsonl` (see `constants::filename::recording::FRAMES_JSONL`).
    pub frame_index: u64,
    /// Nanoseconds since recording start. Same clock as `input.jsonl`.
    pub timestamp_ns: u64,
    /// Width of the depth buffer in pixels. Typically matches the render
    /// target (not necessarily the swapchain — DLSS / FSR titles render
    /// lower and upscale).
    pub width: u32,
    /// Height of the depth buffer in pixels.
    pub height: u32,
    /// Raw depth bytes, in the format declared by the profile
    /// (`DepthFormat`). Length = `width * height * bytes_per_pixel`.
    pub pixels: Vec<u8>,
    /// Camera matrices at the moment this depth was rasterised. Needed to
    /// unproject (u, v, depth) back into world-space.
    pub camera: CameraMatrices,
}

/// Camera state for a single frame.
///
/// View and projection are kept separate (instead of being pre-multiplied)
/// because downstream tooling commonly needs them individually — view for
/// computing camera pose, projection for unprojecting depth.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CameraMatrices {
    /// World → view (camera space) transform.
    pub view: Matrix4,
    /// View → clip-space projection. For reverse-Z infinite-far titles (see
    /// `DepthHookProfile::reverse_z`), the projection matrix is built with
    /// `[0, 1]` depth mapped from far→near, not the OpenGL `[-1, 1]`
    /// near→far convention.
    pub projection: Matrix4,
    /// Near plane used when the profile derived it from `projection`. For
    /// reverse-Z infinite-far setups, `far == f32::INFINITY` and only `near`
    /// is meaningful.
    pub near: f32,
    /// Far plane. `f32::INFINITY` for reverse-Z infinite-far titles.
    pub far: f32,
}

impl Default for CameraMatrices {
    fn default() -> Self {
        Self {
            view: Matrix4::IDENTITY,
            projection: Matrix4::IDENTITY,
            near: 0.1,
            far: 1000.0,
        }
    }
}
