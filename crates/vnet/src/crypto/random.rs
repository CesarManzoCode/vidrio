//! Random bytes from the kernel through glibc's `getrandom`.

use std::io;

unsafe extern "C" {
    fn getrandom(buf: *mut u8, buflen: usize, flags: u32) -> isize;
}

/// Fill `buf` with cryptographically secure random bytes.
pub fn fill(buf: &mut [u8]) -> io::Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let rest = &mut buf[done..];
        // SAFETY: `rest` is a valid writable region of `rest.len()` bytes.
        let n = unsafe { getrandom(rest.as_mut_ptr(), rest.len(), 0) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        done += n as usize;
    }
    Ok(())
}

/// `N` random bytes.
pub fn bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut b = [0u8; N];
    fill(&mut b)?;
    Ok(b)
}
