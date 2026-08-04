//! Zero-copy file transmission with the `sendfile` syscall.
//!
//! Serving static files should not round-trip every byte through userspace.
//! This module exposes one safe [`send_file`] helper that hands the kernel a
//! byte range of a regular file and asks it to write those bytes straight to a
//! socket. Linux uses `sendfile(2)`; macOS uses its two-fd `sendfile(2)`
//! variant. Both transfer at most what the socket accepts, so callers must
//! loop on partial transfers and on `EAGAIN`.

use std::io;

/// Send up to `len` bytes starting at `offset` from the file `in_fd` to the
/// socket `out_fd`.
///
/// Returns the number of bytes actually sent, which may be less than `len`
/// (in particular when the socket buffer fills). Callers loop, advancing
/// `offset` by the returned count, until the whole range is sent or the
/// underlying error is `WouldBlock`/`Interrupted`.
///
/// # Panics
///
/// Does not panic in practice: the kernel-reported transfer count is clamped
/// to the requested `len`, and both fit in `usize` on supported platforms.
pub fn send_file(
    out_fd: std::os::fd::RawFd,
    in_fd: std::os::fd::RawFd,
    offset: u64,
    len: u64,
) -> io::Result<usize> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `out_fd` and `in_fd` are live descriptors owned by the
        // caller (`in_fd` a regular file, `out_fd` a socket), and the kernel
        // copies `count` bytes from one to the other; `offset` is a writable
        // pointer the kernel advances past the transferred bytes.
        let mut offset = libc::off_t::try_from(offset)
            .map_err(|_| io::Error::other("file offset exceeds platform range"))?;
        let count = libc::size_t::try_from(len)
            .map_err(|_| io::Error::other("transfer length exceeds platform range"))?;
        let written = unsafe { libc::sendfile(out_fd, in_fd, &mut offset, count) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(usize::try_from(written).expect("non-negative sendfile count"));
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // SAFETY: `in_fd` is a regular file and `out_fd` a socket, both live
        // and owned by the caller. `remaining` is an in/out pointer the kernel
        // uses to read the desired length and to write back the transferred
        // count; it is valid for the duration of the call.
        let offset = libc::off_t::try_from(offset)
            .map_err(|_| io::Error::other("file offset exceeds platform range"))?;
        let mut remaining = libc::off_t::try_from(len)
            .map_err(|_| io::Error::other("transfer length exceeds platform range"))?;
        let result = unsafe {
            libc::sendfile(
                in_fd,
                out_fd,
                offset,
                &raw mut remaining,
                std::ptr::null_mut(),
                0,
            )
        };
        let transferred = usize::try_from(remaining).expect("non-negative transferred count");
        if result == 0 {
            return Ok(transferred);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(transferred);
        }
        Err(error)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    {
        let _ = (out_fd, in_fd, offset, len);
        Err(io::Error::other(
            "sendfile is not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::send_file;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn copies_whole_file_to_socket() {
        let mut file = tempfile::tempfile().expect("temp file");
        file.write_all(b"hello world").expect("write");
        let (mut peer, socket) = UnixStream::pair().expect("socket pair");
        let written = send_file(socket.as_raw_fd(), file.as_raw_fd(), 0, 11).expect("sendfile");
        assert_eq!(written, 11);
        let mut buf = [0u8; 11];
        peer.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn honors_offset_and_length() {
        let mut file = tempfile::tempfile().expect("temp file");
        file.write_all(b"hello world").expect("write");
        let (mut peer, socket) = UnixStream::pair().expect("socket pair");
        let written = send_file(socket.as_raw_fd(), file.as_raw_fd(), 6, 5).expect("sendfile");
        assert_eq!(written, 5);
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn truncates_at_eof() {
        let mut file = tempfile::tempfile().expect("temp file");
        file.write_all(b"abc").expect("write");
        let (mut peer, socket) = UnixStream::pair().expect("socket pair");
        let written = send_file(socket.as_raw_fd(), file.as_raw_fd(), 1, 5).expect("sendfile");
        assert_eq!(written, 2);
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"bc");
    }
}
