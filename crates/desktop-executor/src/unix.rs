//! 将不安全的系统调用限制在此模块；文件描述符交由 File 所有权管理。
use std::{
    ffi::CString,
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::ffi::OsStrExt,
    path::Path,
};

pub(crate) fn open_root(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "目录路径含 NUL"))?;
    // SAFETY: path 是有效且以 NUL 结尾的字符串；不含 O_CREAT，不需要 mode 参数。
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    own_descriptor(descriptor)
}

/// name 必须为单个已验证组件；每一层禁止跟随符号链接，不拼接宿主绝对路径。
pub(crate) fn open_child(parent: &File, name: &str, flags: i32) -> io::Result<File> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "非法路径组件"));
    }
    let name = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))?;
    // SAFETY: parent 在调用期间持有有效 fd；name 以 NUL 结尾；mode 仅在 O_CREAT 时使用。
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    own_descriptor(descriptor)
}

fn own_descriptor(descriptor: i32) -> io::Result<File> {
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: descriptor 是上述 open/openat 新返回的 fd，未交给其他所有者。
    Ok(unsafe { File::from_raw_fd(descriptor) })
}
