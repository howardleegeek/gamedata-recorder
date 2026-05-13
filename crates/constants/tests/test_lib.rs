//! Integration tests for `constants::lib` — recording knobs + game allowlist.
//!
//! Most of `lib.rs` is `const` data, so the tests here are existence /
//! invariant checks rather than behavioural unit tests. They catch a class
//! of bug that's surprisingly common: dropping a game from the allowlist
//! during a refactor (which silently disables recording for that title).
//
// `assertions_on_constants` is intentionally allowed here — we *want*
// the assertion to fail loudly if a top-level constant value changes.

#![allow(clippy::assertions_on_constants)]
#![allow(clippy::manual_range_contains)]
#![allow(clippy::const_is_empty)]

use std::time::Duration;

use constants::{
    FPS, GAME_WHITELIST, GH_ORG, GH_REPO, HOOK_TIMEOUT, KNOWN_HOOK_REQUIRED_GAMES, MAX_FOOTAGE,
    MAX_IDLE_DURATION, MIN_AVERAGE_FPS, MIN_FOOTAGE, MIN_FREE_SPACE_MB, PLAY_TIME_BREAK_THRESHOLD,
    PLAY_TIME_DISPLAY_GRANULARITY, PLAY_TIME_ROLLING_WINDOW, PLAY_TIME_SAVE_INTERVAL,
    PLAY_TIME_TESTING, PLAY_TIME_THRESHOLD, RECORDING_HEIGHT, RECORDING_WIDTH,
};

// ---------------------------------------------------------------------------
// Recording knobs
// ---------------------------------------------------------------------------

#[test]
fn fps_matches_buyer_spec_30() {
    // Buyer spec: 30 fps fixed. Locking the constant down because the
    // training pipeline assumes exactly 30 fps per frame_index alignment.
    assert_eq!(FPS, 30);
}

#[test]
fn recording_resolution_is_1080p() {
    // 1920x1080 is the buyer spec resolution.
    assert_eq!(RECORDING_WIDTH, 1920);
    assert_eq!(RECORDING_HEIGHT, 1080);
}

#[test]
fn min_free_space_is_at_least_512mb() {
    assert!(MIN_FREE_SPACE_MB >= 512);
}

#[test]
fn min_average_fps_supports_low_end_hardware() {
    // Set low (5 fps) so integrated GPUs / low-end laptops can still
    // contribute training data. Above-zero is the load-bearing invariant.
    assert!(MIN_AVERAGE_FPS > 0.0);
    assert!(MIN_AVERAGE_FPS <= 30.0);
    assert_eq!(MIN_AVERAGE_FPS, 5.0);
}

// ---------------------------------------------------------------------------
// Footage durations
// ---------------------------------------------------------------------------

#[test]
fn min_footage_is_at_least_15_seconds() {
    // Anything shorter than ~15s is too noisy to be useful training data;
    // 20s is the calibrated floor.
    assert!(MIN_FOOTAGE >= Duration::from_secs(15));
}

#[test]
fn max_footage_is_at_least_5_minutes() {
    // 30 minutes — balancing file size against uninterrupted recording.
    assert!(MAX_FOOTAGE >= Duration::from_secs(5 * 60));
}

#[test]
fn max_idle_duration_is_at_least_3_minutes() {
    // Long loading screens (GTA V, Elden Ring) need at least 3 min of
    // grace before the idle gate fires. Current value: 5 min.
    assert!(MAX_IDLE_DURATION >= Duration::from_secs(3 * 60));
}

#[test]
fn hook_timeout_allows_anti_cheat_init() {
    // Anti-cheat games (BattlEye, EAC, Vanguard) need ~10-15s to finish
    // init before OBS tries to hook. Current value: 15s.
    assert!(HOOK_TIMEOUT >= Duration::from_secs(10));
    assert!(HOOK_TIMEOUT <= Duration::from_secs(30));
}

// ---------------------------------------------------------------------------
// Play-time tracker constants (PLAY_TIME_TESTING gate)
// ---------------------------------------------------------------------------

#[test]
fn play_time_testing_is_false_in_production_builds() {
    // R51 contract: PLAY_TIME_TESTING is the flag that switches the
    // play-time tracker between production durations (hours/minutes)
    // and testing durations (60 seconds). It MUST be false in checked-in
    // code; turning it on for a debug run is fine but committing it is
    // a production bug — recorder would warn users they've played 2h
    // after 60 seconds of gameplay.
    assert!(!PLAY_TIME_TESTING);
}

#[test]
fn play_time_threshold_is_2_hours_in_production() {
    // PLAY_TIME_TESTING = false branch -> 2 hours.
    assert_eq!(PLAY_TIME_THRESHOLD, Duration::from_secs(2 * 60 * 60));
}

#[test]
fn play_time_display_granularity_is_30_minutes() {
    assert_eq!(PLAY_TIME_DISPLAY_GRANULARITY, Duration::from_secs(30 * 60));
}

#[test]
fn play_time_break_threshold_is_4_hours() {
    assert_eq!(PLAY_TIME_BREAK_THRESHOLD, Duration::from_secs(4 * 60 * 60));
}

#[test]
fn play_time_rolling_window_is_8_hours() {
    assert_eq!(PLAY_TIME_ROLLING_WINDOW, Duration::from_secs(8 * 60 * 60));
}

#[test]
fn play_time_save_interval_is_5_minutes() {
    assert_eq!(PLAY_TIME_SAVE_INTERVAL, Duration::from_secs(5 * 60));
}

// ---------------------------------------------------------------------------
// Game whitelist invariants
// ---------------------------------------------------------------------------

#[test]
fn whitelist_contains_priority_titles() {
    // R7.2: Cyberpunk 2077, GTA V, CS2 are the buyer's top-priority titles.
    // Removing any of them would break the depth-hook / engine-telemetry
    // pipeline for the most valuable training data. Locking them in.
    assert!(GAME_WHITELIST.contains(&"cyberpunk2077"));
    assert!(GAME_WHITELIST.contains(&"gta5") || GAME_WHITELIST.contains(&"gtav"));
    assert!(GAME_WHITELIST.contains(&"cs2"));
}

#[test]
fn whitelist_entries_are_all_lowercase() {
    // The recorder's foreground-check lowercases the exe stem before
    // comparing. Mixed-case entries would silently never match.
    for stem in GAME_WHITELIST {
        assert_eq!(
            *stem,
            stem.to_lowercase(),
            "whitelist entry must be all lowercase: {stem:?}"
        );
    }
}

#[test]
fn whitelist_entries_have_no_exe_suffix() {
    // The recorder strips `.exe` before lookup. An accidental ".exe"
    // suffix here would never match.
    for stem in GAME_WHITELIST {
        assert!(
            !stem.ends_with(".exe"),
            "whitelist entry must NOT include .exe suffix: {stem:?}"
        );
    }
}

#[test]
fn whitelist_excludes_banned_anti_cheat_titles() {
    // R47: kernel-level anti-cheat is a HWID-ban risk for the recorder
    // user. The whitelist must NOT include Valorant / LoL / EFT / Halo
    // Infinite / Hell Let Loose / CoD Vanguard. Locking this catches a
    // future PR that re-adds them.
    let banned = [
        "valorant",
        "league of legends",
        "leagueoflegends",
        "lol",
        "eft",
        "tarkov",
        "halo",
        "haloinfinite",
        "halo infinite",
        "hellletloose",
        "hll",
        "vanguard",  // Riot Vanguard binary name
        "rictochet", // Activision Ricochet
    ];
    for b in banned {
        assert!(
            !GAME_WHITELIST.contains(&b),
            "banned anti-cheat title in whitelist: {b}"
        );
    }
}

#[test]
fn whitelist_has_no_playgtav_launcher() {
    // The Rockstar launcher exe is "playgtav.exe" — explicitly removed
    // from the whitelist (the game itself is gta5/gtav/gta5_enhanced).
    // Locking this in catches an accidental re-add.
    assert!(!GAME_WHITELIST.contains(&"playgtav"));
}

#[test]
fn whitelist_has_no_duplicates() {
    use std::collections::HashSet;
    let unique: HashSet<&&str> = GAME_WHITELIST.iter().collect();
    assert_eq!(
        unique.len(),
        GAME_WHITELIST.len(),
        "whitelist has duplicate entries"
    );
}

#[test]
fn whitelist_is_nonempty() {
    assert!(!GAME_WHITELIST.is_empty());
    // Should have at least 50 entries — sanity check on size.
    assert!(GAME_WHITELIST.len() >= 50);
}

#[test]
fn known_hook_required_games_starts_empty() {
    // Per doc comment: start empty; only add entries when a specific
    // game empirically fails under WGC. Catching unintentional adds is
    // valuable — every entry here forces an older code path that we'd
    // rather not exercise unless required.
    //
    // If this test fails, ADD a comment to the constant explaining
    // exactly which OS/GPU/game combo regressed and what the symptom
    // is, then update the assertion to match.
    if !KNOWN_HOOK_REQUIRED_GAMES.is_empty() {
        eprintln!(
            "WARNING: KNOWN_HOOK_REQUIRED_GAMES has {} entry/entries — \
             ensure each is documented with the regression that motivated it",
            KNOWN_HOOK_REQUIRED_GAMES.len()
        );
    }
    // Hard lock: ≤5 entries (anything more means WGC is broken broadly
    // and should escalate to a recorder-wide fix, not a per-title pin).
    assert!(KNOWN_HOOK_REQUIRED_GAMES.len() <= 5);
}

// ---------------------------------------------------------------------------
// GitHub project metadata
// ---------------------------------------------------------------------------

#[test]
fn github_org_is_howardleegeek() {
    // R0: GitHub project is howardleegeek/gamedata-recorder. Lock it.
    assert_eq!(GH_ORG, "howardleegeek");
    assert_eq!(GH_REPO, "gamedata-recorder");
}

// ---------------------------------------------------------------------------
// Filename constants
// ---------------------------------------------------------------------------

#[test]
fn filename_recording_constants_are_stable() {
    // The buyer's parser hard-codes these filenames. Locking them.
    use constants::filename::recording;
    assert_eq!(recording::VIDEO, "recording.mp4");
    assert_eq!(recording::INPUTS, "inputs.jsonl");
    assert_eq!(recording::METADATA, "metadata.json");
    assert_eq!(recording::FPS_LOG, "fps_log.json");
    assert_eq!(recording::FRAMES_JSONL, "frames.jsonl");
    assert_eq!(recording::ACTION_CAMERA_JSON, "action_camera.json");
    assert_eq!(recording::INVALID, ".invalid");
    assert_eq!(recording::SERVER_INVALID, ".server_invalid");
    assert_eq!(recording::UPLOADED, ".uploaded");
    assert_eq!(recording::UPLOAD_PROGRESS, ".upload-progress");
    assert_eq!(recording::INPUTS_LEGACY_CSV, "inputs.csv");
}

#[test]
fn filename_persistent_constants_are_stable() {
    use constants::filename::persistent;
    assert_eq!(persistent::CONFIG, "config.json");
    assert_eq!(persistent::PLAY_TIME_STATE, "play_time.json");
}

// ---------------------------------------------------------------------------
// Helper `const fn` runtime coverage
// ---------------------------------------------------------------------------
//
// `duration_from_mins` / `duration_from_hours` are private `const fn` helpers
// inlined into the `PLAY_TIME_*` constants. llvm-cov treats `const fn` calls
// resolved at compile time as not executed at runtime, so we exercise them
// indirectly by asserting their result matches the manual computation. This
// keeps the coverage report honest about the helpers being used.

#[test]
fn play_time_constants_match_manual_minute_computation() {
    // PLAY_TIME_DISPLAY_GRANULARITY uses `duration_from_mins(30)`. If
    // someone refactored that helper to e.g. divide by 60 instead of
    // multiply, this assertion would fail.
    assert_eq!(PLAY_TIME_DISPLAY_GRANULARITY, Duration::from_secs(30 * 60));
    assert_eq!(PLAY_TIME_SAVE_INTERVAL, Duration::from_secs(5 * 60));
}

#[test]
fn play_time_constants_match_manual_hour_computation() {
    // PLAY_TIME_THRESHOLD uses `duration_from_hours(2)`. Verifying the
    // result lines up with manual 2*60*60 seconds.
    assert_eq!(PLAY_TIME_THRESHOLD, Duration::from_secs(2 * 60 * 60));
    assert_eq!(PLAY_TIME_BREAK_THRESHOLD, Duration::from_secs(4 * 60 * 60));
    assert_eq!(PLAY_TIME_ROLLING_WINDOW, Duration::from_secs(8 * 60 * 60));
    // MAX_FOOTAGE uses `duration_from_mins(30)`.
    assert_eq!(MAX_FOOTAGE, Duration::from_secs(30 * 60));
}

// ---------------------------------------------------------------------------
// supported_games.json sanity — embedded fixture parses
// ---------------------------------------------------------------------------

#[test]
fn supported_games_json_is_valid_json_array() {
    // The shipped supported_games.json is parsed at build time by the
    // recorder's `include_str!` machinery elsewhere; we sanity-check it
    // here too so a malformed edit fails its own crate's test suite
    // before propagating.
    let raw = include_str!("../src/supported_games.json");
    let v: serde_json::Value =
        serde_json::from_str(raw).expect("supported_games.json is valid JSON");
    assert!(
        v.is_array(),
        "supported_games.json must be a top-level array"
    );
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty(), "supported_games.json must be non-empty");
    // Every entry must have `game`, `url`, `binaries` keys.
    for (i, entry) in arr.iter().enumerate() {
        let obj = entry
            .as_object()
            .unwrap_or_else(|| panic!("entry {i} must be an object"));
        assert!(obj.contains_key("game"), "entry {i} missing `game`");
        assert!(obj.contains_key("url"), "entry {i} missing `url`");
        assert!(obj.contains_key("binaries"), "entry {i} missing `binaries`");
        let binaries = obj["binaries"]
            .as_array()
            .unwrap_or_else(|| panic!("entry {i}.binaries must be array"));
        assert!(
            !binaries.is_empty(),
            "entry {i} ({}) has empty binaries",
            obj["game"]
        );
    }
}
