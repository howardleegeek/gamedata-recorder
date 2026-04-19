use color_eyre::Result;
use game_process::hardware_id;

pub(crate) fn get() -> Result<String> {
    // Strip "{}" off the ends of the windows hardware ID.
    // Guard against unexpected short strings to avoid panicking on slice.
    hardware_id().map(|id| {
        if id.len() >= 2 && id.starts_with('{') && id.ends_with('}') {
            id[1..id.len() - 1].to_owned()
        } else {
            id
        }
    })
}
