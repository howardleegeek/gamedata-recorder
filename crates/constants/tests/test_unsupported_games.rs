//! Integration tests for `constants::unsupported_games`.
//!
//! `UnsupportedGames` is the recorder's blocklist used by the foreground
//! gate to refuse recording for non-game processes (launchers, Discord
//! overlays, browsers, etc.). The matching rules are non-trivial — exact
//! match OR suffix-with-underscore-or-dash OR `epicgamesstore` prefix —
//! and each branch needs a regression test.

use constants::unsupported_games::{UnsupportedGames, UnsupportedReason};

// ---------------------------------------------------------------------------
// UnsupportedReason::Display
// ---------------------------------------------------------------------------

#[test]
fn unsupported_reason_display_enough_data() {
    let r = UnsupportedReason::EnoughData;
    let s = format!("{r}");
    assert_eq!(s, "We have collected enough data for this game.");
}

#[test]
fn unsupported_reason_display_not_a_game() {
    let r = UnsupportedReason::NotAGame;
    let s = format!("{r}");
    assert_eq!(s, "This is not a game.");
}

#[test]
fn unsupported_reason_display_other_is_pass_through() {
    let r = UnsupportedReason::Other("Banned upstream".to_string());
    assert_eq!(format!("{r}"), "Banned upstream");
}

#[test]
fn unsupported_reason_serde_round_trip() {
    // Configs may persist the reason. Lock down serde shape.
    for r in [
        UnsupportedReason::EnoughData,
        UnsupportedReason::NotAGame,
        UnsupportedReason::Other("x".into()),
    ] {
        let s = serde_json::to_string(&r).unwrap();
        let back: UnsupportedReason = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}

// ---------------------------------------------------------------------------
// UnsupportedGames::load_from_str — parser contract
// ---------------------------------------------------------------------------

#[test]
fn load_from_str_with_empty_array_succeeds() {
    let games = UnsupportedGames::load_from_str("[]").expect("empty array is valid");
    assert_eq!(games.games.len(), 0);
}

#[test]
fn load_from_str_with_single_entry() {
    let json = r#"[
        {
            "name": "Discord",
            "binaries": ["discord"],
            "reason": "NotAGame"
        }
    ]"#;
    let games = UnsupportedGames::load_from_str(json).expect("valid single entry");
    assert_eq!(games.games.len(), 1);
    assert_eq!(games.games[0].name, "Discord");
    assert_eq!(games.games[0].binaries, vec!["discord"]);
    assert_eq!(games.games[0].reason, UnsupportedReason::NotAGame);
}

#[test]
fn load_from_str_with_multi_binary_entry() {
    // The recorder needs to support multiple exe names per game (e.g.
    // launcher + game.exe). Lock that down.
    let json = r#"[
        {
            "name": "Some Launcher",
            "binaries": ["launcher", "client", "service"],
            "reason": "NotAGame"
        }
    ]"#;
    let games = UnsupportedGames::load_from_str(json).expect("valid multi-binary");
    assert_eq!(games.games[0].binaries.len(), 3);
}

#[test]
fn load_from_str_with_other_reason() {
    // "Other(string)" variant must round-trip through serde.
    let json = r#"[
        {
            "name": "Foo",
            "binaries": ["foo"],
            "reason": {"Other": "custom reason"}
        }
    ]"#;
    let games = UnsupportedGames::load_from_str(json).expect("Other variant parses");
    assert_eq!(
        games.games[0].reason,
        UnsupportedReason::Other("custom reason".to_string())
    );
}

#[test]
fn load_from_str_with_enough_data_reason() {
    let json = r#"[
        {
            "name": "Saturated Game",
            "binaries": ["saturated"],
            "reason": "EnoughData"
        }
    ]"#;
    let games = UnsupportedGames::load_from_str(json).expect("EnoughData parses");
    assert_eq!(games.games[0].reason, UnsupportedReason::EnoughData);
}

#[test]
fn load_from_str_with_malformed_json_returns_error() {
    // serde_json must surface the parse error rather than panic. The
    // recorder uses this error to fall back to the embedded list.
    let bad = "not-json-at-all";
    let result = UnsupportedGames::load_from_str(bad);
    assert!(result.is_err(), "malformed JSON must return Err");
}

#[test]
fn load_from_str_with_missing_field_returns_error() {
    // Missing "binaries" should fail rather than silently default to [].
    let json = r#"[{"name":"x","reason":"NotAGame"}]"#;
    let result = UnsupportedGames::load_from_str(json);
    assert!(result.is_err(), "missing required field must error");
}

// ---------------------------------------------------------------------------
// UnsupportedGames::load_from_embedded — sanity check the shipped JSON
// ---------------------------------------------------------------------------

#[test]
fn load_from_embedded_succeeds() {
    // The build-included unsupported_games.json must always parse —
    // a broken JSON file would crash the recorder on startup. Catch it
    // here at test time instead of at user-runtime.
    let _games = UnsupportedGames::load_from_embedded();
}

// ---------------------------------------------------------------------------
// UnsupportedGames::get — the matching contract
// ---------------------------------------------------------------------------

fn make_test_games() -> UnsupportedGames {
    UnsupportedGames::load_from_str(
        r#"[
            {"name":"Discord","binaries":["discord"],"reason":"NotAGame"},
            {"name":"Game X","binaries":["gamex"],"reason":"NotAGame"},
            {"name":"Launcher","binaries":["launcher"],"reason":"NotAGame"}
        ]"#,
    )
    .expect("test fixture must parse")
}

#[test]
fn get_returns_none_for_unknown_binary() {
    let games = make_test_games();
    assert!(games.get("totally_unknown_game").is_none());
    assert!(games.get("").is_none());
}

#[test]
fn get_returns_match_for_exact_lowercase() {
    let games = make_test_games();
    assert!(games.get("discord").is_some());
}

#[test]
fn get_is_case_insensitive() {
    // The matching function lowercases the query before comparison.
    // The recorder's foreground-check passes `file_stem()` which can
    // have mixed casing (`Discord.exe` on Windows) — locking case-
    // insensitivity here catches accidental case-sensitive comparison.
    let games = make_test_games();
    assert!(games.get("Discord").is_some());
    assert!(games.get("DISCORD").is_some());
    assert!(games.get("DiScOrD").is_some());
}

#[test]
fn get_matches_underscore_suffix() {
    // Exe suffixes like `gamex_dx12` should still match `gamex`. This is
    // how DirectX 11 vs 12 variants of the same game share an entry.
    let games = make_test_games();
    assert!(games.get("gamex_dx12").is_some());
    assert!(games.get("gamex_x64").is_some());
    assert!(games.get("gamex_shipping").is_some());
}

#[test]
fn get_matches_dash_suffix() {
    // Unreal Engine titles ship with `-win64-shipping` etc. suffixes.
    let games = make_test_games();
    assert!(games.get("gamex-win64-shipping").is_some());
    assert!(games.get("gamex-win32-shipping").is_some());
}

#[test]
fn get_matches_epicgamesstore_prefix() {
    // The Epic Games Store launcher prefixes some exes with "EpicGamesStore"
    // followed by the game's exe stem — match this variant too.
    let games = make_test_games();
    assert!(games.get("gamexepicgamesstore").is_some());
    assert!(games.get("gamexepicgamesstore_launcher").is_some());
}

#[test]
fn get_does_not_match_partial_substring() {
    // Just "gamex" should not match a different exe that happens to
    // contain "gamex" in the middle — only the suffix rules apply.
    let games = make_test_games();
    // Suffix without underscore/dash/epicgamesstore prefix → no match.
    assert!(
        games.get("notgamex").is_none(),
        "prefix substring should not match"
    );
    assert!(
        games.get("mygamex").is_none(),
        "embedded substring should not match"
    );
}

#[test]
fn get_returns_correct_entry_for_multi_match() {
    // When multiple entries could match, we return the first one. The
    // current impl uses iterator find() so it returns the first
    // registration order. Lock the contract down: callers can rely on
    // "first registration wins" for ambiguous cases.
    let games = UnsupportedGames::load_from_str(
        r#"[
            {"name":"First","binaries":["abc"],"reason":"NotAGame"},
            {"name":"Second","binaries":["abc"],"reason":"EnoughData"}
        ]"#,
    )
    .unwrap();
    let m = games.get("abc").expect("must find a match");
    assert_eq!(m.name, "First");
}

// ---------------------------------------------------------------------------
// UnsupportedGame derives — Clone, Debug, Eq
// ---------------------------------------------------------------------------

#[test]
fn unsupported_game_is_cloneable_and_equatable() {
    let games = make_test_games();
    let first = &games.games[0];
    let cloned = first.clone();
    assert_eq!(*first, cloned);
    // Debug should not panic — sanity-only assertion.
    let _ = format!("{first:?}");
}

// ---------------------------------------------------------------------------
// detect_installed_games — Steam locator integration
// ---------------------------------------------------------------------------

#[test]
fn detect_installed_games_does_not_panic_without_steam() {
    // The recorder's startup code calls `detect_installed_games()` whether
    // or not Steam is installed. The function must return cleanly (an
    // empty Vec is the documented behaviour on no-Steam machines).
    //
    // This test runs both on dev boxes with Steam installed and on CI
    // without it; either path must not panic. We assert the return type
    // is a Vec but make no claim about its contents.
    use constants::unsupported_games::detect_installed_games;
    let games = detect_installed_games();
    // Length is non-negative by construction; this is mostly a smoke test
    // that the function didn't panic / abort.
    // (We use the result to keep clippy from flagging it as unused.)
    assert!(games.len() < 100_000);
}

#[test]
fn installed_game_struct_is_constructible() {
    // The recorder constructs `InstalledGame` from the Steam locator
    // output. Locking the public field shape so a refactor that renamed
    // `steam_app_id` -> `app_id` would fail loudly at this point.
    use constants::unsupported_games::InstalledGame;
    let g = InstalledGame {
        name: "Test Game".to_string(),
        steam_app_id: 12345,
    };
    assert_eq!(g.name, "Test Game");
    assert_eq!(g.steam_app_id, 12345);
}
