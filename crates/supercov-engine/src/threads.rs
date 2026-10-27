//! What a joined thread hands back when it panicked.

/// A joined thread's panic, raised again in the thread that joined it, as
/// it was: `handle.join().unwrap_or_else(threads::rethrow)`.
pub(crate) fn rethrow<T>(panic: Box<dyn std::any::Any + Send>) -> T {
    std::panic::resume_unwind(panic)
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
