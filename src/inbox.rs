//! The owner's alerts, worked out from the board's `@manager` mentions (spec 3a and 7): the
//! one-line alert line's counts and newest item, each lead's badges, and the inbox.
//!
//! Pure: no SQLite, no terminal. Every message is classified by `board::tone_of`, the one
//! classifier, and counted on its own: each `@manager` message is one item.
//! - Need you (a `question:` to the owner): on the alert line until the owner clears it
//!   (marks it read) or its thread is resolved.
//! - For your awareness (any other mention of the owner): on the alert line until the owner
//!   marks it read (opening its thread does) or its thread is resolved.
//!
//! The inbox lists need you and awareness only; an item read, cleared or on a
//! resolved thread is not kept here, and stays readable in its thread.
//!
//! The inbox is worked out once, when the board is read (`Store::load`), and kept on the
//! `Board`, so drawing a frame never works it out again.

use std::collections::HashMap;

use crate::board::{Mention, Message, Repo, RepoId, ThreadId, Tone, tone_of};

/// Which kind of `@manager` message an alert is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NeedsYou,
    Awareness,
}

/// One `@manager` message for the owner, with the repo whose board it is on and the thread
/// it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub kind: Kind,
    pub repo: RepoId,
    pub thread: ThreadId,
    pub message: Message,
}

/// The owner's items in two parts, each newest first (by time, then message id): what needs
/// the owner and is not yet cleared, and what is for their awareness and not yet read or
/// resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inbox {
    pub need_you: Vec<Alert>,
    pub awareness: Vec<Alert>,
}

/// The item a `@manager` message is, read by the owner or not, on a thread open or
/// resolved: its kind, or `None` when the inbox does not list it: read (an awareness item
/// read, a need-you item cleared), on a resolved thread, or no item at all. The one
/// rule `Inbox::of` sorts by and `listed` counts by; the board's query (`MENTIONS_WHERE`)
/// reads only unread messages on open threads to begin with.
fn place(message: &Message, read: bool, open: bool) -> Option<Kind> {
    if read || !open {
        return None;
    }
    match tone_of(message) {
        Tone::NeedsYou => Some(Kind::NeedsYou),
        Tone::Awareness => Some(Kind::Awareness),
        Tone::Plain | Tone::Blocked => None,
    }
}

/// Whether the inbox lists `message` (`place`): a need-you or an unread awareness item on
/// an open thread. `tower repo remove` counts the items it removes by it.
pub fn listed(message: &Message, read: bool, open: bool) -> bool {
    place(message, read, open).is_some()
}

impl Inbox {
    /// Sorts `mentions` into their sections, by the threads and leads of `repos`; one the
    /// inbox does not list (`place`) is left out. A mention whose thread is not among them
    /// (it cannot be, as the board is read in one snapshot) is left out too.
    pub fn of(repos: &[Repo], mentions: Vec<Mention>) -> Inbox {
        let threads: HashMap<i64, (&Repo, bool)> = (repos.iter())
            .flat_map(|r| {
                r.threads
                    .iter()
                    .map(move |t| (t.id.0, (r, t.resolved.is_none())))
            })
            .collect();
        let mut inbox = Inbox::default();
        for mention in mentions {
            let Some(&(repo, open)) = threads.get(&mention.thread.0) else {
                continue;
            };
            let Some(kind) = place(&mention.message, mention.read, open) else {
                continue;
            };
            let section = match kind {
                Kind::NeedsYou => &mut inbox.need_you,
                Kind::Awareness => &mut inbox.awareness,
            };
            section.push(Alert {
                kind,
                repo: repo.id,
                thread: mention.thread,
                message: mention.message,
            });
        }
        for section in [&mut inbox.need_you, &mut inbox.awareness] {
            section.sort_by_key(|a| std::cmp::Reverse((a.message.at, a.message.id)));
        }
        inbox
    }

    /// The newest need-you item: the alert line's.
    pub fn newest(&self) -> Option<&Alert> {
        self.need_you.first()
    }

    /// A repo's badges, its lead's: its need-you items and its unread awareness items.
    pub fn badges(&self, repo: RepoId) -> (usize, usize) {
        let of = |s: &[Alert]| s.iter().filter(|a| a.repo == repo).count();
        (of(&self.need_you), of(&self.awareness))
    }

    /// Every item the inbox lists, in its order: need you, then awareness. Read items are
    /// not listed; they stay readable in their threads.
    pub fn all(&self) -> impl Iterator<Item = &Alert> {
        self.need_you.iter().chain(&self.awareness)
    }

    /// The unread awareness items on `thread`: what opening it marks read. Its need-you
    /// items stay: a question seen is not answered.
    pub fn unread_on(&self, thread: ThreadId) -> Vec<i64> {
        (self.awareness.iter())
            .filter(|a| a.thread == thread)
            .map(|a| a.message.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Lead, Mention, Resolution, Thread, ThreadId, check_message};

    fn lead(name: &str, session: &str) -> Lead {
        Lead {
            name: name.into(),
            session: session.into(),
            colour: None,
        }
    }

    fn repo(id: i64, lead: Lead, threads: Vec<Thread>) -> Repo {
        Repo {
            id: RepoId(id),
            path: None,
            link: None,
            lead: Some(lead),
            folded: false,
            done_folded: true,
            threads,
        }
    }

    fn thread(id: i64, resolved: bool) -> Thread {
        Thread {
            id: ThreadId(id),
            title: "t".into(),
            task: None,
            milestone: None,
            resolved: resolved.then(|| Resolution {
                by: "x".into(),
                at: 0,
            }),
            by: "x".into(),
            total: 1,
            authors: vec!["x".into()],
            messages: Vec::new(),
        }
    }

    fn mention(id: i64, thread: i64, author: &str, text: &str, at: i64, read: bool) -> Mention {
        Mention {
            thread: ThreadId(thread),
            message: Message {
                id,
                author: author.into(),
                text: text.into(),
                at,
            },
            read,
        }
    }

    /// R1 (cat-lead) with open T1 and resolved T2, R2 (checkout-lead) with open T3.
    fn repos() -> Vec<Repo> {
        vec![
            repo(
                1,
                lead("Catalogue", "cat-lead"),
                vec![thread(1, false), thread(2, true)],
            ),
            repo(2, lead("Checkout", "checkout-lead"), vec![thread(3, false)]),
        ]
    }

    fn inbox(mentions: Vec<Mention>) -> Inbox {
        Inbox::of(&repos(), mentions)
    }

    fn ids(section: &[Alert]) -> Vec<i64> {
        section.iter().map(|a| a.message.id).collect()
    }

    const ASK: &str = "question: @manager can you approve?";
    const DECISION: &str =
        "decision: the shelf shows 12 books, not 10. @manager for your awareness";

    #[test]
    fn sorts_by_the_one_classifier_and_counts_each_message() {
        let b = inbox(vec![
            mention(1, 1, "cat-lead", ASK, 100, false),
            // The same lead asking again is a second item: counting is by message.
            mention(2, 1, "cat-lead", ASK, 110, false),
            mention(3, 1, "cat-lead", DECISION, 120, false),
            // An untagged ask or reply, from anyone, is for the owner's awareness.
            mention(4, 3, "dev-2", "decision: x @manager approve?", 130, false),
            mention(5, 3, "checkout-lead", "@manager done: merged", 140, false),
            // A question from a worker needs the owner too.
            mention(6, 3, "dev-2", ASK, 150, false),
            // A quoted handle is no mention, so no item.
            mention(
                7,
                3,
                "checkout-lead",
                "question: is `@manager` red?",
                160,
                false,
            ),
        ]);
        let inbox = b.clone();
        assert_eq!(ids(&inbox.need_you), [6, 2, 1]);
        assert_eq!(ids(&inbox.awareness), [5, 4, 3]);
        // Every one agrees with Tone::of.
        for a in inbox.all() {
            let want = match a.kind {
                Kind::NeedsYou => Tone::NeedsYou,
                Kind::Awareness => Tone::Awareness,
            };
            assert_eq!(Tone::of(&a.message.text), want, "{}", a.message.id);
        }
    }

    #[test]
    fn the_newest_is_the_latest_by_time_then_by_id() {
        let b = inbox(vec![
            mention(7, 3, "dev-1", ASK, 300, false),
            mention(3, 1, "dev-2", ASK, 500, false),
            mention(9, 1, "qa", ASK, 200, false),
            // Same second as message 3: the higher id was posted later.
            mention(4, 3, "qa", ASK, 500, false),
            // A newer decision is not a need-you item.
            mention(10, 1, "cat-lead", DECISION, 900, false),
        ]);
        let inbox = b.clone();
        assert_eq!(inbox.newest().map(|a| a.message.id), Some(4));
        assert_eq!(ids(&inbox.need_you), [4, 3, 7, 9]);
    }

    #[test]
    fn nothing_needs_you_on_a_board_without_mentions_or_with_only_decisions() {
        let empty = inbox(Vec::new());
        assert_eq!(empty.clone(), Inbox::default());
        assert_eq!(empty.clone().newest(), None);
        let b = inbox(vec![mention(1, 1, "cat-lead", DECISION, 1, false)]);
        let inbox = b.clone();
        assert_eq!((inbox.newest(), inbox.awareness.len()), (None, 1));
    }

    #[test]
    fn resolving_takes_its_threads_items_out_of_the_inbox() {
        let b = inbox(vec![
            mention(1, 2, "dev-2", ASK, 100, false),
            mention(2, 2, "cat-lead", DECISION, 110, false),
            mention(3, 1, "dev-2", ASK, 120, false),
            // A question the owner cleared, on the resolved thread too.
            mention(4, 2, "cat-lead", ASK, 130, true),
        ]);
        let inbox = b.clone();
        assert_eq!(ids(&inbox.need_you), [3]);
        assert!(inbox.awareness.is_empty());
        // The inbox lists only the open question.
        let listed: Vec<i64> = inbox.all().map(|a| a.message.id).collect();
        assert_eq!(listed, [3]);
    }

    #[test]
    fn reading_takes_an_item_off_the_alerts_and_out_of_the_inbox() {
        let b = inbox(vec![
            mention(1, 1, "cat-lead", DECISION, 100, true),
            mention(2, 1, "cat-lead", DECISION, 110, false),
            // A need-you item the owner cleared leaves the alert line and the inbox: read.
            mention(3, 1, "dev-1", ASK, 120, true),
            mention(4, 1, "dev-1", ASK, 130, false),
        ]);
        let inbox = b.clone();
        assert_eq!(ids(&inbox.awareness), [2]);
        assert_eq!(ids(&inbox.need_you), [4]);
        assert_eq!(inbox.newest().map(|a| a.message.id), Some(4));
        // The inbox lists need you, then awareness; not the read items.
        let order: Vec<i64> = inbox.all().map(|a| a.message.id).collect();
        assert_eq!(order, [4, 2]);
    }

    #[test]
    fn opening_a_thread_reads_its_awareness_items_and_leaves_its_questions() {
        let b = inbox(vec![
            mention(1, 1, "cat-lead", DECISION, 100, false),
            mention(2, 1, "dev-1", ASK, 110, false),
            mention(3, 1, "cat-lead", "@manager done: merged", 120, false),
            // Read already, or on another thread: not this thread's to read.
            mention(4, 1, "cat-lead", DECISION, 130, true),
            mention(5, 3, "checkout-lead", DECISION, 140, false),
        ]);
        assert_eq!(b.unread_on(ThreadId(1)), [3, 1]);
        assert_eq!(b.unread_on(ThreadId(3)), [5]);
        assert!(b.unread_on(ThreadId(2)).is_empty());
    }

    #[test]
    fn listed_counts_exactly_what_the_inbox_lists() {
        // On T1 (open) and T2 (resolved), read and unread: an ask, a decision, plain text.
        for text in [ASK, DECISION, "decision: no mention"] {
            for (thread, open) in [(1, true), (2, false)] {
                for read in [false, true] {
                    let m = mention(1, thread, "cat-lead", text, 1, read);
                    let want = open && !read && text != "decision: no mention";
                    assert_eq!(listed(&m.message, read, open), want, "{text} {open} {read}");
                    assert_eq!(inbox(vec![m]).all().count(), usize::from(want));
                }
            }
        }
    }

    #[test]
    fn badges_count_each_leads_own_items() {
        let b = inbox(vec![
            mention(1, 1, "dev-2", ASK, 1, false),
            mention(2, 1, "qa", ASK, 2, false),
            mention(3, 1, "cat-lead", DECISION, 3, false),
            mention(4, 1, "cat-lead", DECISION, 4, true),
            mention(5, 3, "checkout-lead", DECISION, 5, false),
            mention(6, 2, "dev-2", ASK, 6, false),
        ]);
        let inbox = b.clone();
        assert_eq!(inbox.badges(RepoId(1)), (2, 1));
        assert_eq!(inbox.badges(RepoId(2)), (0, 1));
        assert_eq!(inbox.badges(RepoId(99)), (0, 0));
    }

    #[test]
    fn a_mention_on_a_thread_not_on_the_board_is_left_out() {
        let b = inbox(vec![mention(1, 99, "dev", ASK, 1, false)]);
        assert_eq!(b.clone(), Inbox::default());
        // Plain text that only looks close never counts.
        let text = check_message("message", "@manage r please").unwrap();
        let b = inbox(vec![mention(2, 1, "dev", &text, 1, false)]);
        assert_eq!(b.clone(), Inbox::default());
    }
}
