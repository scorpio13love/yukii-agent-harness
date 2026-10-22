//! 受控 Office 文档生成工具：Rust 负责权限/参数/超时，Python bridge 负责 OOXML 写入。
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::RepositoryError,
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};

const MAX_INPUT: usize = 200_000;
const MAX_OUTPUT: usize = 24 * 1024 * 1024;

#[derive(Clone)]
pub struct OfficeTools {
    python: PathBuf,
    bridge: PathBuf,
    uploads: PathBuf,
    timeout: Duration,
}

impl OfficeTools {
    pub fn from_env() -> Self {
        Self {
            python: std::env::var_os("YUKII_OFFICE_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("python3")),
            bridge: std::env::var_os("YUKII_OFFICE_BRIDGE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("services/office-tools/office_bridge.py")),
            uploads: std::env::var_os("YUKII_UPLOADS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".data/uploads")),
            timeout: std::env::var("YUKII_OFFICE_TIMEOUT_SECONDS")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| (1..=120).contains(value))
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(60)),
        }
    }

    pub fn new(python: PathBuf, bridge: PathBuf, timeout: Duration) -> Self {
        Self {
            python,
            bridge,
            uploads: PathBuf::from(".data/uploads"),
            timeout,
        }
    }
}

#[async_trait]
impl ToolCatalog for OfficeTools {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok(vec![
            Arc::new(OfficeTool {
                parent: self.clone(),
                kind: OfficeKind::Docx,
            }),
            Arc::new(OfficeTool {
                parent: self.clone(),
                kind: OfficeKind::Xlsx,
            }),
            Arc::new(OfficeTool {
                parent: self.clone(),
                kind: OfficeKind::Pptx,
            }),
            Arc::new(OfficeTool {
                parent: self.clone(),
                kind: OfficeKind::Pdf,
            }),
            Arc::new(ReadOfficeTool {
                parent: self.clone(),
                kind: ReadKind::Docx,
            }),
            Arc::new(ReadOfficeTool {
                parent: self.clone(),
                kind: ReadKind::Xlsx,
            }),
            Arc::new(ReadOfficeTool {
                parent: self.clone(),
                kind: ReadKind::Pptx,
            }),
            Arc::new(ReadOfficeTool {
                parent: self.clone(),
                kind: ReadKind::Pdf,
            }),
            Arc::new(ReadImageTool {
                parent: self.clone(),
            }),
            Arc::new(EditDocxTool {
                parent: self.clone(),
            }),
            Arc::new(EditXlsxTool {
                parent: self.clone(),
            }),
        ])
    }
}

#[derive(Clone, Copy)]
enum OfficeKind {
    Docx,
    Xlsx,
    Pptx,
    Pdf,
}

impl OfficeKind {
    fn name(self) -> &'static str {
        match self {
            Self::Docx => "create_docx",
            Self::Xlsx => "create_xlsx",
            Self::Pptx => "create_pptx",
            Self::Pdf => "create_pdf",
        }
    }

    fn format(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Pdf => "pdf",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Docx => "生成可下载的 Word .docx 文档；不写入用户任意路径，不接受宏或外部模板。",
            Self::Xlsx => "生成可下载的 Excel .xlsx 工作簿；工作表最多 20 个、总行数最多 5000。",
            Self::Pptx => "生成可下载的 PowerPoint .pptx 演示文稿；最多 100 张内容页。",
            Self::Pdf => {
                "将结构化标题/章节导出为 PDF；使用已安装的 LibreOffice Kit 转换并由 %PDF- 签名校验。"
            }
        }
    }

    fn schema(self) -> Value {
        match self {
            Self::Docx => json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "maxLength": 200},
                    "filename": {"type": "string", "maxLength": 120},
                    "sections": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "heading": {"type": "string", "maxLength": 200},
                                "body": {"type": "string", "maxLength": 20000}
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
            Self::Xlsx => json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "maxLength": 200},
                    "filename": {"type": "string", "maxLength": 120},
                    "sheets": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "maxLength": 31},
                                "rows": {
                                    "type": "array",
                                    "maxItems": 5000,
                                    "items": {
                                        "type": "array",
                                        "items": {"type": ["string", "number", "boolean", "null"]}
                                    }
                                }
                            },
                            "required": ["name", "rows"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["sheets"],
                "additionalProperties": false
            }),
            Self::Pdf => json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "maxLength": 200},
                    "filename": {"type": "string", "maxLength": 120},
                    "sections": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "heading": {"type": "string", "maxLength": 200},
                                "body": {"type": "string", "maxLength": 20000}
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
            Self::Pptx => json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "maxLength": 200},
                    "filename": {"type": "string", "maxLength": 120},
                    "slides": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string", "maxLength": 200},
                                "bullets": {
                                    "type": "array",
                                    "maxItems": 20,
                                    "items": {"type": "string", "maxLength": 500}
                                }
                            },
                            "required": ["title", "bullets"],
                            "additionalProperties": false
                        }
                    }
                },
                "additionalProperties": false
            }),
        }
    }
}

struct OfficeTool {
    parent: OfficeTools,
    kind: OfficeKind,
}

#[async_trait]
impl Tool for OfficeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.kind.name().into(),
            description: self.kind.description().into(),
            schema: self.kind.schema(),
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, _: &ToolContext, mut arguments: Value) -> Result<ToolEffect, String> {
        if !arguments.is_object() {
            return Err("Office 工具参数必须是对象".into());
        }
        arguments["format"] = json!(self.kind.format());
        let input = arguments.to_string();
        if input.len() > MAX_INPUT {
            return Err("Office 工具参数过大".into());
        }
        let mut command = Command::new(&self.parent.python);
        command
            .arg(&self.parent.bridge)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        // bridge 需要 LibreOffice Kit 的绝对路径；只显式传递这两个非秘密配置。
        for key in ["YUKII_OFFICE_PDF_NODE", "YUKII_OFFICE_PDF_CLI"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 Office bridge：{error}"))?;
        let mut stdin = child.stdin.take().ok_or("Office bridge stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("Office bridge stdout 不可用")?;
        let stderr = child.stderr.take().ok_or("Office bridge stderr 不可用")?;
        let run = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            let mut output = Vec::new();
            stdout
                .take(MAX_OUTPUT as u64 + 1)
                .read_to_end(&mut output)
                .await?;
            let mut error = Vec::new();
            stderr.take(16 * 1024 + 1).read_to_end(&mut error).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((output, error, status))
        };
        let (output, error, status) = tokio::time::timeout(self.parent.timeout, run)
            .await
            .map_err(|_| "Office bridge 执行超时".to_owned())?
            .map_err(|error| format!("Office bridge I/O 失败：{error}"))?;
        if output.len() > MAX_OUTPUT {
            return Err("Office bridge 输出过大".into());
        }
        if !status.success() {
            let detail = String::from_utf8_lossy(&error);
            return Err(format!(
                "Office bridge 执行失败：{}",
                detail.chars().take(300).collect::<String>()
            ));
        }
        let value: Value = serde_json::from_slice(&output)
            .map_err(|_| "Office bridge 必须返回单个 JSON".to_owned())?;
        let filename = value["filename"]
            .as_str()
            .ok_or("Office bridge 缺少 filename")?;
        let media_type = value["media_type"]
            .as_str()
            .ok_or("Office bridge 缺少 media_type")?;
        let base64 = value["base64"]
            .as_str()
            .ok_or("Office bridge 缺少 base64")?;
        if filename.is_empty()
            || filename.len() > 120
            || filename.contains('/')
            || filename.contains('\\')
        {
            return Err("Office bridge filename 无效".into());
        }
        if base64.is_empty() || base64.len() > MAX_OUTPUT {
            return Err("Office bridge 文件内容为空或过大".into());
        }
        Ok(ToolEffect::Artifact {
            title: arguments["title"].as_str().unwrap_or(filename).to_owned(),
            filename: filename.into(),
            media_type: media_type.into(),
            base64: base64.into(),
        })
    }
}

/// 解析旧全局上传或 Session 子目录中的唯一文件；拒绝多义/路径穿越。
fn resolve_upload(root: &std::path::Path, id: &str, extension: &str) -> Option<PathBuf> {
    let direct = root.join(format!("{id}.{extension}"));
    if direct.is_file() {
        return Some(direct);
    }
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let candidate = entry.path().join(format!("{id}.{extension}"));
        if candidate.is_file() {
            matches.push(candidate);
        }
    }
    (matches.len() == 1).then(|| matches.remove(0))
}

#[derive(Clone, Copy)]
enum ReadKind {
    Docx,
    Xlsx,
    Pptx,
    Pdf,
}

impl ReadKind {
    fn name(self) -> &'static str {
        match self {
            Self::Docx => "read_docx",
            Self::Xlsx => "read_xlsx",
            Self::Pptx => "read_pptx",
            Self::Pdf => "read_pdf",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Pdf => "pdf",
        }
    }
    fn bridge_format(self) -> &'static str {
        match self {
            Self::Docx => "read_docx",
            Self::Xlsx => "read_xlsx",
            Self::Pptx => "read_pptx",
            Self::Pdf => "read_pdf",
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::Docx => "读取已上传 .docx 的段落、标题和表格文本；只接受 uploads 文件 id。",
            Self::Xlsx => "读取已上传 .xlsx 的工作表名称和单元格值；只接受 uploads 文件 id。",
            Self::Pptx => "读取已上传 .pptx 的每页文本；只接受 uploads 文件 id。",
            Self::Pdf => "通过 macOS PDFKit 提取已上传 .pdf 的文本；只接受 uploads 文件 id。",
        }
    }
}

struct ReadOfficeTool {
    parent: OfficeTools,
    kind: ReadKind,
}

#[async_trait]
impl Tool for ReadOfficeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.kind.name().into(),
            description: self.kind.description().into(),
            schema: json!({"type":"object","properties":{"file_id":{"type":"string"}},"required":["file_id"],"additionalProperties":false}),
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let id = arguments
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{} 需要 file_id", self.kind.name()))?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("file_id 无效".into());
        }
        let path =
            resolve_upload(&self.parent.uploads, id, self.kind.extension()).ok_or_else(|| {
                format!(
                    "文件不存在、不是 {} 或 file_id 不唯一",
                    self.kind.extension()
                )
            })?;
        let input = json!({"format":self.kind.bridge_format(),"path":path}).to_string();
        let mut command = Command::new(&self.parent.python);
        command
            .arg(&self.parent.bridge)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 Office bridge：{error}"))?;
        let mut stdin = child.stdin.take().ok_or("Office bridge stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("Office bridge stdout 不可用")?;
        let stderr = child.stderr.take().ok_or("Office bridge stderr 不可用")?;
        let run = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            let mut output = Vec::new();
            stdout
                .take(MAX_OUTPUT as u64)
                .read_to_end(&mut output)
                .await?;
            let mut error = Vec::new();
            stderr.take(16 * 1024).read_to_end(&mut error).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((output, error, status))
        };
        let (output, error, status) = tokio::time::timeout(self.parent.timeout, run)
            .await
            .map_err(|_| "Office bridge 执行超时".to_owned())?
            .map_err(|error| format!("Office bridge I/O 失败：{error}"))?;
        if !status.success() {
            return Err(format!(
                "读取 {} 失败：{}",
                self.kind.extension(),
                String::from_utf8_lossy(&error)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        let value: Value =
            serde_json::from_slice(&output).map_err(|_| "Office bridge 返回非 JSON".to_owned())?;
        match self.kind {
            ReadKind::Docx | ReadKind::Pdf => {
                let text = value["text"].as_str().ok_or("Office bridge 缺少 text")?;
                Ok(ToolEffect::Result(Value::String(text.to_owned())))
            }
            ReadKind::Xlsx | ReadKind::Pptx => Ok(ToolEffect::Result(value)),
        }
    }
}

struct EditDocxTool {
    parent: OfficeTools,
}

#[async_trait]
impl Tool for EditDocxTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_docx".into(),
            description: "在已上传 .docx 末尾追加段落，并输出新的 artifact；不覆盖源文件。".into(),
            schema: json!({"type":"object","properties":{"file_id":{"type":"string"},"append":{"type":"array","maxItems":50,"items":{"type":"string","maxLength":4000}},"filename":{"type":"string","maxLength":120}},"required":["file_id","append"],"additionalProperties":false}),
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let id = arguments
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or("edit_docx 需要 file_id")?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("file_id 无效".into());
        }
        let source = resolve_upload(&self.parent.uploads, id, "docx")
            .ok_or("文件不存在、不是 docx 或 file_id 不唯一")?;
        let output = self
            .parent
            .uploads
            .join(format!("{id}-edited-{}.docx", uuid::Uuid::new_v4()));
        let filename = arguments
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("edited.docx");
        let input = json!({"format":"edit_docx","path":source,"output":output,"append":arguments.get("append").cloned().unwrap_or(json!([])),"filename":filename}).to_string();
        let mut command = Command::new(&self.parent.python);
        command
            .arg(&self.parent.bridge)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 Office bridge：{error}"))?;
        let mut stdin = child.stdin.take().ok_or("Office bridge stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("Office bridge stdout 不可用")?;
        let stderr = child.stderr.take().ok_or("Office bridge stderr 不可用")?;
        let run = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            let mut output = Vec::new();
            stdout
                .take(MAX_OUTPUT as u64)
                .read_to_end(&mut output)
                .await?;
            let mut error = Vec::new();
            stderr.take(16 * 1024).read_to_end(&mut error).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((output, error, status))
        };
        let (output, error, status) = tokio::time::timeout(self.parent.timeout, run)
            .await
            .map_err(|_| "Office bridge 执行超时".to_owned())?
            .map_err(|e| format!("Office bridge I/O 失败：{e}"))?;
        if !status.success() {
            return Err(format!(
                "edit_docx 失败：{}",
                String::from_utf8_lossy(&error)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        let value: Value =
            serde_json::from_slice(&output).map_err(|_| "Office bridge 返回非 JSON".to_owned())?;
        let base64 = value["base64"]
            .as_str()
            .ok_or("Office bridge 缺少 base64")?
            .to_owned();
        let filename = value["filename"]
            .as_str()
            .unwrap_or("edited.docx")
            .to_owned();
        let media_type = value["media_type"]
            .as_str()
            .unwrap_or("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
            .to_owned();
        Ok(ToolEffect::Artifact {
            title: "编辑后的 Word 文档".into(),
            filename,
            media_type,
            base64,
        })
    }
}

struct EditXlsxTool {
    parent: OfficeTools,
}

#[async_trait]
impl Tool for EditXlsxTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_xlsx".into(),
            description: "修改已上传 .xlsx 的指定单元格并输出新的 artifact；不覆盖源文件。".into(),
            schema: json!({"type":"object","properties":{"file_id":{"type":"string"},"cells":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","properties":{"sheet":{"type":"string"},"cell":{"type":"string"},"value":{}},"required":["sheet","cell","value"],"additionalProperties":false}},"filename":{"type":"string","maxLength":120}},"required":["file_id","cells"],"additionalProperties":false}),
        }
    }
    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }
    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let id = arguments
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or("edit_xlsx 需要 file_id")?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("file_id 无效".into());
        }
        let source = resolve_upload(&self.parent.uploads, id, "xlsx")
            .ok_or("文件不存在、不是 xlsx 或 file_id 不唯一")?;
        let output = self
            .parent
            .uploads
            .join(format!("{id}-edited-{}.xlsx", uuid::Uuid::new_v4()));
        let filename = arguments
            .get("filename")
            .and_then(Value::as_str)
            .unwrap_or("edited.xlsx");
        let input = json!({"format":"edit_xlsx","path":source,"output":output,"cells":arguments.get("cells").cloned().unwrap_or(json!([])),"filename":filename}).to_string();
        let mut command = Command::new(&self.parent.python);
        command
            .arg(&self.parent.bridge)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 Office bridge：{error}"))?;
        let mut stdin = child.stdin.take().ok_or("Office bridge stdin 不可用")?;
        let stdout = child.stdout.take().ok_or("Office bridge stdout 不可用")?;
        let stderr = child.stderr.take().ok_or("Office bridge stderr 不可用")?;
        let run = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            let mut output = Vec::new();
            stdout
                .take(MAX_OUTPUT as u64)
                .read_to_end(&mut output)
                .await?;
            let mut error = Vec::new();
            stderr.take(16 * 1024).read_to_end(&mut error).await?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((output, error, status))
        };
        let (output, error, status) = tokio::time::timeout(self.parent.timeout, run)
            .await
            .map_err(|_| "Office bridge 执行超时".to_owned())?
            .map_err(|e| format!("Office bridge I/O 失败：{e}"))?;
        if !status.success() {
            return Err(format!(
                "edit_xlsx 失败：{}",
                String::from_utf8_lossy(&error)
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        let value: Value =
            serde_json::from_slice(&output).map_err(|_| "Office bridge 返回非 JSON".to_owned())?;
        Ok(ToolEffect::Artifact {
            title: "编辑后的 Excel 工作簿".into(),
            filename: value["filename"].as_str().unwrap_or("edited.xlsx").into(),
            media_type: value["media_type"]
                .as_str()
                .unwrap_or("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
                .into(),
            base64: value["base64"]
                .as_str()
                .ok_or("Office bridge 缺少 base64")?
                .into(),
        })
    }
}

struct ReadImageTool {
    parent: OfficeTools,
}

#[async_trait]
impl Tool for ReadImageTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_image".into(),
            description: "读取已上传的 PNG/JPEG 图片并返回给视觉模型查看；只接受 uploads 文件 id。"
                .into(),
            schema: json!({"type":"object","properties":{"file_id":{"type":"string"}},"required":["file_id"],"additionalProperties":false}),
        }
    }
    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }
    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let id = arguments
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or("read_image 需要 file_id")?;
        if uuid::Uuid::parse_str(id).is_err() {
            return Err("file_id 无效".into());
        }
        let (path, media_type) = ["png", "jpg", "jpeg"]
            .iter()
            .find_map(|extension| {
                resolve_upload(&self.parent.uploads, id, extension).map(|path| {
                    (
                        path,
                        match *extension {
                            "png" => "image/png",
                            _ => "image/jpeg",
                        },
                    )
                })
            })
            .ok_or("文件不存在、不是 PNG/JPEG 或 file_id 不唯一")?;
        use base64::{Engine, engine::general_purpose::STANDARD};
        let bytes = std::fs::read(&path).map_err(|_| "无法读取图片")?;
        if bytes.len() > 7 * 1024 * 1024 {
            return Err("图片超过 7 MiB".into());
        }
        Ok(ToolEffect::ResultWithImages {
            value: json!({"filename": path.file_name().and_then(|v| v.to_str()).unwrap_or("image"), "note": "图片内容以 image content 返回。"}),
            images: vec![yukii_agent_ports::model::ImageContent {
                media_type: media_type.into(),
                base64: STANDARD.encode(bytes),
                width: 0,
                height: 0,
            }],
        })
    }
}
