//! Bookmarks.

use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CoreResult;
use crate::storage::{load_json_or_default, save_json_atomic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub url: Url,
    pub title: String,
    pub created: u64,
}

/// Ordered bookmark list. Order is user-defined and shown as-is in the bookmarks bar.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Bookmarks {
    items: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn load(path: &Path) -> CoreResult<Self> {
        load_json_or_default(path)
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        save_json_atomic(path, self)
    }

    pub fn items(&self) -> &[Bookmark] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn contains(&self, url: &Url) -> bool {
        self.items.iter().any(|bookmark| &bookmark.url == url)
    }

    /// Add a bookmark; adding an existing URL only updates its title.
    pub fn add(&mut self, url: Url, title: String, now: u64) {
        match self.items.iter_mut().find(|bookmark| bookmark.url == url) {
            Some(existing) => existing.title = title,
            None => self.items.push(Bookmark { url, title, created: now }),
        }
    }

    pub fn remove(&mut self, url: &Url) -> bool {
        let before = self.items.len();
        self.items.retain(|bookmark| &bookmark.url != url);
        before != self.items.len()
    }

    /// Toggle the bookmark for `url`, returning whether it is now bookmarked.
    pub fn toggle(&mut self, url: &Url, title: &str, now: u64) -> bool {
        if self.remove(url) {
            false
        } else {
            self.add(url.clone(), title.to_owned(), now);
            true
        }
    }

    pub fn rename(&mut self, url: &Url, title: String) {
        if let Some(bookmark) = self.items.iter_mut().find(|bookmark| &bookmark.url == url) {
            bookmark.title = title;
        }
    }

    pub fn move_to(&mut self, url: &Url, index: usize) {
        if let Some(position) = self.items.iter().position(|bookmark| &bookmark.url == url) {
            let bookmark = self.items.remove(position);
            self.items.insert(index.min(self.items.len()), bookmark);
        }
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&Bookmark> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.items
            .iter()
            .filter(|bookmark| {
                bookmark.title.to_lowercase().contains(&query) || bookmark.url.as_str().to_lowercase().contains(&query)
            })
            .take(limit)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn toggle_adds_and_removes() {
        let mut bookmarks = Bookmarks::default();
        assert!(bookmarks.toggle(&url("https://servo.org/"), "Servo", 1));
        assert!(bookmarks.contains(&url("https://servo.org/")));
        assert!(!bookmarks.toggle(&url("https://servo.org/"), "Servo", 2));
        assert!(bookmarks.is_empty());
    }

    #[test]
    fn duplicates_update_title() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(url("https://a.com/"), "A".into(), 1);
        bookmarks.add(url("https://a.com/"), "A2".into(), 2);
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks.items()[0].title, "A2");
    }

    #[test]
    fn reorder_and_search() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(url("https://a.com/"), "Alpha".into(), 1);
        bookmarks.add(url("https://b.com/"), "Beta".into(), 1);
        bookmarks.move_to(&url("https://b.com/"), 0);
        assert_eq!(bookmarks.items()[0].title, "Beta");
        assert_eq!(bookmarks.search("alp", 10).len(), 1);
        assert_eq!(bookmarks.search("b.com", 10).len(), 1);
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(url("https://a.com/"), "A".into(), 1);
        bookmarks.save(&path).unwrap();
        assert_eq!(Bookmarks::load(&path).unwrap().len(), 1);
    }
}
