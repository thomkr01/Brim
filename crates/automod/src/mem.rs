//! Gives memory back to the system that the allocator or the loader keeps after a one-off job.
//! Only Linux with glibc (the Docker images) does anything; other targets do nothing.

/// Returns free heap pages to the system. glibc keeps freed buffers (a list refresh, a model load,
/// a scan's activations) in its heap: measured +37 MB after loading the image-scan model, and a
/// 12-hour list refresh once grew the heap 95 -> 206 MB.
pub fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` takes no pointers; it only returns free heap pages to the system.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Drops the resident pages of read-only data that is embedded in the binary (`include_bytes!`) and
/// was copied at startup. The pages stay valid: a later read faults them in again from the file, so
/// this never changes what the bytes say. Only whole pages inside the slice are touched.
pub fn drop_file_pages(bytes: &'static [u8]) {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const PAGE: usize = 4096;
        let start = bytes.as_ptr() as usize;
        let from = start.div_ceil(PAGE) * PAGE;
        let to = (start + bytes.len()) / PAGE * PAGE;
        if to > from {
            // SAFETY: the range lies inside a read-only, file-backed private mapping of `bytes`;
            // MADV_DONTNEED on such a mapping only discards clean pages, which reload from the file.
            unsafe {
                libc::madvise(from as *mut libc::c_void, to - from, libc::MADV_DONTNEED);
            }
        }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    let _ = bytes;
}

#[cfg(test)]
mod tests {
    static BYTES: &[u8] = include_bytes!("../../../models/nsfw.onnx");

    #[test]
    fn dropping_file_pages_leaves_the_bytes_readable_and_unchanged() {
        let sum = |b: &[u8]| {
            b.iter()
                .fold(0u64, |a, x| a.wrapping_mul(31).wrapping_add(u64::from(*x)))
        };
        let before = sum(BYTES);
        super::drop_file_pages(BYTES);
        super::release_freed_memory();
        assert_eq!(sum(BYTES), before);
        // A slice inside one page is left alone.
        super::drop_file_pages(&BYTES[..10]);
    }
}
