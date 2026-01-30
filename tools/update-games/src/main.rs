use constants::unsupported_games::{UnsupportedGames, UnsupportedReason};
use std::collections::BTreeMap;
use std::fs;

fn reason_heading(reason: &UnsupportedReason) -> Option<&'static str> {
    match reason {
        UnsupportedReason::EnoughData => Some("Sufficient Data Collected"),
        UnsupportedReason::NotAGame => None, // hidden
        UnsupportedReason::Other(_) => Some("Other"),
    }
}

fn main() {
    let md_path = "GAMES.md";
    let marker_start = "<!-- MARKER:";

    let games = UnsupportedGames::load_from_embedded();

    let md_content = fs::read_to_string(md_path).expect("Failed to read GAMES.md");

    let marker_start_pos = md_content
        .find(marker_start)
        .expect("Marker not found in GAMES.md");

    // Find the end of the marker (the closing -->)
    let marker_end_pos = md_content[marker_start_pos..]
        .find("-->")
        .expect("Marker end not found in GAMES.md")
        + marker_start_pos
        + 3; // +3 for "-->"

    let before_marker = &md_content[..marker_end_pos];

    // Group games by reason heading, skipping NotAGame
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    // Collect Other reasons separately so we can show the specific reason
    let mut other_games: Vec<(&str, &str)> = Vec::new();

    for game in &games.games {
        let Some(heading) = reason_heading(&game.reason) else {
            continue; // skip NotAGame
        };
        if let UnsupportedReason::Other(ref s) = game.reason {
            other_games.push((&game.name, s));
        } else {
            groups.entry(heading).or_default().push(&game.name);
        }
    }

    // Sort each group alphabetically and deduplicate
    for names in groups.values_mut() {
        names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        names.dedup();
    }
    other_games.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    other_games.dedup();

    let mut sections = Vec::new();

    for (heading, names) in &groups {
        let mut section = format!("### {heading}\n\n");
        for name in names {
            section.push_str(&format!("- {name}\n"));
        }
        sections.push(section);
    }

    if !other_games.is_empty() {
        let mut section = "### Other\n\n".to_string();
        for (name, reason) in &other_games {
            section.push_str(&format!("- {name} ({reason})\n"));
        }
        sections.push(section);
    }

    let games_list = sections.join("\n");

    let visible_count = groups.values().map(|v| v.len()).sum::<usize>() + other_games.len();
    let new_content = format!("{}\n\n{}", before_marker, games_list);

    fs::write(md_path, new_content).expect("Failed to write GAMES.md");

    println!(
        "Updated GAMES.md with {visible_count} unsupported games (excluding non-game entries)"
    );
}
