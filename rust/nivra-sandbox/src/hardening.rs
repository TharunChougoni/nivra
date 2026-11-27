//! One-time OS hardening applied in `pre_exec` before loading any Python
//! strategy: close inherited fds, rlimits (memory, open files, file size — no
//! lifetime CPU limit), `no_new_privs`, Landlock (read-only Python/stdlib/runner,
//! TCP bind/connect denied), and seccomp blocking sockets, process creation,
//! and ptrace.
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const SYS_CLOSE_RANGE: libc::c_long = 436;

const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;
const LANDLOCK_ACCESS_FS_IOCTL_DEV: u64 = 1 << 15;

const LANDLOCK_ACCESS_NET_BIND_TCP: u64 = 1 << 0;
const LANDLOCK_ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;
const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const LANDLOCK_SCOPE_SIGNAL: u64 = 1 << 1;

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Probe the host kernel's Landlock ABI version, if supported.
pub fn probe_landlock_abi() -> Option<u32> {
    // SAFETY: syscall with null attr and LANDLOCK_CREATE_RULESET_VERSION only
    // queries the kernel's ABI version.
    let version = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    (version >= 1).then_some(version as u32)
}

#[derive(Clone)]
pub struct HardeningPlan {
    memory_bytes: u64,
    max_open_files: u64,
    landlock_abi: Option<u32>,
    ro_dirs: Vec<CString>,
    ro_files: Vec<CString>,
    seccomp_filter: Vec<libc::sock_filter>,
}

fn cstring(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_bytes()).ok()
}

fn seccomp_program() -> Result<Vec<libc::sock_filter>, &'static str> {
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err("strategy_sandbox_setup")
    }
    #[cfg(target_arch = "x86_64")]
    {
        let mut insns: Vec<libc::sock_filter> = vec![
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 4,
            }, // arch
            libc::sock_filter {
                code: 0x15,
                jt: 1,
                jf: 0,
                k: 0xc000_003e,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x8000_0000,
            }, // kill wrong arch
            libc::sock_filter {
                code: 0x20,
                jt: 0,
                jf: 0,
                k: 0,
            }, // syscall nr
            libc::sock_filter {
                code: 0x35,
                jt: 0,
                jf: 1,
                k: 0x4000_0000,
            },
            libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x8000_0000,
            }, // kill x32
        ];
        for nr in [
            libc::SYS_fork,
            libc::SYS_vfork,
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_socket,
            libc::SYS_socketpair,
            libc::SYS_connect,
            libc::SYS_bind,
            libc::SYS_listen,
            libc::SYS_accept,
            libc::SYS_accept4,
            libc::SYS_sendto,
            libc::SYS_recvfrom,
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
        ] {
            insns.push(libc::sock_filter {
                code: 0x15,
                jt: 0,
                jf: 1,
                k: nr as u32,
            });
            insns.push(libc::sock_filter {
                code: 0x06,
                jt: 0,
                jf: 0,
                k: 0x0005_0000 | libc::EPERM as u32,
            });
        }
        insns.push(libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: 0x7fff_0000,
        });
        Ok(insns)
    }
}

impl HardeningPlan {
    pub fn prepare(
        python_path: &Path,
        workdir: &Path,
        env_site_packages: Option<&Path>,
        memory_mb: u64,
        max_open_files: u64,
    ) -> Result<Self, &'static str> {
        let mut dirs = BTreeSet::<PathBuf>::new();
        let mut files = BTreeSet::<PathBuf>::new();
        dirs.insert(workdir.to_path_buf());
        if let Ok(canon) = fs::canonicalize(workdir) {
            dirs.insert(canon);
        }
        for candidate in ["/usr/lib", "/usr/lib64", "/lib", "/lib64"] {
            let path = Path::new(candidate);
            if path.is_dir() {
                dirs.insert(path.to_path_buf());
                if let Ok(canon) = fs::canonicalize(path)
                    && canon.is_dir()
                {
                    dirs.insert(canon);
                }
            }
        }
        if let Some(site) = env_site_packages {
            dirs.insert(site.to_path_buf());
            if let Ok(canon) = fs::canonicalize(site) {
                dirs.insert(canon);
            }
        }
        files.insert(python_path.to_path_buf());
        if let Ok(canon) = fs::canonicalize(python_path) {
            files.insert(canon);
        }
        for candidate in ["/etc/ld.so.cache", "/dev/null", "/dev/urandom"] {
            let path = Path::new(candidate);
            if path.exists() {
                files.insert(path.to_path_buf());
            }
        }
        let ro_dirs = dirs.iter().filter_map(|p| cstring(p)).collect();
        let ro_files = files.iter().filter_map(|p| cstring(p)).collect();
        Ok(Self {
            memory_bytes: memory_mb.saturating_mul(1024 * 1024),
            max_open_files,
            landlock_abi: probe_landlock_abi(),
            ro_dirs,
            ro_files,
            seccomp_filter: seccomp_program()?,
        })
    }

    unsafe fn apply_in_pre_exec(&self) -> std::io::Result<()> {
        // 1. Close any inherited descriptors beyond stdio (0, 1, 2).
        if unsafe {
            libc::syscall(
                SYS_CLOSE_RANGE,
                3 as libc::c_uint,
                u32::MAX,
                0 as libc::c_uint,
            )
        } != 0
        {
            for fd in 3..1024 {
                unsafe {
                    libc::close(fd);
                }
            }
        }
        // 2. Isolate process group so timeout/drop can reap the whole group.
        if unsafe { libc::setpgid(0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // 3. Memory, file-size, open-file, and core limits (no RLIMIT_CPU on persistent worker).
        for (resource, value) in [
            (libc::RLIMIT_AS, self.memory_bytes),
            (libc::RLIMIT_FSIZE, 0),
            (libc::RLIMIT_NOFILE, self.max_open_files),
            (libc::RLIMIT_CORE, 0),
        ] {
            let limit = libc::rlimit {
                rlim_cur: value,
                rlim_max: value,
            };
            if unsafe { libc::setrlimit(resource, &limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        // 4. Disable privilege escalation before Landlock and seccomp.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // 5. Landlock filesystem & network containment when kernel supports it.
        if let Some(abi) = self.landlock_abi {
            unsafe { self.apply_landlock(abi)? };
        }
        // 6. Seccomp filter blocking sockets, forks/clones, and ptrace.
        let prog = libc::sock_fprog {
            len: self.seccomp_filter.len() as u16,
            filter: self.seccomp_filter.as_ptr() as *mut libc::sock_filter,
        };
        if unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &prog as *const libc::sock_fprog,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    unsafe fn apply_landlock(&self, abi: u32) -> std::io::Result<()> {
        let mut handled_fs = LANDLOCK_ACCESS_FS_EXECUTE
            | LANDLOCK_ACCESS_FS_WRITE_FILE
            | LANDLOCK_ACCESS_FS_READ_FILE
            | LANDLOCK_ACCESS_FS_READ_DIR
            | LANDLOCK_ACCESS_FS_REMOVE_DIR
            | LANDLOCK_ACCESS_FS_REMOVE_FILE
            | LANDLOCK_ACCESS_FS_MAKE_CHAR
            | LANDLOCK_ACCESS_FS_MAKE_DIR
            | LANDLOCK_ACCESS_FS_MAKE_REG
            | LANDLOCK_ACCESS_FS_MAKE_SOCK
            | LANDLOCK_ACCESS_FS_MAKE_FIFO
            | LANDLOCK_ACCESS_FS_MAKE_BLOCK
            | LANDLOCK_ACCESS_FS_MAKE_SYM;
        if abi >= 2 {
            handled_fs |= LANDLOCK_ACCESS_FS_REFER;
        }
        if abi >= 3 {
            handled_fs |= LANDLOCK_ACCESS_FS_TRUNCATE;
        }
        if abi >= 5 {
            handled_fs |= LANDLOCK_ACCESS_FS_IOCTL_DEV;
        }
        let handled_net = if abi >= 4 {
            LANDLOCK_ACCESS_NET_BIND_TCP | LANDLOCK_ACCESS_NET_CONNECT_TCP
        } else {
            0
        };
        let scoped = if abi >= 6 {
            LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET | LANDLOCK_SCOPE_SIGNAL
        } else {
            0
        };
        let attr_size = if abi >= 6 {
            std::mem::size_of::<LandlockRulesetAttr>()
        } else if abi >= 4 {
            16
        } else {
            8
        };
        let attr = LandlockRulesetAttr {
            handled_access_fs: handled_fs,
            handled_access_net: handled_net,
            scoped,
        };
        let ruleset_fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const LandlockRulesetAttr,
                attr_size,
                0u32,
            )
        };
        if ruleset_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let ruleset_fd = ruleset_fd as i32;
        let dir_access =
            LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE | LANDLOCK_ACCESS_FS_READ_DIR;
        let file_access = LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE;
        for (paths, access) in [(&self.ro_dirs, dir_access), (&self.ro_files, file_access)] {
            for path in paths {
                let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
                if fd < 0 {
                    continue;
                }
                let rule = LandlockPathBeneathAttr {
                    allowed_access: access,
                    parent_fd: fd,
                };
                let rc = unsafe {
                    libc::syscall(
                        SYS_LANDLOCK_ADD_RULE,
                        ruleset_fd,
                        LANDLOCK_RULE_PATH_BENEATH,
                        &rule as *const LandlockPathBeneathAttr,
                        0u32,
                    )
                };
                unsafe {
                    libc::close(fd);
                }
                if rc != 0 {
                    let err = std::io::Error::last_os_error();
                    unsafe {
                        libc::close(ruleset_fd);
                    }
                    return Err(err);
                }
            }
        }
        let rc = unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(ruleset_fd);
        }
        if rc != 0 {
            return Err(err);
        }
        Ok(())
    }
}

/// Install full OS hardening + process-group isolation on `cmd`.
pub fn install_pre_exec(cmd: &mut Command, plan: HardeningPlan) {
    // SAFETY: `apply_in_pre_exec` uses only async-signal-safe syscalls on
    // pre-allocated buffers owned by `plan`.
    unsafe {
        cmd.pre_exec(move || plan.apply_in_pre_exec());
    }
}

/// Install process-group isolation only (without rlimits/Landlock/seccomp).
pub fn install_setpgid_only(cmd: &mut Command) {
    // SAFETY: `setpgid` is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Send `SIGKILL` to the process group `-pgid` when `pgid > 0`.
pub fn kill_process_group(pgid: i32) {
    if pgid > 0 {
        // SAFETY: killing a live child process group by negative pgid.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

pub use tempfile::TempDir;

/// Create a `0700` temporary working directory for a worker process.
pub fn private_workdir(prefix: &str) -> std::io::Result<TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new().prefix(prefix).tempdir()?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}
