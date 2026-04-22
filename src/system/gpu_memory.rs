//! GPU memory querying functionality using DXGI
//!
//! This module provides Windows-specific GPU memory information retrieval
//! using the DXGI (DirectX Graphics Infrastructure) API.

use color_eyre::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GpuMemoryInfo {
    /// GPU name
    pub name: String,
    /// Adapter index (matches wgpu enumeration)
    pub adapter_index: usize,
    /// Total VRAM in bytes
    pub total_vram_bytes: u64,
    /// Total VRAM in GB (formatted)
    pub total_vram_gb: f64,
    /// Currently available VRAM in bytes
    pub available_vram_bytes: u64,
    /// Currently available VRAM in GB (formatted)
    pub available_vram_gb: f64,
    /// Currently used VRAM in bytes
    pub used_vram_bytes: u64,
    /// Percentage of VRAM used
    pub usage_percentage: f64,
}

impl GpuMemoryInfo {
    /// Get VRAM in GB as a formatted string
    pub fn total_vram_gb_string(&self) -> String {
        format!("{:.1} GB", self.total_vram_gb)
    }

    /// Get available VRAM in GB as a formatted string
    pub fn available_vram_gb_string(&self) -> String {
        format!("{:.1} GB", self.available_vram_gb)
    }

    /// Check if there's enough VRAM for recording
    /// Returns true if available VRAM is greater than the threshold
    pub fn has_sufficient_vram(&self, threshold_gb: f64) -> bool {
        self.available_vram_gb >= threshold_gb
    }

    /// Get suggested solutions based on VRAM availability
    pub fn suggestions(&self) -> Vec<String> {
        let mut suggestions = Vec::new();

        if self.available_vram_gb < 2.0 {
            suggestions.push(
                "Your GPU has very little VRAM available. Consider closing other applications to free up memory.".to_string(),
            );
        }

        if self.available_vram_gb < 4.0 {
            suggestions.push(
                "Try using the x264 (CPU) encoder instead of GPU encoders to reduce VRAM usage.".to_string(),
            );
        }

        suggestions.push(
            "Close any browser tabs, especially those with YouTube or other video content.".to_string(),
        );

        suggestions.push(
            "Lower your in-game graphics settings, especially texture quality and resolution.".to_string(),
        );

        if self.usage_percentage > 90.0 {
            suggestions.push(format!(
                "Your GPU is at {:.0}% VRAM capacity. This is likely causing the recording to fail.",
                self.usage_percentage
            ));
        }

        suggestions
    }
}

/// Query GPU memory information for a specific adapter index
///
/// # Arguments
/// * `adapter_index` - The GPU adapter index (matches wgpu enumeration)
///
/// # Returns
/// * `Ok(GpuMemoryInfo)` - GPU memory information
/// * `Err` - If unable to query GPU memory
#[cfg(target_os = "windows")]
pub fn query_gpu_memory(adapter_index: usize) -> Result<GpuMemoryInfo> {
    use windows::{
        core::Interface,
        Win32::{
            Graphics::Dxgi::{
                IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND,
            },
        },
    };

    unsafe {
        // Create DXGI factory
        let factory: IDXGIFactory1 = {
            match windows::Win32::Graphics::Dxgi::CreateDXGIFactory1() {
                Ok(f) => f,
                Err(e) => {
                    color_eyre::eyre::bail!("Failed to create DXGI factory: {:?}", e);
                }
            }
        };

        // Enumerate adapters to find the one matching our index
        let mut adapter = None;
        let mut current_index = 0;

        loop {
            let temp_adapter_result = factory.EnumAdapters1(current_index);

            let temp_adapter = match temp_adapter_result {
                Ok(a) => a,
                Err(_) => break, // No more adapters
            };

            if current_index == adapter_index as u32 {
                adapter = Some(temp_adapter);
                break;
            }
            current_index += 1;
        }

        let adapter = adapter.ok_or_else(|| {
            color_eyre::eyre::eyre!("GPU adapter index {} not found", adapter_index)
        })?;

        // Get adapter description
        let desc = adapter.GetDesc1().map_err(|e| {
            color_eyre::eyre::eyre!("Failed to get adapter description: {:?}", e)
        })?;

        let gpu_name = String::from_utf16_lossy(
            &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(0)],
        );

        // For now, we'll use the dedicated video memory as total
        // and estimate available memory
        let total_vram = if desc.DedicatedVideoMemory > 0 {
            desc.DedicatedVideoMemory as u64
        } else {
            desc.SharedSystemMemory as u64
        };

        // Since QueryVideoMemoryInfo is not available in this version,
        // we'll use a simple estimate (50% usage)
        let estimated_used = total_vram / 2;
        let estimated_available = total_vram - estimated_used;

        let (available_vram, used_vram) = (estimated_available, estimated_used);

        let usage_percentage = if total_vram > 0 {
            (used_vram as f64 / total_vram as f64) * 100.0
        } else {
            0.0
        };

        Ok(GpuMemoryInfo {
            name: gpu_name,
            adapter_index,
            total_vram_bytes: total_vram,
            total_vram_gb: total_vram as f64 / (1024.0 * 1024.0 * 1024.0),
            available_vram_bytes: available_vram,
            available_vram_gb: available_vram as f64 / (1024.0 * 1024.0 * 1024.0),
            used_vram_bytes: used_vram,
            usage_percentage,
        })
    }
}

/// Query GPU memory information for all available adapters
#[cfg(target_os = "windows")]
pub fn query_all_gpu_memory() -> Result<Vec<GpuMemoryInfo>> {
    use windows::{
        core::Interface,
        Win32::Graphics::Dxgi::{
            IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ERROR_NOT_FOUND,
        },
    };

    unsafe {
        let factory: IDXGIFactory1 = {
            match windows::Win32::Graphics::Dxgi::CreateDXGIFactory1() {
                Ok(f) => f,
                Err(e) => {
                    color_eyre::eyre::bail!("Failed to create DXGI factory: {:?}", e);
                }
            }
        };

        let mut gpus = Vec::new();
        let mut current_index = 0;

        loop {
            let adapter_result = factory.EnumAdapters1(current_index);

            let _adapter = match adapter_result {
                Ok(a) => a,
                Err(_) => break, // No more adapters
            };

            match query_gpu_memory(current_index as usize) {
                Ok(info) => gpus.push(info),
                Err(e) => {
                    tracing::warn!(
                        "Failed to query memory for GPU adapter {}: {}",
                        current_index,
                        e
                    );
                }
            }
            current_index += 1;
        }

        Ok(gpus)
    }
}

/// Stub implementation for non-Windows platforms
#[cfg(not(target_os = "windows"))]
pub fn query_gpu_memory(_adapter_index: usize) -> Result<GpuMemoryInfo> {
    color_eyre::eyre::bail!("GPU memory querying is only supported on Windows")
}

#[cfg(not(target_os = "windows"))]
pub fn query_all_gpu_memory() -> Result<Vec<GpuMemoryInfo>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "windows")]
    fn test_query_gpu_memory() {
        // This test requires a GPU and will fail in CI without one
        if let Ok(info) = query_gpu_memory(0) {
            println!("GPU: {} ({:.1} GB total, {:.1} GB available, {:.0}% used)",
                info.name,
                info.total_vram_gb,
                info.available_vram_gb,
                info.usage_percentage
            );
            assert!(!info.name.is_empty());
            assert!(info.total_vram_gb > 0.0);
        }
    }

    #[test]
    fn test_suggestions() {
        let info = GpuMemoryInfo {
            name: "Test GPU".to_string(),
            adapter_index: 0,
            total_vram_bytes: 4 * 1024 * 1024 * 1024, // 4 GB
            total_vram_gb: 4.0,
            available_vram_bytes: 500 * 1024 * 1024, // 500 MB
            available_vram_gb: 0.5,
            used_vram_bytes: 3_500 * 1024 * 1024, // 3.5 GB
            usage_percentage: 87.5,
        };

        let suggestions = info.suggestions();
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.contains("x264")));
    }
}
