use crate::unix::{open_child, open_root};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
};
use thiserror::Error;
use yukii_agent_domain::permissions::{validate_binding, validate_identity};
use yukii_agent_ports::permissions::{PermissionError, PermissionRepository};
use yukii_contracts::permissions::{FileOperation, PermissionBinding};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum FileExecutionError {
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error("授权工作目录不存在或不属于当前会话")]
    WorkspaceUnavailable,
    #[error("操作内容与审批范围不匹配")]
    ContentMismatch,
    #[error("文件超过 1 MiB 限制")]
    TooLarge,
    #[error("仅支持无硬链接的普通 UTF-8 文件")]
    UnsupportedFile,
    #[error("本机文件操作失败：{0}")]
    Filesystem(#[from] std::io::Error),
    #[error("新文件可能已部分写入，禁止自动重试，请核对文件：{0}")]
    WriteUncertain(std::io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileOutcome {
    Text(String),
    Created { bytes: usize, sha256: String },
}

struct Workspace {
    session_id: String,
    directory: File,
}

pub struct FileExecutor {
    permissions: Arc<dyn PermissionRepository>,
    workspaces: HashMap<String, Workspace>,
}

pub fn content_digest(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

impl FileExecutor {
    pub fn new(permissions: Arc<dyn PermissionRepository>) -> Self {
        Self {
            permissions,
            workspaces: HashMap::new(),
        }
    }

    /// 仅供受信宿主接收目录选择结果；不能作为模型工具或普通网页 API 暴露。
    /// 绑定已打开的目录句柄而非可替换的路径名，重启后必须重新注册和审批。
    pub fn register_workspace(
        &mut self,
        session_id: &str,
        path: &Path,
    ) -> Result<String, FileExecutionError> {
        let id = uuid::Uuid::new_v4().to_string();
        validate_identity(session_id)?;
        let directory = open_root(path)?;
        self.workspaces.insert(
            id.clone(),
            Workspace {
                session_id: session_id.into(),
                directory,
            },
        );
        Ok(id)
    }

    pub fn unregister_workspace(&mut self, workspace_id: &str) {
        self.workspaces.remove(workspace_id);
    }

    /// 一次批准只领取一次；领取后任意失败都不会恢复批准，避免不确定写入被重放。
    /// 同步文件 I/O 应由宿主放到阻塞线程，不能在 renderer 或 async worker 中直接运行。
    pub fn execute(
        &self,
        permission_id: &str,
        binding: &PermissionBinding,
        content: Option<&str>,
    ) -> Result<FileOutcome, FileExecutionError> {
        validate_binding(binding)?;
        let workspace = self
            .workspaces
            .get(&binding.scope.workspace_id)
            .filter(|workspace| workspace.session_id == binding.session_id)
            .ok_or(FileExecutionError::WorkspaceUnavailable)?;
        match (&binding.scope.operation, content) {
            (FileOperation::Read, None) => {}
            (FileOperation::Create { content_sha256 }, Some(content)) => {
                if content.len() > MAX_FILE_BYTES {
                    return Err(FileExecutionError::TooLarge);
                }
                if &content_digest(content) != content_sha256 {
                    return Err(FileExecutionError::ContentMismatch);
                }
            }
            _ => return Err(FileExecutionError::ContentMismatch),
        }
        let permit = self.permissions.claim_permission(permission_id, binding)?;
        let mut components = binding.scope.relative_path.split('/').collect::<Vec<_>>();
        let filename = components
            .pop()
            .ok_or(FileExecutionError::ContentMismatch)?;
        let mut parent = workspace.directory.try_clone()?;
        for component in components {
            parent = open_child(&parent, component, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        // 路径遍历后再查撤销和过期；检查与 OS 操作仍非同一事务，撤销不保证回滚。
        self.permissions.validate_permit(&permit)?;
        match content {
            None => {
                // NONBLOCK 避免误开 FIFO 时挂住；fstat 后仅允许普通、单链接文件。
                let file = open_child(&parent, filename, libc::O_RDONLY | libc::O_NONBLOCK)?;
                let metadata = file.metadata()?;
                if !metadata.is_file() || metadata.nlink() != 1 {
                    return Err(FileExecutionError::UnsupportedFile);
                }
                if metadata.len() > MAX_FILE_BYTES as u64 {
                    return Err(FileExecutionError::TooLarge);
                }
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(FileExecutionError::TooLarge);
                }
                let text =
                    String::from_utf8(bytes).map_err(|_| FileExecutionError::UnsupportedFile)?;
                Ok(FileOutcome::Text(text))
            }
            Some(content) => {
                // O_EXCL 原子拒绝已有文件与链接，绝不截断/覆盖用户文件。
                let mut file = open_child(
                    &parent,
                    filename,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                )?;
                file.write_all(content.as_bytes())
                    .and_then(|()| file.sync_all())
                    .and_then(|()| parent.sync_all())
                    .map_err(FileExecutionError::WriteUncertain)?;
                Ok(FileOutcome::Created {
                    bytes: content.len(),
                    sha256: content_digest(content),
                })
            }
        }
    }
}
