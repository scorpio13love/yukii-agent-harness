use crate::{model::ToolDefinition, repository::RepositoryError};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
pub use yukii_contracts::ExecutionTarget;
use yukii_contracts::desktop::GuiAction;

/// 工具不能直接修改任务终态；产物由 Runtime 按任务权限提交。
pub enum ToolEffect {
    Result(Value),
    /// 结果正文加截图。图像只来自受控观察工具，由模型网关按协议编码。
    ResultWithImages {
        value: Value,
        images: Vec<crate::model::ImageContent>,
    },
    Plan(Vec<String>),
    Document {
        title: String,
        content: String,
    },
    /// 二进制产物；由 Runtime 以 base64 存入 artifact，下载时按 media_type 解码。
    Artifact {
        title: String,
        filename: String,
        media_type: String,
        base64: String,
    },
}
pub struct ToolContext {
    pub task_id: String,
    pub call_id: String,
    // 由受信 Runtime 构造，不接受模型参数中的 target 或授权标记。
    target: ExecutionTarget,
}

impl ToolContext {
    /// 网页办公上下文；桌面授权服务接入前不提供本机上下文构造入口。
    /// 桌面 Host 通过 `desktop` 显式构造本机上下文，二者不能由模型参数切换。
    pub fn office(task_id: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            call_id: call_id.into(),
            target: ExecutionTarget::CloudOffice,
        }
    }

    pub fn desktop(task_id: impl Into<String>, call_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            call_id: call_id.into(),
            target: ExecutionTarget::LocalComputer,
        }
    }

    pub fn target(&self) -> ExecutionTarget {
        self.target
    }
}

/// 受信桌面 Host 暴露给计算机工具的最小控制面。实现负责 IPC、用户审批和进程监督；
/// 工具本身不持有原生桥路径，也不能扩大窗口或系统权限。
#[async_trait]
pub trait DesktopControl: Send + Sync {
    async fn observe(&self, task_id: &str) -> Result<DesktopObservationResult, String>;
    async fn act(
        &self,
        task_id: &str,
        call_id: &str,
        observation_id: &str,
        action: GuiAction,
    ) -> Result<String, String>;
}

pub struct DesktopObservationResult {
    pub observation_id: String,
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    /// 平台可访问性树快照；工具优先使用 element_id，坐标动作作为回退。
    pub elements: Vec<yukii_contracts::desktop::UiElement>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    /// 工具声明其执行位置；注册或启用插件并不改变调用方拥有的能力。
    /// 这不是文件权限或操作系统隔离，桌面工具仍需独立的授权与执行边界。
    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }
    async fn execute(&self, context: &ToolContext, arguments: Value) -> Result<ToolEffect, String>;
}

/// 每次开始任务时读取已启用扩展，固定本次可见工具集合；撤销权限仍需执行时复查。
#[async_trait]
pub trait ToolCatalog: Send + Sync {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError>;

    /// 任务开始时按执行位置固定工具集合；工具存在不等于当前任务已获授权。
    async fn tools_for(
        &self,
        target: ExecutionTarget,
    ) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok(self
            .tools()
            .await?
            .into_iter()
            .filter(|tool| tool.execution_target() == target)
            .collect())
    }
}
