//! The only Unix ownership/FFI boundary in the runtime.
//!
//! SAFETY invariants:
//! - the descriptor is accepted only after `fcntl(F_GETFL)` and `fstat` prove it
//!   is open, write-only, and a pipe/FIFO or Unix socket;
//! - `FD_CLOEXEC` is set before ownership is transferred, preventing later child
//!   processes from inheriting it;
//! - after `File::from_raw_fd`, no other runtime owner uses or closes the fd.

use std::{fs::File, mem::MaybeUninit, os::fd::FromRawFd};

pub(super) fn take_write_stream(fd: i32) -> Option<File> {
    if fd < 3 {
        return None;
    }
    // SAFETY: fcntl only inspects the integer descriptor; errors are checked.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || flags & libc::O_ACCMODE != libc::O_WRONLY {
        return None;
    }
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat points to writable, correctly aligned storage; return checked.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: successful fstat initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let kind = stat.st_mode & libc::S_IFMT;
    if kind != libc::S_IFIFO && kind != libc::S_IFSOCK {
        return None;
    }
    // SAFETY: descriptor was proved valid above; fcntl failure rejects ownership.
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor_flags < 0
        // SAFETY: setting descriptor flags does not transfer or duplicate ownership.
        || unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } != 0
    {
        return None;
    }
    // SAFETY: all validation succeeded and this function establishes the sole owner.
    Some(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn observer_pipe() -> std::io::Result<(File, File)> {
    let mut descriptors = [0_i32; 2];
    // SAFETY: points to storage for exactly two descriptors; return is checked.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe returned fresh descriptors and ownership is immediately unique.
    let reader = unsafe { File::from_raw_fd(descriptors[0]) };
    // SAFETY: as above.
    let writer = unsafe { File::from_raw_fd(descriptors[1]) };
    // The observer's read end must not cross exec; the write end deliberately must.
    // SAFETY: both descriptors are live and these operations only update fd flags.
    let reader_flags = unsafe { libc::fcntl(descriptors[0], libc::F_GETFD) };
    // SAFETY: live descriptor; return is checked below.
    let writer_flags = unsafe { libc::fcntl(descriptors[1], libc::F_GETFD) };
    if reader_flags < 0
        || writer_flags < 0
        // SAFETY: live descriptors with valid flag values.
        || unsafe { libc::fcntl(descriptors[0], libc::F_SETFD, reader_flags | libc::FD_CLOEXEC) } != 0
        // SAFETY: live descriptors with valid flag values.
        || unsafe { libc::fcntl(descriptors[1], libc::F_SETFD, writer_flags & !libc::FD_CLOEXEC) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok((reader, writer))
}

#[cfg(test)]
pub(super) fn pipe_pair() -> (File, File) {
    let mut descriptors = [0_i32; 2];
    // SAFETY: points to storage for exactly two descriptors; success is asserted.
    assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
    // SAFETY: pipe returned two fresh, uniquely owned descriptors.
    let reader = unsafe { File::from_raw_fd(descriptors[0]) };
    let writer = take_write_stream(descriptors[1]).expect("write end passes validation");
    (reader, writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn rejects_standard_and_read_ends() {
        assert!(take_write_stream(0).is_none());
        let mut descriptors = [0_i32; 2];
        // SAFETY: points to storage for exactly two descriptors; return checked.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        assert!(take_write_stream(descriptors[0]).is_none());
        // SAFETY: read descriptor was rejected and remains owned by this test.
        assert_eq!(unsafe { libc::close(descriptors[0]) }, 0);
        let write = take_write_stream(descriptors[1]).unwrap();
        let flags = unsafe { libc::fcntl(write.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
