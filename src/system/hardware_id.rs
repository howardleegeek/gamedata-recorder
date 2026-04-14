use color_eyre::Result;
use game_process::hardware_id;

pub(crate) fn get() -> Result<String> {
    // Strip "{}" off the ends of the windows hardware ID
    hardware_id().map(|id| {
        // Bounds check to prevent panic on malformed hardware IDs
        if id.len() >= 2 && id.starts_with('{') && id.ends_with('}') {
            id[1..id.len() - 1].to_owned()
        } else {
            id
        }
    })
}
