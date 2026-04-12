use std::time::Duration;

pub mod encoding;
pub mod unsupported_games;

/// Game whitelist - binaries that are allowed to be recorded
/// Generated from supported_games.json
pub const GAME_WHITELIST: &[&str] = &[
    // Abyssus
    "abyssus",
    "rgame",
    // Amenti
    "amenti",
    // ARMA 3
    "arma3",
    "arma3_x64",
    // Battlefield Hardline
    "bfh",
    // Blacktail
    "blacktail",
    "blacktail-win64-shipping",
    // Blair Witch
    "blairwitch",
    // Call of Duty: Advanced Warfare
    "s1_sp64_ship",
    "s1_mp64_ship",
    // Call of Duty: Infinite Warfare
    "iw7_ship",
    // Call of Duty: Vanguard
    "vanguard",
    // Call of Duty: WWII
    "s2_sp64_ship",
    "s2_mp64_ship",
    // Close to the Sun
    "ctts-win64-shipping",
    // Conundrum
    "trygame-win32-shipping",
    "conundrum",
    // Dark Hours
    "dark hours",
    "darkhours-win64-shipping",
    // Deadzone: Rogue
    "deadzonesteam",
    // Earthfall
    "earthfall",
    // Escape from Tarkov
    "escapefromtarkov",
    "escapefromtarkov_be",
    // Everyone's Gone to the Rapture
    "rapture_release",
    // Fobia - St. Dinfna Hotel
    "fobia-win64-shipping",
    // Ghost Watchers
    "ghost watchers",
    // Halo: Infinite
    "haloinfinite",
    // Hard Reset Redux
    "hr.win32",
    "hr.x64",
    "hr",
    // Hardspace: Shipbreaker
    "shipbreaker",
    // Hell Let Loose
    "hll",
    "hll-win64-shipping",
    "hllepicgamesstore",
    // Home Sweet Home 2
    "homesweethome2",
    "homesweethome2-win64-shipping",
    // Home Sweet Home
    "homesweethome",
    "homesweethome-win32-shipping",
    // Immortals of Aveum
    "immortalsofaveum",
    "immortalsofaveum-win64-shipping",
    // In Sound Mind
    "in sound mind",
    // Layers of Fear 2
    "lof2",
    "lof2-win64-shipping",
    // Layers of Fear
    "layers of fear",
    "layers of fearsub",
    // Madison
    "madison",
    // METAL EDEN
    "metaleden",
    "metaleden-win64-shipping",
    // Observer: System Redux
    "observersystemredux",
    // The Outer Worlds 2
    "theouterworlds2",
    "theouterworlds2-win64-shipping",
    // Painkiller 2025
    "painkiller",
    "painkiller-win64-shipping",
    // Painkiller Hell & Damnation
    "pkhdgame-win32-shipping",
    // Panicore
    "panicore",
    "panicore-win64-shipping",
    // PAYDAY 3
    "payday3client",
    "payday3client-win64-shipping",
    // Ready or Not
    "readyornot",
    "readyornotsteam-win64-shipping",
    "readyornotxboxpc-wingdk-shipping",
    // Riven
    "riven",
    "riven-win64-shipping",
    // Salt 2
    "salt2",
    // SCP: 5K
    "pandemic",
    // Shadow Warrior 3
    "sw3",
    // Soma
    "soma",
    "soma_nosteam",
    // Squad
    "squadgame",
    // Tacoma
    "tacoma",
    // The Beast Inside
    "thebeastinside",
    "thebeastinside-win64-shipping",
    // The Darkness II
    "darknessii",
    // The Lightkeeper
    "thelightkeeper",
    "thelightkeeper-win64-shipping",
    // The Stanley Parable
    "stanley",
    // The Talos Principle 2
    "talos2",
    "talos2-win64-shipping",
    // The Witness
    "witness64_d3d11",
    "witness_d3d11",
    // Trepang2
    "cppfps-win64-shipping",
    // Visage
    "visage",
    "visage-win64-shipping",
    // VOIDBREAKER
    "voidbreaker",
    "voidbreaker-win64-shipping",
    // VOIN
    "voin",
    "voin-win64-shipping",
    // What Remains of Edith Finch
    "finchgame",
    // Witchfire
    "witchfire",
    // Wolfenstein: Youngblood
    "youngblood_x64vk",
    // Ziggurat 2
    "ziggurat2",
];

pub const FPS: u32 = 60;
pub const RECORDING_WIDTH: u32 = 1280;
pub const RECORDING_HEIGHT: u32 = 720;

/// Minimum free space required to record (in megabytes)
pub const MIN_FREE_SPACE_MB: u64 = 512;

/// Minimum footage length
pub const MIN_FOOTAGE: Duration = Duration::from_secs(20);
/// Maximum footage length
pub const MAX_FOOTAGE: Duration = duration_from_mins(10);
/// Maximum idle duration before stopping recording
pub const MAX_IDLE_DURATION: Duration = Duration::from_secs(30);
/// Maximum time to wait for OBS to hook into the application before stopping recording
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum average FPS. We allow some leeway below 60 FPS, but we want to make sure
/// we aren't getting 30-40 FPS data.
pub const MIN_AVERAGE_FPS: f64 = FPS as f64 * 0.9;

// Play-time tracker
/// Whether or not to use testing constants (should always be false in production)
pub const PLAY_TIME_TESTING: bool = false;
/// Threshold before showing overlay
pub const PLAY_TIME_THRESHOLD: Duration = if PLAY_TIME_TESTING {
    Duration::from_secs(60)
} else {
    duration_from_hours(2)
};
/// Display granularity - how coarsely to round time values for display
pub const PLAY_TIME_DISPLAY_GRANULARITY: Duration = if PLAY_TIME_TESTING {
    Duration::from_secs(60)
} else {
    duration_from_mins(30)
};
/// Break threshold - reset after this much idle time
pub const PLAY_TIME_BREAK_THRESHOLD: Duration = if PLAY_TIME_TESTING {
    Duration::from_secs(60)
} else {
    duration_from_hours(4)
};
/// Rolling window - reset after this much time since last break
pub const PLAY_TIME_ROLLING_WINDOW: Duration = if PLAY_TIME_TESTING {
    Duration::from_secs(60)
} else {
    duration_from_hours(8)
};
/// Save interval for play time state
pub const PLAY_TIME_SAVE_INTERVAL: Duration = if PLAY_TIME_TESTING {
    Duration::from_secs(60)
} else {
    duration_from_mins(5)
};

/// GitHub organization
pub const GH_ORG: &str = "Overworldai";
/// GitHub repository
pub const GH_REPO: &str = "owl-control";

pub mod filename {
    pub mod recording {
        /// Reasons that a recording is invalid
        pub const INVALID: &str = ".invalid";
        /// Reasons that a server invalidated a recording
        pub const SERVER_INVALID: &str = ".server_invalid";
        /// Indicates the file was uploaded; contains information about the upload
        pub const UPLOADED: &str = ".uploaded";
        /// Stores upload progress state for pause/resume functionality
        pub const UPLOAD_PROGRESS: &str = ".upload-progress";
        /// The video recording file
        pub const VIDEO: &str = "recording.mp4";
        /// The input recording file (JSON Lines format for buyer spec compliance)
        pub const INPUTS: &str = "inputs.jsonl";
        /// Legacy CSV input file (for backward compatibility with older recordings)
        pub const INPUTS_LEGACY_CSV: &str = "inputs.csv";
        /// The metadata file
        pub const METADATA: &str = "metadata.json";
        /// Per-second FPS log (buyer spec requirement)
        pub const FPS_LOG: &str = "fps_log.json";
    }

    pub mod persistent {
        /// The config file, stored in persistent data directory
        pub const CONFIG: &str = "config.json";
        /// The play time state file, stored in persistent data directory
        pub const PLAY_TIME_STATE: &str = "play_time.json";
    }
}

// This may not be necessary in a future Rust: <https://github.com/rust-lang/rust/issues/120301>
const fn duration_from_mins(minutes: u64) -> Duration {
    Duration::from_secs(minutes * 60)
}

const fn duration_from_hours(hours: u64) -> Duration {
    duration_from_mins(hours * 60)
}
