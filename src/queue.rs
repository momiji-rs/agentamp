//! The play order: the current track, what comes next, and what has played.

use std::collections::VecDeque;

use serde::Serialize;

use crate::model::Track;

const HISTORY: usize = 100;

#[derive(Debug, Default, Serialize)]
pub struct Queue {
    pub current: Option<Track>,
    pub upcoming: VecDeque<Track>,
    #[serde(skip)]
    pub history: VecDeque<Track>,
}

impl Queue {
    /// Puts `tracks` straight after the current track, in their order.
    pub fn insert_next(&mut self, tracks: Vec<Track>) {
        for track in tracks.into_iter().rev() {
            self.upcoming.push_front(track);
        }
    }

    pub fn append(&mut self, tracks: Vec<Track>) {
        self.upcoming.extend(tracks);
    }

    /// Moves to the next track, keeping the finished one in the history.
    pub fn advance(&mut self) -> Option<Track> {
        if let Some(done) = self.current.take() {
            self.history.push_back(done);
            if self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        self.current = self.upcoming.pop_front();
        self.current.clone()
    }

    /// Steps back to the last played track; the current one plays next.
    pub fn back(&mut self) -> Option<Track> {
        let previous = self.history.pop_back()?;
        if let Some(current) = self.current.take() {
            self.upcoming.push_front(current);
        }
        self.current = Some(previous);
        self.current.clone()
    }

    /// Ends playback: the current track goes to the history, the rest stays.
    pub fn stop(&mut self) {
        if let Some(done) = self.current.take() {
            self.history.push_back(done);
        }
    }

    pub fn clear(&mut self) {
        self.upcoming.clear();
    }

    /// The downloads the queue waits for, in the order they will play, once each.
    pub fn downloads(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for track in self.current.iter().chain(&self.upcoming).filter(|t| t.downloading) {
            if !keys.contains(&track.uri) {
                keys.push(track.uri.clone());
            }
        }
        keys
    }

    /// Puts the downloaded `track` wherever the download `key` waited, the
    /// history included, so going back finds the file.
    pub fn downloaded(&mut self, key: &str, track: &Track) {
        let waiting = |t: &&mut Track| t.downloading && t.uri == key;
        for slot in self.current.iter_mut().chain(self.upcoming.iter_mut()).chain(self.history.iter_mut()).filter(waiting) {
            *slot = track.clone();
        }
    }

    /// Takes out every track waiting for the download `key`, which failed,
    /// and says how many there were.
    pub fn drop_download(&mut self, key: &str) -> usize {
        let waiting = |t: &Track| t.downloading && t.uri == key;
        let before = self.upcoming.len() + self.history.len() + usize::from(self.current.is_some());
        self.upcoming.retain(|t| !waiting(t));
        self.history.retain(|t| !waiting(t));
        if self.current.as_ref().is_some_and(waiting) {
            self.current = None;
        }
        before - (self.upcoming.len() + self.history.len() + usize::from(self.current.is_some()))
    }

    /// Fills in details that arrived after the track was queued.
    pub fn update(&mut self, resolved: &Track) {
        let same = |t: &&mut Track| t.uri == resolved.uri && t.source == resolved.source;
        for track in self.current.iter_mut().chain(self.upcoming.iter_mut()).filter(same) {
            let link = track.link.take();
            *track = resolved.clone();
            track.link = track.link.take().or(link);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Source;

    fn t(name: &str) -> Track {
        Track::placeholder(Source::Local, name)
    }

    fn names(q: &Queue) -> Vec<String> {
        q.upcoming.iter().map(|t| t.uri.clone()).collect()
    }

    fn replace(q: &mut Queue, tracks: Vec<Track>) -> Option<Track> {
        q.clear();
        q.append(tracks);
        q.advance()
    }

    #[test]
    fn replacing_starts_the_first_and_queues_the_rest() {
        let mut q = Queue::default();
        q.append(vec![t("old")]);
        assert_eq!(replace(&mut q, vec![t("a"), t("b")]), Some(t("a")));
        assert_eq!(names(&q), ["b"]);
    }

    #[test]
    fn insert_next_keeps_the_given_order_ahead_of_the_queue() {
        let mut q = Queue::default();
        replace(&mut q, vec![t("now"), t("later")]);
        q.insert_next(vec![t("x"), t("y")]);
        assert_eq!(names(&q), ["x", "y", "later"]);
        assert_eq!(q.current, Some(t("now")));
    }

    #[test]
    fn advance_records_history_and_ends_empty() {
        let mut q = Queue::default();
        replace(&mut q, vec![t("a"), t("b")]);
        assert_eq!(q.advance(), Some(t("b")));
        assert_eq!(q.advance(), None);
        assert_eq!(q.current, None);
        assert_eq!(q.history.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn history_is_bounded() {
        let mut q = Queue::default();
        replace(&mut q, (0..HISTORY + 10).map(|i| t(&i.to_string())).collect());
        while q.advance().is_some() {}
        assert_eq!(q.history.len(), HISTORY);
        assert_eq!(q.history.back().unwrap().uri, (HISTORY + 9).to_string());
    }

    #[test]
    fn update_fills_every_copy_and_keeps_the_link() {
        let mut q = Queue::default();
        let mut placeholder = t("a");
        placeholder.link = Some("https://youtu.be/x".into());
        replace(&mut q, vec![placeholder.clone(), t("b"), placeholder]);
        let mut resolved = t("a");
        resolved.title = "Song".into();
        q.update(&resolved);
        assert_eq!(q.current.as_ref().unwrap().title, "Song");
        assert_eq!(q.upcoming[1].title, "Song");
        assert_eq!(q.upcoming[1].link.as_deref(), Some("https://youtu.be/x"));
        assert_eq!(q.upcoming[0].title, "b");
    }

    #[test]
    fn back_returns_to_the_last_played_and_keeps_the_current_next() {
        let mut q = Queue::default();
        replace(&mut q, vec![t("a"), t("b"), t("c")]);
        q.advance();
        assert_eq!(q.back().unwrap().uri, "a");
        assert_eq!(q.upcoming.iter().map(|t| t.uri.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert!(q.back().is_none());
        assert_eq!(q.current.as_ref().unwrap().uri, "a");
    }

    fn waiting(key: &str) -> Track {
        crate::youtube::pending(key)
    }

    #[test]
    fn downloads_come_in_play_order_once_each() {
        let mut q = Queue::default();
        q.append(vec![t("a"), waiting("ytsearch1:x"), t("b"), waiting("ytsearch1:y"), waiting("ytsearch1:x")]);
        q.advance();
        assert_eq!(q.downloads(), ["ytsearch1:x", "ytsearch1:y"]);
        q.insert_next(vec![waiting("ytsearch1:z")]);
        assert_eq!(q.downloads(), ["ytsearch1:z", "ytsearch1:x", "ytsearch1:y"], "what plays sooner downloads sooner");
        q.clear();
        assert!(q.downloads().is_empty());
    }

    #[test]
    fn a_download_fills_every_place_it_waited() {
        let mut q = Queue::default();
        q.append(vec![waiting("ytsearch1:x"), t("a"), waiting("ytsearch1:x")]);
        q.advance();
        let file = t("/cache/x.m4a");
        q.downloaded("ytsearch1:x", &file);
        assert_eq!(q.current.as_ref().unwrap().uri, "/cache/x.m4a");
        assert_eq!(q.upcoming[1].uri, "/cache/x.m4a");
        assert!(q.downloads().is_empty());
    }

    #[test]
    fn a_failed_download_leaves_the_queue() {
        let mut q = Queue::default();
        q.append(vec![waiting("ytsearch1:x"), t("a"), waiting("ytsearch1:x"), waiting("ytsearch1:y")]);
        q.advance();
        assert_eq!(q.drop_download("ytsearch1:x"), 2, "the current track and the third");
        assert!(q.current.is_none());
        let left: Vec<&str> = q.upcoming.iter().map(|t| t.uri.as_str()).collect();
        assert_eq!(left, ["a", "ytsearch1:y"]);
        assert_eq!(q.drop_download("ytsearch1:y"), 1);
        assert_eq!(q.drop_download("ytsearch1:y"), 0);
        assert_eq!(q.upcoming.len(), 1);
    }
}
