//! Integration tests for `ProfileRegistry`.
//!
//! These tests deliberately live outside the `cfg(windows)` gate so they
//! run on the Mac developer box and in Linux CI. They exercise the
//! platform-independent parts of the crate (profile registration + exe
//! stem lookup) with no DX12 hook involvement.

use depth_hook::ProfileRegistry;
use depth_hook::profiles::cyberpunk2077::Cyberpunk2077;

#[test]
fn builtin_registry_contains_cyberpunk2077() {
    let registry = ProfileRegistry::with_builtin_profiles();
    assert!(!registry.is_empty(), "builtin registry must not be empty");

    let profile = registry
        .find_for_exe_stem("cyberpunk2077")
        .expect("Cyberpunk 2077 profile must be registered by default");
    assert_eq!(profile.name(), "Cyberpunk 2077 (REDengine 4, DX12)");
}

#[test]
fn lookup_is_case_insensitive() {
    // The recorder normalises via file_stem().to_lowercase() upstream,
    // but callers who don't may still pass e.g. "Cyberpunk2077". The
    // registry must tolerate that rather than silently miss.
    let registry = ProfileRegistry::with_builtin_profiles();
    assert!(registry.find_for_exe_stem("Cyberpunk2077").is_some());
    assert!(registry.find_for_exe_stem("CYBERPUNK2077").is_some());
    assert!(registry.find_for_exe_stem("cyberpunk2077").is_some());
}

#[test]
fn unknown_exe_stem_returns_none() {
    let registry = ProfileRegistry::with_builtin_profiles();
    assert!(registry.find_for_exe_stem("not_a_real_game").is_none());
    assert!(registry.find_for_exe_stem("").is_none());
}

#[test]
fn empty_registry_finds_nothing() {
    let registry = ProfileRegistry::empty();
    assert_eq!(registry.len(), 0);
    assert!(registry.is_empty());
    assert!(registry.find_for_exe_stem("cyberpunk2077").is_none());
}

#[test]
fn manually_registered_profile_is_findable() {
    // Prove that downstream crates can register their own profiles without
    // forking this one — critical for the "compounding profiles" thesis.
    let mut registry = ProfileRegistry::empty();
    registry.register(std::sync::Arc::new(Cyberpunk2077));
    assert_eq!(registry.len(), 1);
    assert!(registry.find_for_exe_stem("cyberpunk2077").is_some());
}
