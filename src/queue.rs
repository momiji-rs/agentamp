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

    /// Ends playback: the current track goes to the history, the rest stays.
    pub fn stop(&mut self) {
        if let Some(done) = self.current.take() {
            self.history.push_back(done);
        }
    }

    pub fn clear(&mut self) {
        self.upcoming.clear();
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
}
