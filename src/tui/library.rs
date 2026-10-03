//! The library's shelves and the Spotify pages opened from them: what is
//! selected, which page is open on which, and the pages as they load.

use std::collections::HashMap;

use crate::browse::{MOST, Page};
use crate::ipc::Request;
use crate::spotify_search::Hit;

/// The shelves, by the names `browse` takes and as the panel shows them.
pub const SHELVES: [(&str, &str); 5] = [
    ("liked", "Liked Songs"),
    ("playlists", "Playlists"),
    ("albums", "Albums"),
    ("artists", "Artists"),
    ("top", "Top this month"),
];
/// The shelves whose size is shown, asked for when the window opens.
pub const COUNTED: [&str; 4] = ["liked", "playlists", "albums", "artists"];
/// How near the end of what has loaded the selection comes before more is asked for.
const AHEAD: usize = 10;
/// How far Page Up and Page Down move.
pub const LEAP: isize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    /// The keys work the player.
    #[default]
    Player,
    Shelves,
    Page,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub focus: Focus,
    /// The selected shelf.
    pub shelf: usize,
    /// How many each shelf holds, once Spotify has said.
    pub counts: HashMap<String, u32>,
    /// The pages opened, the shown one last. None shows the queue.
    pub pages: Vec<Opened>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Opened {
    pub target: String,
    /// What it was called where it was opened, shown until it loads.
    pub title: String,
    pub page: Option<Result<Page, String>>,
    /// The selected item, counted across the sections.
    pub selected: usize,
    /// More of it is on its way.
    pub more: bool,
}

/// What Enter does with an item.
pub enum Choice {
    Play(String),
    Open(String, String),
}

impl Opened {
    fn new(target: &str, title: &str) -> Self {
        Self { target: target.into(), title: title.into(), page: None, selected: 0, more: false }
    }

    pub fn items(&self) -> impl Iterator<Item = &Hit> {
        let page = self.page.as_ref().and_then(|p| p.as_ref().ok());
        page.into_iter().flat_map(|p| &p.sections).flat_map(|s| &s.items)
    }

    pub fn hit(&self) -> Option<&Hit> {
        self.items().nth(self.selected)
    }

    /// The next part of a paged section, when the selection nears the end
    /// of what has loaded.
    fn wants_more(&mut self) -> Option<Request> {
        let Some(Ok(page)) = &self.page else { return None };
        let loaded = page.sections.iter().map(|s| s.items.len()).sum::<usize>();
        let unfinished = page.sections.iter().find(|s| (s.items.len() as u32) < s.total)?;
        if self.more || self.selected + AHEAD < loaded {
            return None;
        }
        self.more = true;
        Some(Request::Browse { target: self.target.clone(), offset: unfinished.items.len() as u32, count: MOST })
    }
}

/// Whether a target is a page to open rather than something to play.
pub fn opens(target: &str) -> bool {
    !target.starts_with("spotify:track:")
}

/// Whether a target can be queued.
pub fn plays(target: &str) -> bool {
    !target.contains(":folder:")
}

fn browse(target: &str) -> Request {
    Request::Browse { target: target.into(), offset: 0, count: MOST }
}

impl Library {
    /// Where Tab goes from here: the shelves, the open page, then the player.
    pub fn next_focus(&self, forward: bool) -> Focus {
        let mut order = vec![Focus::Player, Focus::Shelves];
        if !self.pages.is_empty() {
            order.push(Focus::Page);
        }
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let step = if forward { 1 } else { order.len() - 1 };
        order[(at + step) % order.len()]
    }

    /// Esc: back one page, from the pages to the shelves, from the shelves
    /// to the player, and from the player to the queue. False when there was
    /// nowhere to go back to.
    pub fn back(&mut self) -> bool {
        match self.focus {
            Focus::Page => {
                self.pages.pop();
                if self.pages.is_empty() {
                    self.focus = Focus::Shelves;
                }
            }
            Focus::Shelves => self.focus = Focus::Player,
            Focus::Player if !self.pages.is_empty() => self.pages.clear(),
            Focus::Player => return false,
        }
        true
    }

    /// Moves the selection by `by`, asking for more of the page near its end.
    pub fn step(&mut self, by: isize) -> Option<Request> {
        let move_by = |at: usize, len: usize| at.saturating_add_signed(by).min(len.saturating_sub(1));
        match self.focus {
            Focus::Shelves => self.shelf = move_by(self.shelf, SHELVES.len()),
            Focus::Page => {
                let opened = self.pages.last_mut()?;
                opened.selected = move_by(opened.selected, opened.items().count());
                return opened.wants_more();
            }
            Focus::Player => {}
        }
        None
    }

    /// Opens the selected shelf in place of whatever was open.
    pub fn open_shelf(&mut self) -> Request {
        let (target, title) = SHELVES[self.shelf];
        self.pages = vec![Opened::new(target, title)];
        self.focus = Focus::Page;
        browse(target)
    }

    /// Opens a page on top of the shown one.
    pub fn open(&mut self, target: &str, title: &str) -> Request {
        self.pages.push(Opened::new(target, title));
        self.focus = Focus::Page;
        browse(target)
    }

    /// What Enter does with the selected item.
    pub fn choice(&self) -> Option<Choice> {
        let hit = self.pages.last()?.hit()?;
        Some(if opens(&hit.target) {
            Choice::Open(hit.target.clone(), hit.title.clone())
        } else {
            Choice::Play(hit.target.clone())
        })
    }

    /// Takes in a page from `offset` of `target`: the first part of an open
    /// page, or more of it. An error is given back when it was for more.
    pub fn loaded(&mut self, target: &str, offset: u32, page: Result<Page, String>) -> Option<String> {
        if offset == 0
            && let (Some(shelf), Ok(page)) = (COUNTED.iter().find(|s| **s == target), &page)
            && let Some(section) = page.sections.first()
        {
            self.counts.insert(shelf.to_string(), section.total);
        }
        let opened = self.pages.iter_mut().rev().find(|o| o.target == target)?;
        if offset == 0 {
            opened.page = Some(page);
            return None;
        }
        opened.more = false;
        let more = match page {
            Ok(more) => more,
            Err(e) => return Some(e),
        };
        let Some(Ok(shown)) = &mut opened.page else { return None };
        // Only the paged sections grow; the others came whole the first time.
        for section in more.sections {
            if let Some(kept) = shown.sections.iter_mut().find(|s| s.name == section.name)
                && (kept.items.len() as u32) < kept.total
                && kept.items.len() as u32 == offset
            {
                kept.items.extend(section.items);
            }
        }
        None
    }
}
