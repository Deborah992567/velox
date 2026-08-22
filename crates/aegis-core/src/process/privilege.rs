//! Privilege management (setuid/setgid).
//!
//! Phase 17: Drop root privileges after binding privileged ports.
#![allow(unsafe_code)]

/// Unix user/group identity for privilege drop.
#[derive(Debug, Clone, Copy)]
pub struct PrivilegeDrop {
    pub uid: u32,
    pub gid: u32,
}

impl PrivilegeDrop {
    /// Create from numeric uid/gid.
    pub const fn new(uid: u32, gid: u32) -> Self {
        Self { uid, gid }
    }

    /// Apply the privilege drop: setgid then setuid.
    pub fn apply(&self) -> std::io::Result<()> {
        // SAFETY: caller must be root (euid == 0) and have already bound ports.
        unsafe {
            if libc::setgid(self.gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(self.uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Verify the current process identity matches the target.
    pub fn verify(&self) -> bool {
        // SAFETY: getuid() and getgid() are safe POSIX queries.
        let current = unsafe { (libc::getuid(), libc::getgid()) };
        current.0 == self.uid && current.1 == self.gid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_privilege_drop() {
        let pd = PrivilegeDrop::new(1000, 1000);
        assert_eq!(pd.uid, 1000);
        assert_eq!(pd.gid, 1000);
    }

    #[test]
    fn verify_matches_own_identity() {
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        let pd = PrivilegeDrop::new(uid, gid);
        assert!(pd.verify());
    }

    #[test]
    fn verify_rejects_wrong_identity() {
        let pd = PrivilegeDrop::new(0xFFFF, 0xFFFF);
        assert!(!pd.verify());
    }
}
