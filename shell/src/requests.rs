//! Ticket state shared by the host dialog callback and the framework's polling pass.
use std::collections::BTreeMap;
pub struct Requests<R, A> {
    next: u64,
    entries: BTreeMap<u64, (R, Option<A>)>,
}
impl<R, A> Default for Requests<R, A> {
    fn default() -> Self {
        Self {
            next: 1 << 63,
            entries: BTreeMap::new(),
        }
    }
}
impl<R, A> Requests<R, A> {
    pub fn issue(&mut self, request: R) -> u64 {
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("host dialog ticket exhausted");
        self.entries.insert(id, (request, None));
        id
    }
    pub fn finish(&mut self, id: u64, answer: A) {
        if let Some((_, slot)) = self.entries.get_mut(&id) {
            *slot = Some(answer);
        }
    }
    pub fn take_answer(&mut self, id: u64) -> Option<A> {
        if self
            .entries
            .get(&id)
            .is_some_and(|(_, answer)| answer.is_some())
        {
            self.entries.remove(&id).and_then(|(_, answer)| answer)
        } else {
            None
        }
    }
    pub fn forget(&mut self, id: u64) {
        self.entries.remove(&id);
    }
    pub fn pending(&self) -> Option<&R> {
        self.entries
            .values()
            .find_map(|(request, answer)| answer.is_none().then_some(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_stay_pending_until_their_own_answer_arrives() {
        let mut queue = Requests::<&str, Result<&str, &str>>::default();
        let first = queue.issue("save");
        let second = queue.issue("open");
        assert_ne!(first, second);
        assert_ne!(first & (1 << 63), 0);
        assert_eq!(queue.pending(), Some(&"save"));
        assert_eq!(queue.take_answer(first), None);
        queue.finish(second, Err("cancelled"));
        assert_eq!(queue.pending(), Some(&"save"));
        assert_eq!(queue.take_answer(second), Some(Err("cancelled")));
        queue.finish(first, Ok("/tmp/document.pdf"));
        assert_eq!(queue.pending(), None);
        assert_eq!(queue.take_answer(first), Some(Ok("/tmp/document.pdf")));
        assert_eq!(queue.take_answer(first), None);
    }
    #[test]
    fn forgotten_requests_drop_late_answers() {
        let mut queue = Requests::<&str, &str>::default();
        let id = queue.issue("save");
        queue.forget(id);
        queue.finish(id, "saved");
        assert_eq!(queue.pending(), None);
        assert_eq!(queue.take_answer(id), None);
    }
}
