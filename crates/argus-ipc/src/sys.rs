//! Raw syscall glue. All `unsafe` libc usage for IPC lives here so it can be
//! audited in one place.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest number of fds we will accept on a single `recvmsg`. Phase 0 messages
/// pass at most one (a shared-memory handle); the slack is defensive.
const MAX_RECV_FDS: usize = 8;

/// Send `bytes` over `sock` in a single `sendmsg`, attaching `fds` as ancillary
/// SCM_RIGHTS data. Returns the number of payload bytes accepted by the kernel
/// (a stream socket may accept fewer than requested).
///
/// # Safety
/// `sock` must be a valid, open socket fd; every fd in `fds` must be open.
pub(crate) unsafe fn sendmsg_fds(sock: RawFd, bytes: &[u8], fds: &[RawFd]) -> io::Result<usize> {
    debug_assert!(!bytes.is_empty(), "sendmsg with empty payload");

    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };

    // Control buffer big enough for `fds`.
    let fds_bytes = std::mem::size_of_val(fds);
    let cmsg_space = unsafe { libc::CMSG_SPACE(fds_bytes as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    if !fds.is_empty() {
        msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = cmsg_space as _;

        // SAFETY: control buffer is sized via CMSG_SPACE for `fds`.
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
        debug_assert!(!cmsg.is_null());
        unsafe {
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(fds_bytes as u32) as _;
            ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg) as *mut RawFd, fds.len());
        }
    }

    let n = unsafe { libc::sendmsg(sock, &msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Receive up to `buf.len()` bytes from `sock` in a single `recvmsg`, collecting
/// any passed file descriptors. Returns `(bytes_read, fds)`; `bytes_read == 0`
/// means the peer closed the connection.
///
/// # Safety
/// `sock` must be a valid, open socket fd.
pub(crate) unsafe fn recvmsg_fds(sock: RawFd, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
    };

    let cmsg_space =
        unsafe { libc::CMSG_SPACE((MAX_RECV_FDS * size_of::<RawFd>()) as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;

    let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut fds = Vec::new();
    // SAFETY: msg was populated by recvmsg; walk its control headers.
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        let (level, ty) = unsafe { ((*cmsg).cmsg_level, (*cmsg).cmsg_type) };
        if level == libc::SOL_SOCKET && ty == libc::SCM_RIGHTS {
            let data = unsafe { libc::CMSG_DATA(cmsg) } as *const RawFd;
            let payload_len =
                unsafe { (*cmsg).cmsg_len } as usize - unsafe { libc::CMSG_LEN(0) } as usize;
            let count = payload_len / size_of::<RawFd>();
            for i in 0..count {
                let raw = unsafe { ptr::read_unaligned(data.add(i)) };
                fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }

    Ok((n as usize, fds))
}

/// Create an **anonymous** shared-memory object of `len` bytes and return its fd.
///
/// The object is created with a unique name and immediately `shm_unlink`ed, so it
/// has no filesystem presence; the returned fd (and any copies passed over a
/// socket) keep it alive. macOS limits shm names to 31 bytes, hence the terse name.
pub(crate) fn shm_create_fd(len: usize) -> io::Result<OwnedFd> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();

    for _ in 0..64 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("/argus.{pid:x}.{n:x}")).unwrap();
        // SAFETY: name is a valid C string; shm_open is variadic with a mode arg.
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EEXIST) {
                continue; // name collision, try the next counter value
            }
            return Err(err);
        }
        // Unlink the name immediately; the fd keeps the object alive.
        unsafe { libc::shm_unlink(name.as_ptr()) };

        // SAFETY: fd is a fresh, valid shm fd we own.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd, len as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(owned);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique shm name",
    ))
}

/// Wrapper over `mmap` for a shared region of `len` bytes on `fd`.
///
/// # Safety
/// `fd` must refer to a shared-memory object of at least `len` bytes.
pub(crate) unsafe fn mmap_shared(fd: RawFd, len: usize) -> io::Result<*mut u8> {
    let ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(ptr as *mut u8)
}

/// Wrapper over `munmap`.
///
/// # Safety
/// `ptr`/`len` must come from a prior [`mmap_shared`] that has not been unmapped.
pub(crate) unsafe fn munmap(ptr: *mut u8, len: usize) {
    unsafe {
        libc::munmap(ptr as *mut libc::c_void, len);
    }
}
