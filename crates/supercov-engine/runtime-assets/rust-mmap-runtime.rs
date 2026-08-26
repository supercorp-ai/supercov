#[doc(hidden)]
#[allow(dead_code)]
mod __SUPERCOV_MODULE__ {
    use std::fs::OpenOptions;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

    const MAGIC: &[u8; 8] = b"SCVRUST1";
    const VERSION: u32 = 1;
    const HEADER_SIZE: usize = 128;
    const DESCRIPTOR_SIZE: usize = 40;
    const ENDIAN_MARKER: u32 = 0x0102_0304;
    const NEXT_DESCRIPTOR_OFFSET: usize = 32;
    const NEXT_PAYLOAD_OFFSET: usize = 40;
    const DROPPED_OFFSET: usize = 48;
    const TOKEN_OFFSET: usize = 56;
    const TOKEN_SIZE: usize = 16;
    const ATTACHMENTS_OFFSET: usize = 72;
    const CONTEXT_ENV: &str = "SUPERCOV_RUST_CONTEXT_ID";
    const KIND_HIT: u8 = 1;
    const KIND_DECISION: u8 = 2;
    const KIND_ORDINAL_HIT: u8 = 3;

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
                    || header.get(80..HEADER_SIZE)? != [0; 48]
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

        fn record(&self, kind: u8, outcome: u8, id: &'static str, values: &[u8]) {
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
                write_u64(descriptor_pointer, 8, self.context_id);
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
                        self.context_id,
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

        fn record(&self, _kind: u8, _outcome: u8, _id: &'static str, _values: &[u8]) {}
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

    #[inline]
    pub fn ordinal_hit(ordinal: u64) {
        if let Some(transport) = transport() {
            transport.record(KIND_ORDINAL_HIT, 0, "", &ordinal.to_le_bytes());
        }
    }
}
