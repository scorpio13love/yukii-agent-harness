use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

/// 在恢复任务前独占数据目录；进程退出后由操作系统释放，避免残留 PID 锁。
pub struct InstanceLock {
    _file: File,
}
impl InstanceLock {
    pub fn acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock()
            .map_err(|_| io::Error::other("数据目录已被其他实例占用或不支持文件锁"))?;
        Ok(Self { _file: file })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_second_instance_and_releases_on_drop() {
        let path = std::env::temp_dir().join(format!("yukii-{}.lock", uuid::Uuid::new_v4()));
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(InstanceLock::acquire(&path).is_err());
        drop(first);
        drop(InstanceLock::acquire(&path).unwrap());
        std::fs::remove_file(path).unwrap();
    }
}
