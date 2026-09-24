//! Which instruments each feed streams: pinned ones (open exposure) always, then the most
//! recently queried, up to the feed's cap.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::watch;

use crate::domain::InstrumentId;

pub fn select(pinned: &[InstrumentId], recent: &VecDeque<InstrumentId>, cap: usize) -> Vec<InstrumentId> {
    let mut out: Vec<InstrumentId> = Vec::new();
    for id in pinned.iter().chain(recent.iter()) {
        if out.len() >= cap {
            break;
        }
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

pub struct Subscriptions {
    cap: usize,
    pinned: Mutex<Vec<InstrumentId>>,
    recent: Mutex<VecDeque<InstrumentId>>,
    tx: watch::Sender<Vec<InstrumentId>>,
}

impl Subscriptions {
    pub fn new(cap: usize) -> (Self, watch::Receiver<Vec<InstrumentId>>) {
        let (tx, rx) = watch::channel(Vec::new());
        let subs = Subscriptions { cap, pinned: Mutex::new(Vec::new()), recent: Mutex::new(VecDeque::new()), tx };
        (subs, rx)
    }

    /// Mark `id` as just queried.
    pub fn touch(&self, id: &InstrumentId) {
        {
            let mut recent = self.recent.lock().unwrap();
            recent.retain(|x| x != id);
            recent.push_front(id.clone());
            recent.truncate(self.cap);
        }
        self.publish();
    }

    /// Replace the set of instruments that must stay subscribed.
    // ponytail: pins beyond the cap are dropped; only matters when open exposure exceeds a feed's cap.
    pub fn pin(&self, ids: Vec<InstrumentId>) {
        *self.pinned.lock().unwrap() = ids;
        self.publish();
    }

    pub fn current(&self) -> Vec<InstrumentId> {
        self.tx.borrow().clone()
    }

    fn publish(&self) {
        let set = select(&self.pinned.lock().unwrap(), &self.recent.lock().unwrap(), self.cap);
        self.tx.send_if_modified(|cur| {
            let changed = *cur != set;
            if changed {
                *cur = set;
            }
            changed
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn ids(s: &[&str]) -> Vec<InstrumentId> {
        s.iter().map(|x| format!("UPBIT:{x}").parse().unwrap()).collect()
    }

    #[test]
    fn pinned_come_first_then_recent_up_to_cap() {
        let recent: VecDeque<InstrumentId> = ids(&["C", "A", "D"]).into();
        assert_eq!(select(&ids(&["A", "B"]), &recent, 3), ids(&["A", "B", "C"]));
    }

    #[test]
    fn heavy_querying_never_evicts_a_pinned_instrument() {
        let (subs, rx) = Subscriptions::new(3);
        subs.pin(ids(&["HELD"]));
        for x in ["Q1", "Q2", "Q3", "Q4", "Q5"] {
            subs.touch(&ids(&[x])[0]);
        }
        let set = rx.borrow().clone();
        assert_eq!(set.len(), 3);
        assert_eq!(set[0], ids(&["HELD"])[0]);
        assert_eq!(&set[1..], &ids(&["Q5", "Q4"])[..]);
    }

    #[test]
    fn unchanged_set_does_not_notify() {
        let (subs, mut rx) = Subscriptions::new(3);
        subs.touch(&ids(&["A"])[0]);
        rx.borrow_and_update();
        subs.touch(&ids(&["A"])[0]);
        assert!(!rx.has_changed().unwrap());
        assert_eq!(subs.current(), ids(&["A"]));
    }
}
