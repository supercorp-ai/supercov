#[doc(hidden)]
#[allow(dead_code)]
mod __SUPERCOV_MODULE__ {
    use std::cell::Cell;
    use std::fs::OpenOptions;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    const MAGIC: &[u8; 8] = b"SCVRUST2";
    const VERSION: u32 = 2;
    const HEADER_SIZE: usize = 128;
    const DESCRIPTOR_SIZE: usize = 40;
    const ENDIAN_MARKER: u32 = 0x0102_0304;
    const NEXT_DESCRIPTOR_OFFSET: usize = 32;
    const NEXT_PAYLOAD_OFFSET: usize = 40;
    const DROPPED_OFFSET: usize = 48;
    const TOKEN_OFFSET: usize = 56;
    const TOKEN_SIZE: usize = 16;
    const ATTACHMENTS_OFFSET: usize = 72;
    const NEXT_PHASE_OFFSET: usize = 80;
    const CONTEXT_ENV: &str = "SUPERCOV_RUST_CONTEXT_ID";
    const KIND_HIT: u8 = 1;
    const KIND_DECISION: u8 = 2;
    const KIND_ORDINAL_HIT: u8 = 3;
    const KIND_PHASE: u8 = 4;
    const DECISION_ID_PREFIX: &[u8; 12] = b"rs:decision:";
    const DECISION_ID_LENGTH: u32 = 36;
    const NO_CONTEXT_OVERRIDE: u64 = u64::MAX;

    std::thread_local! {
        static CONTEXT_OVERRIDE: Cell<Option<u64>> = const { Cell::new(None) };
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod inherited_thread_context {
        use std::{ffi::c_void, mem, sync::OnceLock};

        type StartRoutine = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
        type PthreadCreate = unsafe extern "C" fn(
            *mut usize,
            *const c_void,
            StartRoutine,
            *mut c_void,
        ) -> i32;

        struct ThreadStart {
            routine: StartRoutine,
            argument: *mut c_void,
            context: u64,
        }

        #[cfg(target_os = "macos")]
        #[link(name = "System")]
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
        }

        #[cfg(target_os = "linux")]
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
        }

        fn real_pthread_create() -> PthreadCreate {
            static REAL: OnceLock<usize> = OnceLock::new();
            let address = *REAL.get_or_init(|| {
                // RTLD_NEXT resolves the platform implementation after this
                // executable-owned interposer instead of recursively finding
                // Supercov's exported symbol.
                let next = usize::MAX as *mut c_void;
                // SAFETY: the symbol is NUL-terminated and dlsym accepts the
                // platform RTLD_NEXT sentinel on macOS and Linux.
                let resolved = unsafe { dlsym(next, c"pthread_create".as_ptr()) };
                if resolved.is_null() {
                    std::process::abort();
                }
                resolved as usize
            });
            // SAFETY: dlsym returned the platform pthread_create symbol, whose
            // pointer-shaped ABI is described by PthreadCreate above.
            unsafe { mem::transmute::<usize, PthreadCreate>(address) }
        }

        unsafe extern "C" fn run_with_context(opaque: *mut c_void) -> *mut c_void {
            // SAFETY: pthread_create receives exactly one Box allocation from
            // the interposer and invokes this start routine at most once.
            let start = unsafe { Box::from_raw(opaque.cast::<ThreadStart>()) };
            let previous = super::enter_context(start.context);
            // SAFETY: the original start routine and argument came directly
            // from the caller's pthread_create invocation.
            let result = unsafe { (start.routine)(start.argument) };
            super::exit_context(previous);
            result
        }

        /// Preserve the exact active Supercov context across native thread
        /// creation without mutating process-global environment or requiring
        /// cooperation from `std::thread`, an executor, or the test suite.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn pthread_create(
            thread: *mut usize,
            attributes: *const c_void,
            routine: StartRoutine,
            argument: *mut c_void,
        ) -> i32 {
            let context = super::active_context();
            if matches!(context, 0 | u64::MAX) {
                // SAFETY: arguments are forwarded unchanged to the platform.
                return unsafe {
                    real_pthread_create()(thread, attributes, routine, argument)
                };
            }
            let start = Box::new(ThreadStart {
                routine,
                argument,
                context,
            });
            let raw = Box::into_raw(start).cast::<c_void>();
            // SAFETY: run_with_context has the platform start-routine ABI and
            // owns raw only if pthread_create succeeds.
            let result = unsafe {
                real_pthread_create()(thread, attributes, run_with_context, raw)
            };
            if result != 0 {
                // SAFETY: a failed pthread_create never transfers raw to a
                // child thread, so the caller must reclaim it exactly once.
                unsafe { drop(Box::from_raw(raw.cast::<ThreadStart>())) };
            }
            result
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    mod inherited_process_context {
        use std::{ffi::{CStr, CString, c_char, c_void}, mem, sync::OnceLock};

        type PosixSpawn = unsafe extern "C" fn(
            *mut i32,
            *const c_char,
            *const c_void,
            *const c_void,
            *const *mut c_char,
            *const *mut c_char,
        ) -> i32;

        const CONTEXT_PREFIX: &[u8] = b"SUPERCOV_RUST_CONTEXT_ID=";

        #[cfg(target_os = "macos")]
        #[link(name = "System")]
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }

        #[cfg(target_os = "linux")]
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }

        fn resolve(slot: &'static OnceLock<usize>, symbol: &'static CStr) -> PosixSpawn {
            let address = *slot.get_or_init(|| {
                let next = usize::MAX as *mut c_void;
                // SAFETY: symbol is NUL-terminated and the platform accepts
                // RTLD_NEXT for an executable-owned interposer.
                let resolved = unsafe { dlsym(next, symbol.as_ptr()) };
                if resolved.is_null() {
                    std::process::abort();
                }
                resolved as usize
            });
            // SAFETY: both resolved POSIX spawn functions have PosixSpawn's
            // pointer-shaped ABI.
            unsafe { mem::transmute::<usize, PosixSpawn>(address) }
        }

        struct ChildEnvironment {
            pointers: Vec<*mut c_char>,
            _context: CString,
        }

        unsafe fn child_environment(
            environment: *const *mut c_char,
        ) -> Option<ChildEnvironment> {
            let context = super::active_context();
            if environment.is_null() || matches!(context, 0 | u64::MAX) {
                return None;
            }
            let mut pointers = Vec::new();
            let mut found = false;
            let mut index = 0_usize;
            loop {
                // SAFETY: POSIX requires envp to be a null-terminated pointer
                // array whose entries remain valid for the spawn call.
                let pointer = unsafe { *environment.add(index) };
                if pointer.is_null() {
                    break;
                }
                // SAFETY: each non-null envp entry is a NUL-terminated C
                // string owned by the caller for the duration of this call.
                let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
                if bytes.starts_with(CONTEXT_PREFIX) {
                    found = true;
                } else {
                    pointers.push(pointer);
                }
                index = index.checked_add(1).unwrap_or_else(|| std::process::abort());
            }
            // Absence is meaningful: Command::env_remove explicitly opts the
            // child into authenticated context-zero/background attribution.
            if !found {
                return None;
            }
            let context = CString::new(format!(
                "SUPERCOV_RUST_CONTEXT_ID={context:016x}"
            ))
            .unwrap_or_else(|_| std::process::abort());
            pointers.push(context.as_ptr().cast_mut());
            pointers.push(std::ptr::null_mut());
            Some(ChildEnvironment {
                pointers,
                _context: context,
            })
        }

        unsafe fn spawn_with_context(
            real: PosixSpawn,
            pid: *mut i32,
            path: *const c_char,
            file_actions: *const c_void,
            attributes: *const c_void,
            arguments: *const *mut c_char,
            environment: *const *mut c_char,
        ) -> i32 {
            // SAFETY: child_environment only reads the caller-owned envp for
            // the duration of this synchronous spawn call.
            let child = unsafe { child_environment(environment) };
            let environment = child
                .as_ref()
                .map_or(environment, |owned| owned.pointers.as_ptr());
            // SAFETY: all arguments except the optional replacement envp are
            // forwarded unchanged to the platform implementation.
            unsafe {
                real(
                    pid,
                    path,
                    file_actions,
                    attributes,
                    arguments,
                    environment,
                )
            }
        }

        /// Propagate the active context through std::process::Command and any
        /// other POSIX-spawn caller without changing process-global env state.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn posix_spawn(
            pid: *mut i32,
            path: *const c_char,
            file_actions: *const c_void,
            attributes: *const c_void,
            arguments: *const *mut c_char,
            environment: *const *mut c_char,
        ) -> i32 {
            static REAL: OnceLock<usize> = OnceLock::new();
            // SAFETY: the wrapper has the exact pointer-shaped POSIX ABI.
            unsafe {
                spawn_with_context(
                    resolve(&REAL, c"posix_spawn"),
                    pid,
                    path,
                    file_actions,
                    attributes,
                    arguments,
                    environment,
                )
            }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn posix_spawnp(
            pid: *mut i32,
            file: *const c_char,
            file_actions: *const c_void,
            attributes: *const c_void,
            arguments: *const *mut c_char,
            environment: *const *mut c_char,
        ) -> i32 {
            static REAL: OnceLock<usize> = OnceLock::new();
            // SAFETY: the wrapper has the exact pointer-shaped POSIX ABI.
            unsafe {
                spawn_with_context(
                    resolve(&REAL, c"posix_spawnp"),
                    pid,
                    file,
                    file_actions,
                    attributes,
                    arguments,
                    environment,
                )
            }
        }

        type Execve = unsafe extern "C" fn(
            *const c_char,
            *const *mut c_char,
            *const *mut c_char,
        ) -> i32;
        type Execv = unsafe extern "C" fn(*const c_char, *const *mut c_char) -> i32;

        unsafe extern "C" {
            static mut environ: *mut *mut c_char;
        }

        fn resolve_address(slot: &'static OnceLock<usize>, symbol: &'static CStr) -> usize {
            *slot.get_or_init(|| {
                let next = usize::MAX as *mut c_void;
                // SAFETY: symbol is NUL-terminated and the platform accepts
                // RTLD_NEXT for an executable-owned interposer.
                let resolved = unsafe { dlsym(next, symbol.as_ptr()) };
                if resolved.is_null() {
                    std::process::abort();
                }
                resolved as usize
            })
        }

        /// Propagate the active context through a direct execve, whose caller
        /// supplies the child environment explicitly. A fork child still reads
        /// the forking thread's exact context, so fork+execve compositions and
        /// std::process pre_exec fallbacks inherit like posix_spawn callers.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn execve(
            path: *const c_char,
            arguments: *const *mut c_char,
            environment: *const *mut c_char,
        ) -> i32 {
            static REAL: OnceLock<usize> = OnceLock::new();
            // SAFETY: dlsym resolved the platform execve with Execve's ABI.
            let real = unsafe {
                mem::transmute::<usize, Execve>(resolve_address(&REAL, c"execve"))
            };
            // SAFETY: child_environment only reads the caller-owned envp for
            // the duration of this exec attempt.
            let child = unsafe { child_environment(environment) };
            let environment = child
                .as_ref()
                .map_or(environment, |owned| owned.pointers.as_ptr());
            // SAFETY: all arguments except the optional replacement envp are
            // forwarded unchanged; on success the image is replaced and on
            // failure the owned buffers are still alive here.
            unsafe { real(path, arguments, environment) }
        }

        /// execv and execvp read the process-global environ instead of taking
        /// an envp argument, and their libc-internal exec calls do not pass
        /// through the interposed execve symbol. Swap environ to the replaced
        /// copy only for the duration of the exec attempt and restore it on
        /// failure, so a successful exec ships the exact context while a
        /// failed exec leaves the caller's environment untouched.
        unsafe fn exec_with_environ(
            real: Execv,
            path: *const c_char,
            arguments: *const *mut c_char,
        ) -> i32 {
            // SAFETY: environ is the platform-owned NULL-terminated array and
            // child_environment only reads it for the duration of this call.
            let child = unsafe { child_environment(environ.cast_const().cast()) };
            let Some(owned) = child else {
                // SAFETY: no context replacement applies; forward unchanged.
                return unsafe { real(path, arguments) };
            };
            // SAFETY: exec callers own this thread; a fork child is single
            // threaded and a direct caller's environ is restored before any
            // failure return escapes this frame.
            unsafe {
                let saved = environ;
                environ = owned.pointers.as_ptr().cast_mut().cast();
                let result = real(path, arguments);
                environ = saved;
                result
            }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn execv(
            path: *const c_char,
            arguments: *const *mut c_char,
        ) -> i32 {
            static REAL: OnceLock<usize> = OnceLock::new();
            // SAFETY: dlsym resolved the platform execv with Execv's ABI.
            let real = unsafe {
                mem::transmute::<usize, Execv>(resolve_address(&REAL, c"execv"))
            };
            // SAFETY: forwarded with the documented environ swap contract.
            unsafe { exec_with_environ(real, path, arguments) }
        }

        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn execvp(
            file: *const c_char,
            arguments: *const *mut c_char,
        ) -> i32 {
            static REAL: OnceLock<usize> = OnceLock::new();
            // SAFETY: dlsym resolved the platform execvp with Execv's ABI.
            let real = unsafe {
                mem::transmute::<usize, Execv>(resolve_address(&REAL, c"execvp"))
            };
            // SAFETY: forwarded with the documented environ swap contract.
            unsafe { exec_with_environ(real, file, arguments) }
        }
    }

    struct Transport {
        pointer: *mut u8,
        length: usize,
        descriptor_capacity: u32,
        payload_capacity: u32,
        payload_base: usize,
        context_id: u64,
    }

    // SAFETY: all shared mutations use disjoint atomically reserved regions;
    // publication uses a release-store descriptor commit byte.
    unsafe impl Send for Transport {}
    unsafe impl Sync for Transport {}

    #[cfg(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "linux",
            any(target_env = "gnu", target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        )
    ))]
    impl Transport {
        fn open() -> Option<Self> {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::fs::OpenOptionsExt as _;

            #[cfg(target_os = "linux")]
            const O_NOFOLLOW: i32 = 0x2_0000;
            #[cfg(target_os = "macos")]
            const O_NOFOLLOW: i32 = 0x100;

            unsafe extern "C" {
                fn mmap(
                    address: *mut std::ffi::c_void,
                    length: usize,
                    protection: i32,
                    flags: i32,
                    file: i32,
                    offset: isize,
                ) -> *mut std::ffi::c_void;
            }

            let path = std::env::var_os("SUPERCOV_RUST_TRANSPORT_FILE")?;
            let token = parse_token(&std::env::var("SUPERCOV_RUST_TRANSPORT_TOKEN").ok()?)?;
            let context_id = match std::env::var(CONTEXT_ENV) {
                Ok(value) => parse_context(&value)?,
                Err(std::env::VarError::NotPresent) => 0,
                Err(std::env::VarError::NotUnicode(_)) => return None,
            };
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(O_NOFOLLOW)
                .open(path)
                .ok()?;
            if !file.metadata().ok()?.file_type().is_file() {
                return None;
            }
            let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
            if length < HEADER_SIZE {
                return None;
            }
            // SAFETY: the file is kept open through mmap and the returned map
            // is checked before any typed access.
            let pointer = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    length,
                    1 | 2,
                    1,
                    file.as_raw_fd(),
                    0,
                )
            };
            if pointer as isize == -1 {
                return None;
            }
            let pointer = pointer.cast::<u8>();
            let parsed = (|| {
                // SAFETY: the mapping has at least HEADER_SIZE readable bytes.
                let header = unsafe { std::slice::from_raw_parts(pointer, HEADER_SIZE) };
                if header.get(..8) != Some(MAGIC.as_slice())
                    || read_u32(header, 8)? != VERSION
                    || read_u32(header, 12)? != HEADER_SIZE as u32
                    || read_u32(header, 16)? != DESCRIPTOR_SIZE as u32
                    || read_u32(header, 28)? != ENDIAN_MARKER
                    || header.get(TOKEN_OFFSET..TOKEN_OFFSET + TOKEN_SIZE)? != token
                    || header.get(52..56)? != [0; 4]
                    || header.get(88..HEADER_SIZE)? != [0; 40]
                {
                    return None;
                }
                let descriptor_capacity = read_u32(header, 20)?;
                let payload_capacity = read_u32(header, 24)?;
                if descriptor_capacity == 0 || payload_capacity == 0 {
                    return None;
                }
                let payload_base = HEADER_SIZE.checked_add(
                    usize::try_from(descriptor_capacity)
                        .ok()?
                        .checked_mul(DESCRIPTOR_SIZE)?,
                )?;
                (payload_base.checked_add(usize::try_from(payload_capacity).ok()?)? == length)
                    .then_some((descriptor_capacity, payload_capacity, payload_base))
            })();
            let Some((descriptor_capacity, payload_capacity, payload_base)) = parsed else {
                // SAFETY: this is the exact mapping returned by mmap and it
                // has not been transferred into a Transport.
                let _ = unsafe { munmap_region(pointer, length) };
                return None;
            };
            let transport = Self {
                pointer,
                length,
                descriptor_capacity,
                payload_capacity,
                payload_base,
                context_id,
            };
            transport
                .atomic_u64(ATTACHMENTS_OFFSET)
                .fetch_add(1, Ordering::Relaxed);
            Some(transport)
        }

        fn atomic_u64(&self, offset: usize) -> &AtomicU64 {
            // SAFETY: header offsets are aligned within the page-aligned map.
            unsafe { &*self.pointer.add(offset).cast::<AtomicU64>() }
        }

        fn dropped(&self) {
            self.atomic_u64(DROPPED_OFFSET).fetch_add(1, Ordering::Relaxed);
        }

        fn active_context(&self) -> u64 {
            CONTEXT_OVERRIDE.with(Cell::get).unwrap_or(self.context_id)
        }

        fn record(&self, kind: u8, outcome: u8, id: &str, values: &[u8]) {
            self.record_in_context(kind, outcome, id, values, self.active_context());
        }

        fn record_in_context(
            &self,
            kind: u8,
            outcome: u8,
            id: &str,
            values: &[u8],
            context_id: u64,
        ) {
            let Ok(id_length) = u32::try_from(id.len()) else {
                self.dropped();
                return;
            };
            let Ok(value_length) = u32::try_from(values.len()) else {
                self.dropped();
                return;
            };
            let Some(payload_length) = id_length.checked_add(value_length) else {
                self.dropped();
                return;
            };
            let descriptor = self
                .atomic_u64(NEXT_DESCRIPTOR_OFFSET)
                .fetch_add(1, Ordering::Relaxed);
            if descriptor >= u64::from(self.descriptor_capacity) {
                self.dropped();
                return;
            }
            let payload = self
                .atomic_u64(NEXT_PAYLOAD_OFFSET)
                .fetch_add(u64::from(payload_length), Ordering::Relaxed);
            if payload.saturating_add(u64::from(payload_length))
                > u64::from(self.payload_capacity)
            {
                self.dropped();
                return;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            let payload_offset = self.payload_base + payload as usize;
            // SAFETY: the two atomic reservations prove these descriptor and
            // payload ranges are in-bounds and disjoint from every writer.
            unsafe {
                let descriptor_pointer = self.pointer.add(descriptor_offset);
                descriptor_pointer.add(1).write(kind);
                descriptor_pointer.add(2).write(outcome);
                descriptor_pointer.add(3).write(0);
                write_u32(descriptor_pointer, 4, std::process::id());
                write_u64(descriptor_pointer, 8, context_id);
                write_u32(descriptor_pointer, 16, payload as u32);
                write_u32(descriptor_pointer, 20, payload_length);
                write_u32(descriptor_pointer, 24, id_length);
                write_u32(descriptor_pointer, 28, value_length);
                write_u64(
                    descriptor_pointer,
                    32,
                    checksum(
                        kind,
                        outcome,
                        std::process::id(),
                        context_id,
                        payload as u32,
                        payload_length,
                        id_length,
                        value_length,
                        id.as_bytes(),
                        values,
                    ),
                );
                std::ptr::copy_nonoverlapping(id.as_ptr(), self.pointer.add(payload_offset), id.len());
                std::ptr::copy_nonoverlapping(
                    values.as_ptr(),
                    self.pointer.add(payload_offset + id.len()),
                    values.len(),
                );
                (&*descriptor_pointer.cast::<AtomicU8>()).store(1, Ordering::Release);
            }
        }

        fn reserve_decision(
            &self,
            id_high: u64,
            id_low: u32,
            conditions: usize,
        ) -> u64 {
            let id = decision_id(id_high, id_low);
            let id_length = DECISION_ID_LENGTH;
            let Ok(value_length) = u32::try_from(conditions) else {
                self.dropped();
                return 0;
            };
            let Some(payload_length) = id_length.checked_add(value_length) else {
                self.dropped();
                return 0;
            };
            let descriptor = self
                .atomic_u64(NEXT_DESCRIPTOR_OFFSET)
                .fetch_add(1, Ordering::Relaxed);
            if descriptor >= u64::from(self.descriptor_capacity) {
                self.dropped();
                return 0;
            }
            let payload = self
                .atomic_u64(NEXT_PAYLOAD_OFFSET)
                .fetch_add(u64::from(payload_length), Ordering::Relaxed);
            if payload.saturating_add(u64::from(payload_length))
                > u64::from(self.payload_capacity)
            {
                self.dropped();
                return 0;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            let payload_offset = self.payload_base + payload as usize;
            let context_id = self.active_context();
            // SAFETY: reservations are in-bounds and private to this frame.
            // The commit byte remains zero until decision_finish publishes the
            // complete vector; process death therefore becomes explicit
            // incomplete health instead of silent loss.
            unsafe {
                let descriptor_pointer = self.pointer.add(descriptor_offset);
                descriptor_pointer.add(1).write(KIND_DECISION);
                descriptor_pointer.add(2).write(0);
                descriptor_pointer.add(3).write(0);
                write_u32(descriptor_pointer, 4, std::process::id());
                write_u64(descriptor_pointer, 8, context_id);
                write_u32(descriptor_pointer, 16, payload as u32);
                write_u32(descriptor_pointer, 20, payload_length);
                write_u32(descriptor_pointer, 24, id_length);
                write_u32(descriptor_pointer, 28, value_length);
                write_u64(descriptor_pointer, 32, 0);
                std::ptr::copy_nonoverlapping(id.as_ptr(), self.pointer.add(payload_offset), id.len());
                std::ptr::write_bytes(
                    self.pointer.add(payload_offset + id.len()),
                    0,
                    conditions,
                );
            }
            descriptor + 1
        }

        fn decision_condition(&self, token: u64, index: usize, value: bool) {
            let Some(descriptor) = token.checked_sub(1) else {
                return;
            };
            if descriptor >= u64::from(self.descriptor_capacity) {
                return;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            // SAFETY: the descriptor index is in-bounds. Only the evaluation
            // owning this token mutates its uncommitted value bytes.
            unsafe {
                let pointer = self.pointer.add(descriptor_offset);
                if (&*pointer.cast::<AtomicU8>()).load(Ordering::Acquire) != 0 {
                    return;
                }
                let bytes = std::slice::from_raw_parts(pointer, DESCRIPTOR_SIZE);
                if bytes[1] != KIND_DECISION
                    || read_u32(bytes, 4) != Some(std::process::id())
                {
                    return;
                }
                let Some(payload) = read_u32(bytes, 16).map(usize::try_from).and_then(Result::ok)
                else {
                    return;
                };
                let Some(id_length) = read_u32(bytes, 24).map(usize::try_from).and_then(Result::ok)
                else {
                    return;
                };
                let Some(value_length) =
                    read_u32(bytes, 28).map(usize::try_from).and_then(Result::ok)
                else {
                    return;
                };
                if index >= value_length
                    || payload
                        .checked_add(id_length)
                        .and_then(|start| start.checked_add(value_length))
                        .is_none_or(|end| end > self.payload_capacity as usize)
                {
                    return;
                }
                self.pointer
                    .add(self.payload_base + payload + id_length + index)
                    .write(if value { 2 } else { 1 });
            }
        }

        fn finish_decision(&self, token: u64, outcome: bool) {
            let Some(descriptor) = token.checked_sub(1) else {
                return;
            };
            if descriptor >= u64::from(self.descriptor_capacity) {
                return;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            // SAFETY: the descriptor and its reserved payload are in-bounds;
            // this evaluation is the sole writer before the release commit.
            unsafe {
                let pointer = self.pointer.add(descriptor_offset);
                if (&*pointer.cast::<AtomicU8>()).load(Ordering::Acquire) != 0 {
                    return;
                }
                let bytes = std::slice::from_raw_parts(pointer, DESCRIPTOR_SIZE);
                if bytes[1] != KIND_DECISION
                    || read_u32(bytes, 4) != Some(std::process::id())
                {
                    return;
                }
                let Some(context_id) = read_u64(bytes, 8) else {
                    return;
                };
                let Some(payload) = read_u32(bytes, 16) else {
                    return;
                };
                let Some(payload_length) = read_u32(bytes, 20) else {
                    return;
                };
                let Some(id_length) = read_u32(bytes, 24) else {
                    return;
                };
                let Some(value_length) = read_u32(bytes, 28) else {
                    return;
                };
                if id_length.checked_add(value_length) != Some(payload_length)
                    || u64::from(payload).saturating_add(u64::from(payload_length))
                        > u64::from(self.payload_capacity)
                {
                    return;
                }
                let payload_pointer = self.pointer.add(self.payload_base + payload as usize);
                let id = std::slice::from_raw_parts(payload_pointer, id_length as usize);
                let values = std::slice::from_raw_parts(
                    payload_pointer.add(id_length as usize),
                    value_length as usize,
                );
                let outcome = u8::from(outcome);
                pointer.add(2).write(outcome);
                write_u64(
                    pointer,
                    32,
                    checksum(
                        KIND_DECISION,
                        outcome,
                        std::process::id(),
                        context_id,
                        payload,
                        payload_length,
                        id_length,
                        value_length,
                        id,
                        values,
                    ),
                );
                (&*pointer.cast::<AtomicU8>()).store(1, Ordering::Release);
            }
        }

        fn reserve_ordinal(&self) -> u64 {
            let descriptor = self
                .atomic_u64(NEXT_DESCRIPTOR_OFFSET)
                .fetch_add(1, Ordering::Relaxed);
            if descriptor >= u64::from(self.descriptor_capacity) {
                self.dropped();
                return 0;
            }
            let payload = self
                .atomic_u64(NEXT_PAYLOAD_OFFSET)
                .fetch_add(8, Ordering::Relaxed);
            if payload.saturating_add(8) > u64::from(self.payload_capacity) {
                self.dropped();
                return 0;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            // SAFETY: reservations are in-bounds and private to this frame.
            // The first terminal branch observation writes the ordinal and
            // commits the descriptor; interruption remains explicit health.
            unsafe {
                let pointer = self.pointer.add(descriptor_offset);
                pointer.add(1).write(KIND_ORDINAL_HIT);
                pointer.add(2).write(0);
                pointer.add(3).write(0);
                write_u32(pointer, 4, std::process::id());
                write_u64(pointer, 8, self.active_context());
                write_u32(pointer, 16, payload as u32);
                write_u32(pointer, 20, 8);
                write_u32(pointer, 24, 0);
                write_u32(pointer, 28, 8);
                write_u64(pointer, 32, 0);
            }
            descriptor + 1
        }

        fn finish_ordinal(&self, token: u64, ordinal: u64) {
            let Some(descriptor) = token.checked_sub(1) else {
                return;
            };
            if descriptor >= u64::from(self.descriptor_capacity) {
                return;
            }
            let descriptor_offset = HEADER_SIZE + descriptor as usize * DESCRIPTOR_SIZE;
            // SAFETY: the descriptor and its eight-byte payload were reserved
            // together. The release commit publishes exactly one alternative.
            unsafe {
                let pointer = self.pointer.add(descriptor_offset);
                if (&*pointer.cast::<AtomicU8>()).load(Ordering::Acquire) != 0 {
                    return;
                }
                let bytes = std::slice::from_raw_parts(pointer, DESCRIPTOR_SIZE);
                if bytes[1] != KIND_ORDINAL_HIT
                    || read_u32(bytes, 4) != Some(std::process::id())
                    || read_u32(bytes, 20) != Some(8)
                    || read_u32(bytes, 24) != Some(0)
                    || read_u32(bytes, 28) != Some(8)
                {
                    return;
                }
                let Some(context_id) = read_u64(bytes, 8) else {
                    return;
                };
                let Some(payload) = read_u32(bytes, 16) else {
                    return;
                };
                if u64::from(payload).saturating_add(8) > u64::from(self.payload_capacity) {
                    return;
                }
                let payload_pointer = self.pointer.add(self.payload_base + payload as usize);
                write_u64(payload_pointer, 0, ordinal);
                let values = std::slice::from_raw_parts(payload_pointer, 8);
                write_u64(
                    pointer,
                    32,
                    checksum(
                        KIND_ORDINAL_HIT,
                        0,
                        std::process::id(),
                        context_id,
                        payload,
                        8,
                        0,
                        8,
                        &[],
                        values,
                    ),
                );
                (&*pointer.cast::<AtomicU8>()).store(1, Ordering::Release);
            }
        }
    }

    #[cfg(not(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "linux",
            any(target_env = "gnu", target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        )
    )))]
    impl Transport {
        fn open() -> Option<Self> {
            None
        }

        fn record(&self, _kind: u8, _outcome: u8, _id: &str, _values: &[u8]) {}

        fn active_context(&self) -> u64 {
            0
        }

        fn record_in_context(
            &self,
            _kind: u8,
            _outcome: u8,
            _id: &str,
            _values: &[u8],
            _context_id: u64,
        ) {
        }

        fn reserve_decision(
            &self,
            _id_high: u64,
            _id_low: u32,
            _conditions: usize,
        ) -> u64 {
            0
        }

        fn decision_condition(&self, _token: u64, _index: usize, _value: bool) {}

        fn finish_decision(&self, _token: u64, _outcome: bool) {}

        fn reserve_ordinal(&self) -> u64 {
            0
        }

        fn finish_ordinal(&self, _token: u64, _ordinal: u64) {}
    }

    impl Drop for Transport {
        fn drop(&mut self) {
            #[cfg(any(
                all(
                    target_os = "macos",
                    any(target_arch = "aarch64", target_arch = "x86_64")
                ),
                all(
                    target_os = "linux",
                    any(target_env = "gnu", target_env = "musl"),
                    any(target_arch = "aarch64", target_arch = "x86_64")
                )
            ))]
            {
                // SAFETY: this is the exact live mapping returned by mmap.
                let _ = unsafe { munmap_region(self.pointer, self.length) };
            }
        }
    }

    #[cfg(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        all(
            target_os = "linux",
            any(target_env = "gnu", target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        )
    ))]
    unsafe fn munmap_region(pointer: *mut u8, length: usize) -> i32 {
        unsafe extern "C" {
            fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
        }
        // SAFETY: the caller owns the exact live mapping and length.
        unsafe { munmap(pointer.cast(), length) }
    }

    fn read_u32(source: &[u8], offset: usize) -> Option<u32> {
        Some(u32::from_le_bytes(source.get(offset..offset + 4)?.try_into().ok()?))
    }

    fn read_u64(source: &[u8], offset: usize) -> Option<u64> {
        Some(u64::from_le_bytes(source.get(offset..offset + 8)?.try_into().ok()?))
    }

    fn decision_id(id_high: u64, id_low: u32) -> [u8; DECISION_ID_LENGTH as usize] {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = [0; DECISION_ID_LENGTH as usize];
        output[..DECISION_ID_PREFIX.len()].copy_from_slice(DECISION_ID_PREFIX);
        let mut offset = DECISION_ID_PREFIX.len();
        for shift in (0..16).rev() {
            output[offset] = HEX[((id_high >> (shift * 4)) & 0xf) as usize];
            offset += 1;
        }
        for shift in (0..8).rev() {
            output[offset] = HEX[((u64::from(id_low) >> (shift * 4)) & 0xf) as usize];
            offset += 1;
        }
        output
    }

    fn parse_token(value: &str) -> Option<[u8; TOKEN_SIZE]> {
        if value.len() != TOKEN_SIZE * 2 {
            return None;
        }
        let mut token = [0_u8; TOKEN_SIZE];
        for (index, slot) in token.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(token)
    }

    fn parse_context(value: &str) -> Option<u64> {
        (value.len() == 16)
            .then(|| u64::from_str_radix(value, 16).ok())
            .flatten()
    }

    unsafe fn write_u32(target: *mut u8, offset: usize, value: u32) {
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            // SAFETY: caller proves the descriptor range is writable.
            unsafe { target.add(offset + index).write(byte) };
        }
    }

    unsafe fn write_u64(target: *mut u8, offset: usize, value: u64) {
        for (index, byte) in value.to_le_bytes().into_iter().enumerate() {
            // SAFETY: caller proves the descriptor range is writable.
            unsafe { target.add(offset + index).write(byte) };
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn checksum(
        kind: u8,
        outcome: u8,
        pid: u32,
        context: u64,
        payload_offset: u32,
        payload_length: u32,
        id_length: u32,
        value_length: u32,
        id: &[u8],
        values: &[u8],
    ) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325_u64;
        for byte in [kind, outcome]
            .into_iter()
            .chain(pid.to_le_bytes())
            .chain(context.to_le_bytes())
            .chain(payload_offset.to_le_bytes())
            .chain(payload_length.to_le_bytes())
            .chain(id_length.to_le_bytes())
            .chain(value_length.to_le_bytes())
            .chain(id.iter().copied())
            .chain(values.iter().copied())
        {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        value
    }

    fn assertion_context_id(parent: u64, id_high: u64, id_low: u32, nonce: u64) -> u64 {
        let mut value = 0xcbf2_9ce4_8422_2325_u64;
        for byte in b"supercov-rust-assertion-phase-v2"
            .iter()
            .copied()
            .chain(parent.to_le_bytes())
            .chain(id_high.to_le_bytes())
            .chain(id_low.to_le_bytes())
            .chain(nonce.to_le_bytes())
        {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        if matches!(value, 0 | u64::MAX) {
            value ^ 0xa5a5_5a5a_d3c3_b4b4
        } else {
            value
        }
    }

    fn transport() -> Option<&'static Transport> {
        static TRANSPORT: OnceLock<Option<Transport>> = OnceLock::new();
        TRANSPORT.get_or_init(Transport::open).as_ref()
    }

    pub struct DecisionFrame {
        id: &'static str,
        values: Vec<u8>,
    }

    impl DecisionFrame {
        pub fn new(id: &'static str, conditions: usize) -> Self {
            Self {
                id,
                values: vec![0; conditions],
            }
        }
    }

    #[inline]
    pub fn hit(id: &'static str) {
        if let Some(transport) = transport() {
            transport.record(KIND_HIT, 0, id, &[]);
        }
    }

    #[inline]
    pub fn condition(value: bool, frame: &mut DecisionFrame, index: usize) -> bool {
        if let Some(slot) = frame.values.get_mut(index) {
            *slot = if value { 2 } else { 1 };
        }
        value
    }

    #[inline]
    pub fn decision(value: bool, frame: &mut DecisionFrame) -> bool {
        if let Some(transport) = transport() {
            transport.record(KIND_DECISION, u8::from(value), frame.id, &frame.values);
        }
        value
    }

    /// Begins one compiler-injected decision evaluation. The token is carried
    /// in a MIR local, so nested evaluations, parallel tests and thread
    /// migration cannot merge their ternary vectors. Its transport descriptor
    /// remains uncommitted until the decision outcome, making interrupted
    /// evaluations explicit reader health.
    #[inline(never)]
    pub fn mir_decision_start(id_high: u64, id_low: u32, conditions: u64) -> u64 {
        let Ok(conditions) = usize::try_from(conditions) else {
            return 0;
        };
        transport().map_or(0, |transport| {
            transport.reserve_decision(id_high, id_low, conditions)
        })
    }

    #[inline(never)]
    pub fn mir_decision_condition(token: u64, index: u64, value: bool) {
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        if let Some(transport) = transport() {
            transport.decision_condition(token, index, value);
        }
    }

    #[inline(never)]
    pub fn mir_decision_finish(token: u64, outcome: bool) {
        if let Some(transport) = transport() {
            transport.finish_decision(token, outcome);
        }
    }

    /// Reserves one crash-visible branch-selection frame. The first selected
    /// alternative commits it; later loop checks cannot relabel the same
    /// invocation from entered to zero-iteration.
    #[inline(never)]
    pub fn mir_branch_start() -> u64 {
        transport().map_or(0, Transport::reserve_ordinal)
    }

    #[inline(never)]
    pub fn mir_branch_hit(token: u64, ordinal: u64) {
        if let Some(transport) = transport() {
            transport.finish_ordinal(token, ordinal);
        }
    }

    #[inline]
    pub fn ordinal_hit(ordinal: u64) {
        if let Some(transport) = transport() {
            transport.record(KIND_ORDINAL_HIT, 0, "", &ordinal.to_le_bytes());
        }
    }

    #[inline(never)]
    pub fn active_context() -> u64 {
        transport().map_or(0, Transport::active_context)
    }

    #[inline(never)]
    pub fn enter_context(context_id: u64) -> u64 {
        debug_assert!(!matches!(context_id, 0 | u64::MAX));
        CONTEXT_OVERRIDE
            .with(|current| current.replace(Some(context_id)))
            .unwrap_or(NO_CONTEXT_OVERRIDE)
    }

    #[inline(never)]
    pub fn exit_context(previous: u64) {
        CONTEXT_OVERRIDE.with(|current| {
            current.set((previous != NO_CONTEXT_OVERRIDE).then_some(previous));
        });
    }

    #[inline(never)]
    pub fn enter_assertion_context(id_high: u64, id_low: u32) -> u64 {
        let Some(transport) = transport() else {
            return NO_CONTEXT_OVERRIDE;
        };
        let parent = transport.active_context();
        if parent == 0 {
            return NO_CONTEXT_OVERRIDE;
        }
        let nonce = transport
            .atomic_u64(NEXT_PHASE_OFFSET)
            .fetch_add(1, Ordering::Relaxed);
        let child = assertion_context_id(parent, id_high, id_low, nonce);
        let id = decision_id(id_high, id_low);
        // SAFETY: decision_id writes only the fixed ASCII prefix and lowercase
        // hexadecimal digits.
        let id = unsafe { std::str::from_utf8_unchecked(&id) };
        let mut definition = [0_u8; 16];
        definition[..8].copy_from_slice(&parent.to_le_bytes());
        definition[8..].copy_from_slice(&nonce.to_le_bytes());
        transport.record_in_context(KIND_PHASE, 0, id, &definition, child);
        enter_context(child)
    }
}
