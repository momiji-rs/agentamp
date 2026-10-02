//! Reading local files into tracks.

use std::path::Path;

use lofty::prelude::*;

use crate::model::{Source, Track};

/// A track for `path`, named from its tags, or from the file name when it
/// has none.
pub fn read(path: &Path, source: Source) -> Track {
    let mut track = Track::placeholder(source, path.to_string_lossy());
    track.title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| track.uri.clone());
    let Ok(file) = lofty::read_from_path(path) else {
        return track;
    };
    track.duration_ms = file.properties().duration().as_millis() as u32;
    if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
        if let Some(title) = tag.title().filter(|t| !t.trim().is_empty()) {
            track.title = title.into_owned();
        }
        track.artist = tag.artist().map(|a| a.into_owned()).unwrap_or_default();
        track.album = tag.album().map(|a| a.into_owned()).unwrap_or_default();
    }
    track
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_files_are_named_after_the_file() {
        let dir = crate::testutil::scratch("untagged");
        let path = dir.join("01 Intro.mp3");
        std::fs::write(&path, b"not really audio").unwrap();
        let track = read(&path, Source::Local);
        assert_eq!(track.title, "01 Intro");
        assert_eq!(track.uri, path.to_string_lossy());
        assert_eq!(track.duration_ms, 0);
    }

    #[test]
    fn tags_and_duration_come_from_the_file() {
        let path = crate::testutil::tagged_wav("tagged", "Song", "Band", 1500);
        let track = read(&path, Source::Local);
        assert_eq!((track.title.as_str(), track.artist.as_str()), ("Song", "Band"));
        assert!((1400..=1600).contains(&track.duration_ms), "{}", track.duration_ms);
    }
}
