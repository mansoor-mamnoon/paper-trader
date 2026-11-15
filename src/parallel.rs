//! Bounded workers for independent questions, with results kept in input order.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Process independent items concurrently, preserving input order even when calls finish
/// out of order. No database connection crosses a worker boundary.
pub fn map<T: Sync, R: Send>(items: &[T], workers: usize, work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if workers <= 1 || items.len() <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let results = Mutex::new((0..items.len()).map(|_| None).collect::<Vec<Option<R>>>());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(items.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    results.lock().unwrap()[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn workers_overlap_and_results_keep_their_original_order() {
        let barrier = Barrier::new(4);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let results = map(&[3, 2, 1, 0], 4, |value| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            barrier.wait();
            std::thread::sleep(std::time::Duration::from_millis(*value));
            active.fetch_sub(1, Ordering::SeqCst);
            value * 2
        });
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        assert_eq!(results, [6, 4, 2, 0]);
    }
}
