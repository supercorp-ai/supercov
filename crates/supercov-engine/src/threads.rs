//! What a joined thread hands back when it panicked.

/// A joined thread's panic, raised again in the thread that joined it, as
/// it was: `handle.join().unwrap_or_else(threads::rethrow)`.
pub(crate) fn rethrow<T>(panic: Box<dyn std::any::Any + Send>) -> T {
    std::panic::resume_unwind(panic)
}

/// `items.iter().map(work)`, spread over the machine's cores, in order.
pub(crate) fn parallel_map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(items.len().div_ceil(64).max(1));
    if threads <= 1 {
        return items.iter().map(work).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut done = std::thread::scope(|scope| {
        let workers = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            break done;
                        };
                        done.push((index, work(item)));
                    }
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap_or_else(rethrow))
            .collect::<Vec<_>>()
    });
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_joined_thread_s_panic_is_raised_again_as_it_was() {
        let raised = std::panic::catch_unwind(|| {
            std::thread::spawn(|| std::panic::panic_any("lost"))
                .join()
                .unwrap_or_else(super::rethrow::<()>)
        });
        assert_eq!(raised.unwrap_err().downcast_ref::<&str>(), Some(&"lost"));
    }
}
