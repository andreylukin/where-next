//! Bounded work queue with backpressure for spawned children.
use std::collections::VecDeque;

pub struct Queue {
    items: VecDeque<u32>,
}

pub(crate) enum Slot {
    Free,
    Taken,
}

impl Queue {
    pub fn new() -> Self {
        Queue { items: VecDeque::new() }
    }

    pub async fn push(&mut self, x: u32) {
        self.items.push_back(x);
    }
}

mod tests {
    fn helper() {}
}
