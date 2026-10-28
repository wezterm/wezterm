use std::sync::atomic::{AtomicU8, Ordering};

const PENDING: u8 = 1;
const UPDATE_STATUS: u8 = 2;

#[derive(Default)]
pub(super) struct PendingTitleUpdate {
    state: AtomicU8,
}

impl PendingTitleUpdate {
    pub fn request(&self, update_status: bool) -> bool {
        let flags = PENDING | if update_status { UPDATE_STATUS } else { 0 };
        self.state.fetch_or(flags, Ordering::AcqRel) & PENDING == 0
    }

    pub fn take(&self) -> Option<bool> {
        let flags = self.state.swap(0, Ordering::AcqRel);
        if flags & PENDING == 0 {
            None
        } else {
            Some(flags & UPDATE_STATUS != 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn title_burst_schedules_one_refresh_with_status() {
        let update = PendingTitleUpdate::default();
        assert_eq!(update.take(), None);
        assert!(update.request(false));
        for index in 0..1000 {
            assert!(!update.request(index == 500));
        }
        assert_eq!(update.take(), Some(true));
        assert_eq!(update.take(), None);
    }

    #[test]
    fn next_burst_can_schedule_after_refresh_starts() {
        let update = PendingTitleUpdate::default();
        assert!(update.request(true));
        assert_eq!(update.take(), Some(true));
        assert!(update.request(false));
        assert!(!update.request(false));
        assert_eq!(update.take(), Some(false));
    }

    #[test]
    fn concurrent_notifications_queue_one_refresh() {
        let update = Arc::new(PendingTitleUpdate::default());
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let update = Arc::clone(&update);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    update.request(index == 7)
                })
            })
            .collect();
        let scheduled = threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>();
        assert_eq!(scheduled, 1);
        assert_eq!(update.take(), Some(true));
    }
}
