#[doc(hidden)]
// Injected code must be immune to the HOST crate's lint configuration: serde
// builds with `#![deny(warnings)]`, so this module's fully-qualified imports
// (required for no_std hosts) became hard errors as "unused imports".
#[allow(warnings)]
mod __SUPERCOV_MODULE__ {
    // The host crate may be `#![no_std]` or turn the prelude off outright
    // (`#![no_implicit_prelude]`): std is brought in explicitly and every name
    // below is imported in full.
    extern crate std;
    use std::cmp::{Ord, PartialEq, PartialOrd};
    use std::convert::From;
    use std::iter::{IntoIterator, Iterator};
    use std::marker::Sized;
    use std::option::Option::{self, None, Some};
    use std::result::Result::{self, Err, Ok};
    use std::sync::atomic::{AtomicPtr, AtomicU8, AtomicU32, AtomicU64, Ordering};
    use std::cell::Cell;
    use std::ops::ControlFlow;
    use std::task::Poll;

    // ------------------------------------------------------------------ layout
    //
    // One region per process, shared by every instrumented crate the process
    // links: each crate's copy of this module maps the same file, so one
    // sequence orders records across crates (an assertion in an integration
    // test and the library code it witnessed). A slot holds a first sighting
    // as `sequence << 32 | thread`; zero means not seen. The reader rebuilds
    // the records in execution order from the sequences.
    const HITS: usize = __SUPERCOV_HITS__;
    const ASSERTIONS: usize = __SUPERCOV_ASSERTIONS__;
    const DECISIONS: usize = __SUPERCOV_DECISIONS__;
    /// `#[cfg]` predicates of the workspace the target alone does not settle.
    const CFGS: usize = __SUPERCOV_CFGS__;
    const TOKEN: &[u8] = b"__SUPERCOV_TOKEN__";
    /// What `cfg!` says, in THIS crate, of the predicates in its own files:
    /// the compiler that builds the crate decides, with its features, its
    /// `test` and its build script's `--cfg`s. (A predicate naming a cfg the
    /// crate never declares is the author's own warning; it is not repeated
    /// here.)
    #[allow(unexpected_cfgs)]
    const COMPILED: &[(usize, bool)] = &[__SUPERCOV_CFG_TABLE__];

    const MAGIC: u64 = u64::from_le_bytes(*b"SCVRGN01");
    const VERSION: u32 = 1;
    const HEADER: usize = 128;
    const PAGE: usize = 4096;
    /// Distinct (vector, outcome) combinations kept per decision in the
    /// region; more go to the overflow file, nothing is dropped.
    const DECISION_ENTRIES: usize = 8;
    /// state u8, outcome u8, width u8, pad; first sighting u64 at 8; values
    /// u128 at 16 (two bits per condition: 0 unevaluated, 1 false, 2 true).
    const ENTRY: usize = 32;
    const MAX_CONDITIONS: usize = 64;

    /// A byte per predicate: bit 0 some crate built without it, bit 1 with it.
    const CFG_BYTES: usize = (CFGS + 7) / 8 * 8;
    const BODY: usize = HITS * 8 + ASSERTIONS * 8 + DECISIONS * DECISION_ENTRIES * ENTRY + CFG_BYTES;
    const BODY_PAGES: usize = (BODY + PAGE - 1) / PAGE;
    /// One bit per body page a first sighting touched, so the reader reads
    /// only those pages of a mostly-empty sparse file.
    const BITMAP: usize = ((BODY_PAGES + 63) / 64) * 8;
    const BODY_AT: usize = HEADER + BITMAP;
    const HITS_AT: usize = BODY_AT;
    const ASSERTIONS_AT: usize = HITS_AT + HITS * 8;
    const DECISIONS_AT: usize = ASSERTIONS_AT + ASSERTIONS * 8;
    const CFGS_AT: usize = DECISIONS_AT + DECISIONS * DECISION_ENTRIES * ENTRY;
    const REGION_BYTES: usize = ((BODY_AT + BODY + PAGE - 1) / PAGE) * PAGE;

    const SEQUENCE_AT: usize = 40;
    const OVERFLOW_AT: usize = 44;
    const EPOCH_AT: usize = 48;
    const FLAGS_AT: usize = 52;
    const TOKEN_AT: usize = 56;
    const CFGS_COUNT_AT: usize = 72;

    // ------------------------------------------------------------- the region
    //
    // NOTHING ON THE PROBE PATH MAY ALLOCATE. The crate under test may install
    // a `#[global_allocator]` whose body carries probes; an allocating probe
    // re-enters it. Opening the region is the one step that allocates (an
    // environment lookup, a path), and while it runs every probe declines:
    // a few observations at startup cost less than a recursion.
    static REGION: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
    /// 0 unopened, 1 opening, 2 open, 3 unavailable.
    static STATE: AtomicU8 = AtomicU8::new(0);

    #[inline(always)]
    fn region() -> Option<*mut u8> {
        let pointer = REGION.load(Ordering::Acquire);
        if !pointer.is_null() {
            return Some(pointer);
        }
        open_region()
    }

    #[inline(never)]
    fn open_region() -> Option<*mut u8> {
        if STATE
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let pointer = REGION.load(Ordering::Acquire);
            return if pointer.is_null() { None } else { Some(pointer) };
        }
        match channel::open() {
            Some(pointer) if initialise(pointer) => {
                settle_cfgs(pointer);
                REGION.store(pointer, Ordering::Release);
                STATE.store(2, Ordering::Release);
                channel::started(pointer);
                Some(pointer)
            }
            _ => {
                STATE.store(3, Ordering::Release);
                None
            }
        }
    }

    #[inline(always)]
    fn atomic_u32(base: *mut u8, offset: usize) -> &'static AtomicU32 {
        // SAFETY: offsets are 4-aligned within the page-aligned region, which
        // lives as long as the process.
        unsafe { &*base.add(offset).cast::<AtomicU32>() }
    }

    #[inline(always)]
    fn atomic_u64(base: *mut u8, offset: usize) -> &'static AtomicU64 {
        // SAFETY: offsets are 8-aligned within the page-aligned region.
        unsafe { &*base.add(offset).cast::<AtomicU64>() }
    }

    /// Write the header of a fresh region, or check an existing one is this
    /// instrumentation's. Counters are never written: another crate of the
    /// process may already be counting in this very file.
    fn initialise(base: *mut u8) -> bool {
        let magic = atomic_u64(base, 0);
        let fields: [(usize, u32); 9] = [
            (8, VERSION),
            (12, HEADER as u32),
            (16, HITS as u32),
            (20, ASSERTIONS as u32),
            (24, DECISIONS as u32),
            (28, DECISION_ENTRIES as u32),
            (32, BODY_AT as u32),
            (36, REGION_BYTES as u32),
            (CFGS_COUNT_AT, CFGS as u32),
        ];
        if magic.load(Ordering::Acquire) == 0 {
            let mut index = 0;
            while index < fields.len() {
                atomic_u32(base, fields[index].0).store(fields[index].1, Ordering::Relaxed);
                index += 1;
            }
            let mut byte = 0;
            while byte < TOKEN.len() && byte < 16 {
                // SAFETY: inside the header.
                unsafe { base.add(TOKEN_AT + byte).write_volatile(TOKEN[byte]) };
                byte += 1;
            }
            let _ = magic.compare_exchange(0, MAGIC, Ordering::AcqRel, Ordering::Acquire);
        }
        if magic.load(Ordering::Acquire) != MAGIC {
            return false;
        }
        let mut index = 0;
        while index < fields.len() {
            if atomic_u32(base, fields[index].0).load(Ordering::Relaxed) != fields[index].1 {
                return false;
            }
            index += 1;
        }
        true
    }

    /// Record which of this crate's gated code the compiler built. Every
    /// crate writes its own predicates; one file compiled into two crates
    /// may be built by one and not the other, so the bits accumulate.
    fn settle_cfgs(base: *mut u8) {
        let mut index = 0;
        while index < COMPILED.len() {
            let (ordinal, built) = COMPILED[index];
            if ordinal < CFGS {
                let offset = CFGS_AT + ordinal;
                // SAFETY: inside the region's predicate bytes.
                let byte = unsafe { &*base.add(offset).cast::<AtomicU8>() };
                byte.fetch_or(if built { 2 } else { 1 }, Ordering::AcqRel);
                touched(base, offset);
            }
            index += 1;
        }
    }

    // ---------------------------------------------------------------- threads
    //
    // The OS's own thread id, so every crate's copy of this module names a
    // thread the same way: an assertion witnesses only its own thread's
    // records. Cached per thread; the lookup is on the first-sighting path
    // only.
    std::thread_local! {
        static THREAD: Cell<u32> = const { Cell::new(0) };
    }

    #[inline(never)]
    fn thread() -> u32 {
        THREAD.with(|slot| {
            let known = slot.get();
            if known != 0 {
                return known;
            }
            let id = channel::thread_id().max(1);
            slot.set(id);
            id
        })
    }

    /// Claim the next sequence and store this thread's first sighting in
    /// `slot`, unless another thread got there first.
    #[inline(never)]
    fn sight(base: *mut u8, offset: usize) {
        let sequence = atomic_u32(base, SEQUENCE_AT).fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let value = (sequence << 32) | thread() as u64;
        if atomic_u64(base, offset)
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            touched(base, offset);
        }
    }

    #[inline(always)]
    fn touched(base: *mut u8, offset: usize) {
        let page = (offset - BODY_AT) / PAGE;
        // SAFETY: the bitmap covers every body page.
        let byte = unsafe { &*base.add(HEADER + page / 8).cast::<AtomicU8>() };
        byte.fetch_or(1 << (page % 8), Ordering::Relaxed);
        channel::persist(base, offset);
    }

    // ------------------------------------------------------------ hit probes

    /// A statement, function or branch alternative ran. Only the first
    /// sighting in a process carries information, and each test runs in a
    /// process of its own: after it, a probe is one load.
    #[inline(always)]
    pub fn hit(ordinal: u32) {
        let ordinal = ordinal as usize;
        if ordinal >= HITS {
            return;
        }
        let Some(base) = region() else { return };
        let offset = HITS_AT + ordinal * 8;
        if atomic_u64(base, offset).load(Ordering::Relaxed) == 0 {
            sight(base, offset);
        }
    }

    /// Control passed a statement that asserts, so every assertion in it
    /// held: a failing one panics instead. What this thread recorded before
    /// this point was in scope for that check.
    #[inline(always)]
    pub fn assertion(ordinal: u32) {
        let ordinal = ordinal as usize;
        if ordinal >= ASSERTIONS {
            return;
        }
        let Some(base) = region() else { return };
        let offset = ASSERTIONS_AT + ordinal * 8;
        if atomic_u64(base, offset).load(Ordering::Relaxed) == 0 {
            sight(base, offset);
        }
    }

    /// One arm of a match was selected. `ids` holds each arm's `not selected`
    /// and `selected` ordinals in source order, so every arm before `selected`
    /// was considered and passed over.
    #[inline(always)]
    pub fn arms(ids: &[u32], selected: usize) {
        for arm in 0..selected {
            if let Some(id) = ids.get(arm * 2) {
                hit(*id);
            }
        }
        if let Some(id) = ids.get(selected * 2 + 1) {
            hit(*id);
        }
    }

    /// The left operand of `&&` or `||`: it short-circuits when it equals
    /// `short_circuits_when`, otherwise the right operand is about to run.
    #[inline(always)]
    pub fn logical(left: bool, short_circuits_when: bool, short_circuit: u32, evaluated: u32) -> bool {
        hit(if left == short_circuits_when { short_circuit } else { evaluated });
        left
    }

    /// A `for` loop's iterator, recording on the first `next` whether the
    /// body ran at all. `size_hint` passes through so collection sizing is
    /// unchanged.
    pub struct ForLoop<I> {
        inner: I,
        first: bool,
        zero: u32,
        entered: u32,
    }

    impl<I: Iterator> Iterator for ForLoop<I> {
        type Item = I::Item;

        #[inline(always)]
        fn next(&mut self) -> Option<I::Item> {
            let item = self.inner.next();
            if self.first {
                self.first = false;
                hit(if item.is_some() { self.entered } else { self.zero });
            }
            item
        }

        #[inline(always)]
        fn size_hint(&self) -> (usize, Option<usize>) {
            self.inner.size_hint()
        }
    }

    #[inline(always)]
    pub fn for_loop<I: IntoIterator>(iterable: I, zero: u32, entered: u32) -> ForLoop<I::IntoIter> {
        ForLoop { inner: iterable.into_iter(), first: true, zero, entered }
    }

    /// A `while` body ran: clear the loop's flag on the first entry.
    #[inline(always)]
    pub fn entered(first: &mut bool, id: u32) {
        if *first {
            *first = false;
            hit(id);
        }
    }

    /// A `while` loop is over: a flag still set means the body never ran.
    #[inline(always)]
    pub fn zero_iterations(first: bool, id: u32) {
        if first {
            hit(id);
        }
    }

    /// The operand of `?`, recording which way the operator goes. Every type
    /// `?` accepts on stable Rust implements this.
    pub trait TryProbe: Sized {
        fn probe(self, continued: u32, returned: u32) -> Self;
    }

    impl<T> TryProbe for Option<T> {
        #[inline(always)]
        fn probe(self, continued: u32, returned: u32) -> Self {
            hit(if self.is_some() { continued } else { returned });
            self
        }
    }

    impl<T, E> TryProbe for Result<T, E> {
        #[inline(always)]
        fn probe(self, continued: u32, returned: u32) -> Self {
            hit(if self.is_ok() { continued } else { returned });
            self
        }
    }

    impl<B, C> TryProbe for ControlFlow<B, C> {
        #[inline(always)]
        fn probe(self, continued: u32, returned: u32) -> Self {
            hit(match self {
                ControlFlow::Continue(_) => continued,
                _ => returned,
            });
            self
        }
    }

    impl<T, E> TryProbe for Poll<Result<T, E>> {
        #[inline(always)]
        fn probe(self, continued: u32, returned: u32) -> Self {
            hit(match self {
                Poll::Ready(Err(_)) => returned,
                _ => continued,
            });
            self
        }
    }

    impl<T, E> TryProbe for Poll<Option<Result<T, E>>> {
        #[inline(always)]
        fn probe(self, continued: u32, returned: u32) -> Self {
            hit(match self {
                Poll::Ready(Some(Err(_))) => returned,
                _ => continued,
            });
            self
        }
    }

    // -------------------------------------------------------------- decisions
    //
    // A frame lives on the stack of the function whose decision it records,
    // and a recursive descent parser carries every one of them down every
    // level: serde_json's recursion-limit test overflowed the test thread's
    // stack when frames were large. Values are two bits each in a u128 and
    // reached marks one bit each, so a frame is 40 bytes.
    pub struct DecisionFrame {
        id: u32,
        values: u128,
        /// Let-chain conditions the evaluation got to: a `let` cannot be
        /// wrapped, so it is marked reached instead and resolved later.
        reached: u64,
        conditions: usize,
        recordable: bool,
    }

    impl DecisionFrame {
        pub fn new(id: u32, conditions: usize) -> Self {
            Self {
                id,
                values: 0,
                reached: 0,
                conditions,
                recordable: conditions <= MAX_CONDITIONS && (id as usize) < DECISIONS,
            }
        }

        #[inline(always)]
        fn value(&self, index: usize) -> u8 {
            ((self.values >> (2 * index)) & 3) as u8
        }

        #[inline(always)]
        fn set_value(&mut self, index: usize, value: u8) {
            let shift = 2 * index;
            self.values = (self.values & !(3u128 << shift)) | ((value as u128) << shift);
        }

        #[inline(always)]
        fn is_reached(&self, index: usize) -> bool {
            index < MAX_CONDITIONS && (self.reached >> index) & 1 == 1
        }
    }

    /// A let chain got to condition `index` (0 marks the chain evaluated at
    /// all). Always true, so it sits in the chain as an operand.
    #[inline(always)]
    pub fn reached(frame: &mut DecisionFrame, index: usize) -> bool {
        if index < MAX_CONDITIONS {
            frame.reached |= 1u64 << index;
        }
        true
    }

    /// A let chain decided. A chain tries its conditions in order and stops
    /// at the first that fails, so every reached `let` before the last
    /// reached condition held, the last one held when the chain was taken and
    /// failed when it was not, and conditions never reached stay unevaluated.
    /// `operators` lists the `&&` whose left side holds a `let`, by the index
    /// of their right side's first condition: reached means the operator
    /// evaluated its right side, otherwise it short-circuited. The frame then
    /// resets for the next evaluation, which a `while let` makes every turn.
    pub fn decision_chain(frame: &mut DecisionFrame, outcome: bool, operators: &[(usize, u32, u32)]) {
        if !frame.is_reached(0) {
            return;
        }
        let conditions = frame.conditions.min(MAX_CONDITIONS);
        let mut last = 0;
        let mut index = 0;
        while index < conditions {
            if frame.is_reached(index) || frame.value(index) != 0 {
                last = index;
            }
            index += 1;
        }
        let mut index = 0;
        while index < conditions {
            if frame.value(index) == 0 && frame.is_reached(index) {
                frame.set_value(index, if index < last || outcome { 2 } else { 1 });
            }
            index += 1;
        }
        for (first, short_circuit, evaluated) in operators {
            let got_there = frame.is_reached(*first) || (*first < MAX_CONDITIONS && frame.value(*first) != 0);
            hit(if got_there { *evaluated } else { *short_circuit });
        }
        decision(outcome, frame);
        frame.values = 0;
        frame.reached = 0;
    }

    // Anything `!` turns into a bool: `assert!(was_seen)` compiles with a
    // `&bool` (tokio iterates `for was_seen in &seen`), since the macro only
    // negates its operand.
    #[inline(always)]
    pub fn condition<V: std::ops::Not<Output = bool>>(value: V, frame: &mut DecisionFrame, index: usize) -> bool {
        let value = !!value;
        if index < frame.conditions && index < MAX_CONDITIONS {
            frame.set_value(index, if value { 2 } else { 1 });
        }
        value
    }

    #[inline(always)]
    pub fn decision(value: bool, frame: &mut DecisionFrame) -> bool {
        if frame.recordable {
            record_decision(value, frame);
        }
        value
    }

    /// A decision's (vector, outcome) combinations, each recorded on its
    /// first sighting. A combination is compared whole -- a hash collision
    /// would silently drop a distinct vector and understate MC/DC.
    /// Entries go 0 (empty) -> 1 (being written) -> 2 (readable); a reader
    /// that meets one mid-write looks again.
    #[inline(never)]
    fn record_decision(outcome: bool, frame: &DecisionFrame) {
        let Some(base) = region() else { return };
        let width = if frame.conditions < MAX_CONDITIONS { frame.conditions } else { MAX_CONDITIONS };
        let tag = (if outcome { 2u8 } else { 1u8 }, width as u8);
        let area = DECISIONS_AT + frame.id as usize * DECISION_ENTRIES * ENTRY;
        let mut slot = 0;
        while slot < DECISION_ENTRIES {
            let entry = area + slot * ENTRY;
            // SAFETY: entries are 8-aligned inside the region.
            let state = unsafe { &*base.add(entry).cast::<AtomicU8>() };
            match state.load(Ordering::Acquire) {
                2 => {
                    // SAFETY: a readable entry is never written again. The
                    // values are only 8-aligned (the table follows the 8-byte
                    // slots), and x86_64 loads a u128 with an aligned SSE move
                    // that faults on anything less than 16: unaligned access.
                    let same = unsafe {
                        base.add(entry + 1).read() == tag.0
                            && base.add(entry + 2).read() == tag.1
                            && base.add(entry + 16).cast::<u128>().read_unaligned() == frame.values
                    };
                    if same {
                        return;
                    }
                    slot += 1;
                }
                0 => {
                    if state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
                        continue;
                    }
                    let sequence = atomic_u32(base, SEQUENCE_AT).fetch_add(1, Ordering::Relaxed) as u64 + 1;
                    // SAFETY: this thread owns the entry until it publishes 2.
                    unsafe {
                        base.add(entry + 1).write(tag.0);
                        base.add(entry + 2).write(tag.1);
                        base.add(entry + 16).cast::<u128>().write_unaligned(frame.values);
                    }
                    atomic_u64(base, entry + 8).store((sequence << 32) | thread() as u64, Ordering::Relaxed);
                    state.store(2, Ordering::Release);
                    touched(base, entry);
                    return;
                }
                _ => {
                    // Another thread is writing this entry: wait for it.
                    std::hint::spin_loop();
                }
            }
        }
        overflow::decision(base, frame.id, outcome, width, frame.values);
    }

    // --------------------------------------------------------------- overflow
    //
    // A decision that shows more than DECISION_ENTRIES distinct combinations
    // in one process keeps the rest in an append-only file beside the region,
    // deduplicated in memory so a hot loop writes each once.
    mod overflow {
        extern crate std;
        use super::*;

        const SLOTS: usize = 1 << 11;
        struct Slot {
            state: AtomicU8,
            key: AtomicU64,
            values: [AtomicU64; 2],
        }
        static SEEN: [Slot; SLOTS] = [const {
            Slot { state: AtomicU8::new(0), key: AtomicU64::new(0), values: [const { AtomicU64::new(0) }; 2] }
        }; SLOTS];

        fn first(id: u32, outcome: bool, width: usize, values: u128) -> bool {
            let key = ((id as u64) << 16) | ((outcome as u64) << 8) | width as u64 | (1 << 63);
            let low = values as u64;
            let high = (values >> 64) as u64;
            let mut slot = ((id as usize).wrapping_mul(2654435761) ^ low as usize) & (SLOTS - 1);
            let mut attempts = 0;
            while attempts < 16 {
                let cell = &SEEN[slot];
                match cell.state.load(Ordering::Acquire) {
                    2 => {
                        if cell.key.load(Ordering::Relaxed) == key
                            && cell.values[0].load(Ordering::Relaxed) == low
                            && cell.values[1].load(Ordering::Relaxed) == high
                        {
                            return false;
                        }
                    }
                    0 => {
                        if cell.state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                            cell.key.store(key, Ordering::Relaxed);
                            cell.values[0].store(low, Ordering::Relaxed);
                            cell.values[1].store(high, Ordering::Relaxed);
                            cell.state.store(2, Ordering::Release);
                            return true;
                        }
                        continue;
                    }
                    _ => {}
                }
                slot = (slot + 1) & (SLOTS - 1);
                attempts += 1;
            }
            // A crowded table writes the record again: a duplicate costs
            // time, never correctness.
            true
        }

        #[inline(never)]
        pub fn decision(base: *mut u8, id: u32, outcome: bool, width: usize, values: u128) {
            if !first(id, outcome, width, values) {
                return;
            }
            atomic_u32(base, OVERFLOW_AT).fetch_add(1, Ordering::Relaxed);
            let sequence = atomic_u32(base, SEQUENCE_AT).fetch_add(1, Ordering::Relaxed) + 1;
            let mut record = [0u8; 128];
            let mut length = 0;
            let mut fits = push(&mut record, &mut length, b"D\t")
                && number(&mut record, &mut length, sequence as u64)
                && push(&mut record, &mut length, b"\t")
                && number(&mut record, &mut length, thread() as u64)
                && push(&mut record, &mut length, b"\t")
                && number(&mut record, &mut length, id as u64)
                && push(&mut record, &mut length, b"\t");
            let mut index = 0;
            while index < width {
                fits = fits && push(&mut record, &mut length, &[b'0' + ((values >> (2 * index)) & 3) as u8]);
                index += 1;
            }
            fits = fits
                && push(&mut record, &mut length, b"\t")
                && push(&mut record, &mut length, if outcome { b"1" } else { b"0" })
                && push(&mut record, &mut length, b"\n");
            if fits {
                channel::append_overflow(&record[..length]);
            }
        }

        fn push(record: &mut [u8; 128], length: &mut usize, bytes: &[u8]) -> bool {
            let Some(slice) = record.get_mut(*length..*length + bytes.len()) else { return false };
            slice.copy_from_slice(bytes);
            *length += bytes.len();
            true
        }

        fn number(record: &mut [u8; 128], length: &mut usize, value: u64) -> bool {
            let mut digits = [0u8; 20];
            let mut count = 0;
            let mut rest = value;
            loop {
                digits[count] = b'0' + (rest % 10) as u8;
                count += 1;
                rest /= 10;
                if rest == 0 {
                    break;
                }
            }
            let mut index = count;
            while index > 0 {
                index -= 1;
                if !push(record, length, &[digits[index]]) {
                    return false;
                }
            }
            true
        }
    }

    // ---------------------------------------------------------------- channel
    //
    // Where the region lives. On a host with files it is a file the test's
    // evidence directory holds, mapped shared: its pages belong to the file,
    // so a process that dies keeps everything it recorded. In a container
    // across a VM boundary a mapping becomes visible outside only once it is
    // synced, so a flusher syncs it every few milliseconds and stamps the
    // epoch a harvest waits for. Anywhere else (wasm) the region is static
    // memory a host reads through the exported functions.
    #[cfg(any(unix, windows))]
    mod channel {
        extern crate std;
        use super::*;
        use std::fs::OpenOptions;
        use std::sync::Mutex;
        use std::sync::OnceLock;
        use std::io::Write as _;

        fn path(extension: &str) -> Option<std::path::PathBuf> {
            let directory = std::env::var_os("SUPERCOV_RUST_EVIDENCE_DIR")?;
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).ok()?;
            let token = std::str::from_utf8(TOKEN).ok()?;
            Some(directory.join(std::format!("{}-{}.{}", token, std::process::id(), extension)))
        }

        pub fn open() -> Option<*mut u8> {
            let path = path("region")?;
            let file = OpenOptions::new().read(true).write(true).create(true).open(&path).ok()?;
            let metadata = file.metadata().ok()?;
            if !metadata.file_type().is_file() {
                return None;
            }
            if metadata.len() < REGION_BYTES as u64 {
                // A sparse file: pages no probe touches take no space.
                file.set_len(REGION_BYTES as u64).ok()?;
            }
            platform::map(&file, REGION_BYTES)
        }

        pub fn started(base: *mut u8) {
            if std::env::var_os("SUPERCOV_CONTAINER").is_none() {
                return;
            }
            // One flusher per process: every crate maps the same file.
            if atomic_u32(base, FLAGS_AT).fetch_or(1, Ordering::AcqRel) & 1 != 0 {
                return;
            }
            let address = base as usize;
            let _ = std::thread::Builder::new()
                .name(std::string::String::from("supercov-flush"))
                .spawn(move || loop {
                    let base = address as *mut u8;
                    platform::sync(base, REGION_BYTES);
                    atomic_u32(base, EPOCH_AT).fetch_add(1, Ordering::Release);
                    std::thread::sleep(std::time::Duration::from_millis(4));
                });
        }

        pub fn thread_id() -> u32 {
            platform::thread_id()
        }

        /// The region is the file: every store is in it already.
        #[inline(always)]
        pub fn persist(_: *mut u8, _: usize) {}

        static OVERFLOW: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        static OPENING: AtomicU8 = AtomicU8::new(0);

        pub fn append_overflow(record: &[u8]) {
            let writer = match OVERFLOW.get() {
                Some(writer) => writer,
                None => {
                    // Opening allocates; a probe it re-enters declines.
                    if OPENING.swap(1, Ordering::AcqRel) != 0 {
                        return;
                    }
                    let writer = OVERFLOW.get_or_init(|| {
                        let file = OpenOptions::new().create(true).append(true).open(path("overflow")?).ok()?;
                        let guarded = Mutex::new(file);
                        // A std Mutex boxes its platform mutex on the first
                        // lock; do that here, where re-entry is harmless.
                        std::mem::drop(guarded.lock());
                        Some(guarded)
                    });
                    OPENING.store(0, Ordering::Release);
                    writer
                }
            };
            if let Some(writer) = writer {
                if let Ok(mut file) = writer.lock() {
                    let _ = file.write_all(record);
                }
            }
        }

        #[cfg(unix)]
        mod platform {
            extern crate std;
            use super::*;
            use std::os::fd::AsRawFd as _;

            unsafe extern "C" {
                fn mmap(address: *mut std::ffi::c_void, length: usize, protection: i32, flags: i32, file: i32, offset: isize) -> *mut std::ffi::c_void;
                fn msync(address: *mut std::ffi::c_void, length: usize, flags: i32) -> i32;
            }

            pub fn map(file: &std::fs::File, length: usize) -> Option<*mut u8> {
                // PROT_READ | PROT_WRITE, MAP_SHARED: the mapping is the file.
                // SAFETY: the descriptor is valid for the call; the mapping
                // outlives it.
                let pointer = unsafe { mmap(std::ptr::null_mut(), length, 1 | 2, 1, file.as_raw_fd(), 0) };
                if pointer as isize == -1 || pointer.is_null() {
                    return None;
                }
                Some(pointer.cast::<u8>())
            }

            pub fn sync(base: *mut u8, length: usize) {
                #[cfg(target_os = "linux")]
                const MS_SYNC: i32 = 4;
                #[cfg(not(target_os = "linux"))]
                const MS_SYNC: i32 = 0x10;
                // SAFETY: the mapping is the one `map` returned.
                let _ = unsafe { msync(base.cast(), length, MS_SYNC) };
            }

            // `gettid()` the libc function only exists from glibc 2.30; the
            // system call has always been there.
            #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86", target_arch = "arm")))]
            pub fn thread_id() -> u32 {
                unsafe extern "C" {
                    fn syscall(number: isize, ...) -> isize;
                }
                #[cfg(target_arch = "x86_64")]
                const SYS_GETTID: isize = 186;
                #[cfg(any(target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64"))]
                const SYS_GETTID: isize = 178;
                #[cfg(any(target_arch = "x86", target_arch = "arm"))]
                const SYS_GETTID: isize = 224;
                // SAFETY: gettid takes no arguments and cannot fail.
                unsafe { syscall(SYS_GETTID) as u32 }
            }

            #[cfg(target_vendor = "apple")]
            pub fn thread_id() -> u32 {
                unsafe extern "C" {
                    fn pthread_threadid_np(thread: *mut std::ffi::c_void, id: *mut u64) -> i32;
                }
                let mut id = 0u64;
                // SAFETY: a null thread is the calling thread.
                unsafe { pthread_threadid_np(std::ptr::null_mut(), &mut id) };
                id as u32
            }

            #[cfg(not(any(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "riscv64", target_arch = "loongarch64", target_arch = "x86", target_arch = "arm")), target_vendor = "apple")))]
            pub fn thread_id() -> u32 {
                unsafe extern "C" {
                    fn pthread_self() -> usize;
                }
                // SAFETY: no failure.
                let id = unsafe { pthread_self() };
                (id ^ (id >> 32)) as u32
            }
        }

        #[cfg(windows)]
        mod platform {
            extern crate std;
            use super::*;
            use std::os::windows::io::AsRawHandle as _;

            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn CreateFileMappingW(file: *mut std::ffi::c_void, attributes: *mut std::ffi::c_void, protection: u32, high: u32, low: u32, name: *const u16) -> *mut std::ffi::c_void;
                fn MapViewOfFile(mapping: *mut std::ffi::c_void, access: u32, high: u32, low: u32, length: usize) -> *mut std::ffi::c_void;
                fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
                fn FlushViewOfFile(address: *const std::ffi::c_void, length: usize) -> i32;
                fn GetCurrentThreadId() -> u32;
            }

            pub fn map(file: &std::fs::File, length: usize) -> Option<*mut u8> {
                // PAGE_READWRITE; a zero maximum size maps the whole file.
                // SAFETY: the handle is valid for the call.
                let section = unsafe { CreateFileMappingW(file.as_raw_handle(), std::ptr::null_mut(), 0x04, 0, 0, std::ptr::null()) };
                if section.is_null() {
                    return None;
                }
                // FILE_MAP_READ | FILE_MAP_WRITE. The view keeps the section
                // alive after its handle closes.
                // SAFETY: the section was just created.
                let view = unsafe { MapViewOfFile(section, 0x0004 | 0x0002, 0, 0, length) };
                // SAFETY: closing our handle; the view holds its own reference.
                unsafe { CloseHandle(section) };
                if view.is_null() {
                    return None;
                }
                Some(view.cast::<u8>())
            }

            pub fn sync(base: *mut u8, length: usize) {
                // SAFETY: the view is the one `map` returned.
                let _ = unsafe { FlushViewOfFile(base.cast(), length) };
            }

            pub fn thread_id() -> u32 {
                // SAFETY: no failure.
                unsafe { GetCurrentThreadId() }
            }
        }
    }

    #[cfg(not(any(unix, windows, target_os = "wasi")))]
    mod channel {
        extern crate std;
        use super::*;

        // Static memory, 8-aligned: a wasm host reads it through the exports
        // below and clears it between tests.
        #[repr(C, align(4096))]
        struct Static([u8; REGION_BYTES]);
        static mut REGION_MEMORY: Static = Static([0; REGION_BYTES]);

        pub fn open() -> Option<*mut u8> {
            // SAFETY: the only access is through the atomics the probes use.
            Some(unsafe { std::ptr::addr_of_mut!(REGION_MEMORY.0) as *mut u8 })
        }

        pub fn started(_: *mut u8) {}

        pub fn thread_id() -> u32 {
            1
        }

        pub fn append_overflow(_: &[u8]) {
            // Nowhere to write: the header's overflow count says how many
            // combinations were not kept.
        }

        /// A host reads the memory itself.
        #[inline(always)]
        pub fn persist(_: *mut u8, _: usize) {}
    }

    // WASI: a process with a filesystem but no mapping of it. The region is
    // static memory, and each first sighting is written through to a file in
    // the evidence directory the runner opens to the module (`wasmtime
    // --dir`): one write per probe per process, as it happens, so a module
    // that traps keeps what it ran.
    #[cfg(target_os = "wasi")]
    mod channel {
        extern crate std;
        use super::*;
        use std::fs::OpenOptions;
        use std::io::{Seek as _, SeekFrom, Write as _};
        use std::sync::{Mutex, OnceLock};

        #[repr(C, align(4096))]
        struct Static([u8; REGION_BYTES]);
        static mut REGION_MEMORY: Static = Static([0; REGION_BYTES]);
        static REGION_FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        static OVERFLOW_FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
        static STEM: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

        /// `<token>-w<clock>`: WASI has no process id, and the clock tells
        /// one process's files from another's.
        fn stem() -> Option<&'static std::path::PathBuf> {
            STEM.get_or_init(|| {
                let directory = std::env::var_os("SUPERCOV_RUST_EVIDENCE_DIR")?;
                let token = std::str::from_utf8(TOKEN).ok()?;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_nanos();
                Some(std::path::PathBuf::from(directory).join(std::format!("{}-w{:x}", token, stamp)))
            })
            .as_ref()
        }

        fn file(extension: &str, append: bool) -> Option<Mutex<std::fs::File>> {
            let mut path = stem()?.clone();
            path.set_extension(extension);
            let mut options = OpenOptions::new();
            if append {
                options.create(true).append(true);
            } else {
                options.create(true).read(true).write(true).truncate(true);
            }
            options.open(path).ok().map(Mutex::new)
        }

        pub fn open() -> Option<*mut u8> {
            let region = REGION_FILE.get_or_init(|| {
                let file = file("region", false)?;
                file.lock().ok()?.set_len(REGION_BYTES as u64).ok()?;
                Some(file)
            });
            region.as_ref()?;
            // SAFETY: the only access is through the atomics the probes use.
            Some(unsafe { std::ptr::addr_of_mut!(REGION_MEMORY.0) as *mut u8 })
        }

        fn write_at(base: *mut u8, offset: usize, length: usize) {
            let Some(Some(file)) = REGION_FILE.get() else {
                return;
            };
            let end = if offset + length < REGION_BYTES { offset + length } else { REGION_BYTES };
            // SAFETY: inside the region.
            let bytes = unsafe { std::slice::from_raw_parts(base.add(offset), end - offset) };
            if let Ok(mut file) = file.lock() {
                if file.seek(SeekFrom::Start(offset as u64)).is_ok() {
                    let _ = file.write_all(bytes);
                }
            }
        }

        /// The header, once it is written.
        pub fn started(base: *mut u8) {
            write_at(base, 0, BODY_AT);
        }

        pub fn thread_id() -> u32 {
            1
        }

        /// What a first sighting stored -- a slot, or a decision's whole
        /// entry -- and the bitmap byte that says its page was touched.
        pub fn persist(base: *mut u8, offset: usize) {
            write_at(base, offset, ENTRY);
            write_at(base, HEADER + (offset - BODY_AT) / PAGE / 8, 1);
        }

        pub fn append_overflow(record: &[u8]) {
            let writer = OVERFLOW_FILE.get_or_init(|| file("overflow", true));
            if let Some(writer) = writer {
                if let Ok(mut file) = writer.lock() {
                    let _ = file.write_all(record);
                }
            }
        }
    }

    /// The region's address and length, for a host that reads memory (a wasm
    /// runtime through its inspector). Named by crate key: each instrumented
    /// crate of a module keeps a region of its own.
    #[cfg(not(any(unix, windows)))]
    #[unsafe(no_mangle)]
    pub extern "C" fn __SUPERCOV_REGION_EXPORT__() -> u64 {
        match region() {
            Some(base) => ((base as usize as u64) << 32) | REGION_BYTES as u64,
            None => 0,
        }
    }

    /// Clear what the host has read: the next test starts from nothing.
    #[cfg(not(any(unix, windows)))]
    #[unsafe(no_mangle)]
    pub extern "C" fn __SUPERCOV_CLEAR_EXPORT__() {
        if let Some(base) = region() {
            let mut offset = HEADER;
            while offset < REGION_BYTES {
                atomic_u64(base, offset).store(0, Ordering::Relaxed);
                offset += 8;
            }
            atomic_u32(base, SEQUENCE_AT).store(0, Ordering::Relaxed);
            atomic_u32(base, OVERFLOW_AT).store(0, Ordering::Relaxed);
            // What the compiler built is no test's record: write it again.
            settle_cfgs(base);
        }
    }
}
