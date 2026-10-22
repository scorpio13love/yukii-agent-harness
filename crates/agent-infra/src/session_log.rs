//! Per-session append-only JSONL event log：Session 历史的事实源；SQLite 仍作为查询索引。
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{OpenOptions, create_dir_all},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use yukii_agent_ports::model::{ImageContent, Message, ToolCall, ToolResult};

use crate::sqlite::StoreError;

pub struct SessionLog {
    root: PathBuf,
    lock: Mutex<()>,
    sequences: Mutex<HashMap<String, u64>>,
}

impl SessionLog {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Mutex::new(()),
            sequences: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, session_id: &str) -> PathBuf {
        self.root.join(session_id).join("session.jsonl")
    }

    /// 事件 id 幂等；重复回填不会产生重复行。
    pub fn append(
        &self,
        session_id: &str,
        event_id: &str,
        event_type: &str,
        data: Value,
    ) -> Result<(), StoreError> {
        let _guard = self.lock.lock().map_err(|_| StoreError::Lock)?;
        let path = self.path(session_id);
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        // 崩溃可能留下没有换行的撕裂尾行；先截断到最后完整行，避免新事件粘在坏 JSON 后。
        if path.exists() {
            let bytes = std::fs::read(&path)?;
            if bytes.last().is_some_and(|byte| *byte != b'\n') {
                let keep = bytes
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map(|index| index + 1)
                    .unwrap_or(0);
                let file = OpenOptions::new().write(true).open(&path)?;
                file.set_len(keep as u64)?;
                file.sync_all()?;
            }
        }
        let needs_header = !path.exists() || std::fs::metadata(&path)?.len() == 0;
        if needs_header {
            let header = json!({
                "format": 1,
                "type": "session/header",
                "session_id": session_id,
                "created_at": crate::sqlite::now(),
            });
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            serde_json::to_writer(&mut file, &header)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        if path.exists() {
            let file = std::fs::File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = line?;
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && value["id"].as_str() == Some(event_id)
                {
                    return Ok(());
                }
            }
        }
        let mut sequences = self.sequences.lock().map_err(|_| StoreError::Lock)?;
        let next = sequences.entry(session_id.to_owned()).or_insert_with(|| {
            if path.exists() {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| {
                        text.lines()
                            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                            .filter_map(|value| value["seq"].as_u64())
                            .max()
                    })
                    .unwrap_or(0)
            } else {
                0
            }
        });
        *next += 1;
        let sequence = *next;
        let event = json!({
            "format": 1,
            "seq": sequence,
            "id": event_id,
            "session_id": session_id,
            "type": event_type,
            "data": data,
            "created_at": crate::sqlite::now(),
        });
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        serde_json::to_writer(&mut file, &event)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        self.write_checkpoint(session_id, sequence)?;
        Ok(())
    }

    /// 把图片写成 Session 内文件，JSONL 只保留引用，避免日志膨胀。
    pub fn save_images(
        &self,
        session_id: &str,
        task_id: &str,
        call_id: &str,
        images: &[ImageContent],
    ) -> Result<Vec<Value>, StoreError> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let dir = self.root.join(session_id).join("images");
        create_dir_all(&dir)?;
        let mut refs = Vec::new();
        for (index, image) in images.iter().enumerate() {
            let bytes = STANDARD.decode(image.base64.as_bytes()).map_err(|error| {
                StoreError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })?;
            // 文件名不使用模型提供的 task/call id，避免路径注入；只保留随机安全名。
            let _ = (task_id, call_id, index);
            let name = format!(
                "{}.{}",
                uuid::Uuid::new_v4(),
                if image.media_type == "image/jpeg" {
                    "jpg"
                } else {
                    "png"
                }
            );
            std::fs::write(dir.join(&name), bytes)?;
            refs.push(json!({
                "file": name,
                "media_type": image.media_type,
                "width": image.width,
                "height": image.height,
            }));
        }
        Ok(refs)
    }

    fn load_image(&self, session_id: &str, reference: &Value) -> Option<ImageContent> {
        let name = reference["file"].as_str()?;
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            return None;
        }
        let bytes = std::fs::read(self.root.join(session_id).join("images").join(name)).ok()?;
        if bytes.len() > 4 * 1024 * 1024 {
            return None;
        }
        Some(ImageContent {
            media_type: reference["media_type"]
                .as_str()
                .unwrap_or("image/png")
                .to_owned(),
            base64: STANDARD.encode(bytes),
            width: reference["width"].as_u64().unwrap_or(0) as u32,
            height: reference["height"].as_u64().unwrap_or(0) as u32,
        })
    }

    fn prune_images_for_tasks(
        &self,
        session_id: &str,
        events: &[Value],
        tasks: &[String],
    ) -> Result<(), StoreError> {
        let images_dir = self.root.join(session_id).join("images");
        for event in events {
            if event["type"] != "tool/result"
                || !event["data"]["task_id"]
                    .as_str()
                    .is_some_and(|task| tasks.iter().any(|item| item == task))
            {
                continue;
            }
            if let Some(refs) = event["data"]["images"].as_array() {
                for reference in refs {
                    if let Some(name) = reference["file"].as_str()
                        && !name.contains('/')
                        && !name.contains('\\')
                        && !name.contains("..")
                    {
                        let _ = std::fs::remove_file(images_dir.join(name));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn checkpoint_seq(&self, session_id: &str) -> Result<Option<u64>, StoreError> {
        let path = self.root.join(session_id).join("checkpoint.json");
        if !path.exists() {
            return Ok(None);
        }
        let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(value["last_seq"].as_u64())
    }

    /// 用实际 JSONL 尾部修复 checkpoint；崩溃后 checkpoint 落后/超前都能收敛。
    pub fn repair_checkpoint(&self, session_id: &str) -> Result<Option<u64>, StoreError> {
        let events = self.read(session_id)?;
        let last = events
            .iter()
            .filter_map(|event| event["seq"].as_u64())
            .max();
        if let Some(last) = last {
            let known = self.checkpoint_seq(session_id)?;
            if known != Some(last) {
                self.write_checkpoint(session_id, last)?;
            }
            Ok(Some(last))
        } else {
            Ok(None)
        }
    }

    pub fn repair_all(&self) -> Result<(), StoreError> {
        if !self.root.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.root)?.flatten() {
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                let _ = self.repair_checkpoint(name)?;
            }
        }
        Ok(())
    }

    fn write_checkpoint(&self, session_id: &str, last_seq: u64) -> Result<(), StoreError> {
        let dir = self.root.join(session_id);
        create_dir_all(&dir)?;
        let path = dir.join("checkpoint.json");
        // 多个 TaskStore 可能共享同一 Session 根目录；临时名必须唯一，避免并发 rename 互相覆盖。
        let temp = dir.join(format!("checkpoint.json.{}.tmp", uuid::Uuid::new_v4()));
        let value = json!({
            "format": 1,
            "session_id": session_id,
            "last_seq": last_seq,
            "updated_at": crate::sqlite::now(),
        });
        std::fs::write(&temp, serde_json::to_vec(&value)?)?;
        std::fs::rename(temp, path)?;
        Ok(())
    }

    /// 登记一段被 surface replacement 隐藏的事件；事件本身保留，历史推导时过滤。
    pub fn replace(
        &self,
        session_id: &str,
        start_seq: u64,
        end_seq: u64,
    ) -> Result<(), StoreError> {
        self.append(
            session_id,
            &format!("replace:{start_seq}:{end_seq}"),
            "surface/replace",
            json!({"start_seq": start_seq, "end_seq": end_seq}),
        )
    }

    /// 按已完成 turn 数生成 surface replacement；不删除事件，只隐藏历史。
    pub fn replace_turns(&self, session_id: &str, turns: usize) -> Result<usize, StoreError> {
        if turns == 0 {
            return Ok(0);
        }
        let events = self.read(session_id)?;
        let mut order = Vec::new();
        for event in &events {
            if event["type"] == "user/message" {
                let task = event["data"]["task_id"].as_str().unwrap_or_default();
                if !task.is_empty() && !order.iter().any(|item| item == task) {
                    order.push(task.to_owned());
                }
            }
        }
        let selected: Vec<String> = order.into_iter().take(turns).collect();
        self.prune_images_for_tasks(session_id, &events, &selected)?;
        let seqs: Vec<u64> = events
            .iter()
            .filter(|event| {
                event["data"]["task_id"]
                    .as_str()
                    .is_some_and(|task| selected.iter().any(|item| item == task))
            })
            .filter_map(|event| event["seq"].as_u64())
            .collect();
        let (Some(start), Some(end)) = (seqs.iter().min(), seqs.iter().max()) else {
            return Ok(0);
        };
        self.replace(session_id, *start, *end)?;
        Ok(seqs.len())
    }

    pub fn read(&self, session_id: &str) -> Result<Vec<Value>, StoreError> {
        let path = self.path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(path)?;
        let mut events = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value["type"] == "session/header" {
                    let format = value["format"].as_u64().unwrap_or(0);
                    if format > 1 {
                        return Err(StoreError::SessionFormat(format.to_string()));
                    }
                    continue;
                }
                events.push(value);
            }
        }
        Ok(events)
    }

    /// 从日志推导到 before_task 之前的历史消息；工具调用/结果保留真实结构。
    pub fn history(&self, session_id: &str, before_task: &str) -> Result<Vec<Message>, StoreError> {
        let events = self.read(session_id)?;
        let replacements: Vec<(u64, u64)> = events
            .iter()
            .filter(|event| event["type"] == "surface/replace")
            .filter_map(|event| {
                Some((
                    event["data"]["start_seq"].as_u64()?,
                    event["data"]["end_seq"].as_u64()?,
                ))
            })
            .collect();
        let mut turns: Vec<Turn> = Vec::new();
        let mut index = BTreeMap::new();
        let mut summary = None;
        let mut compaction_through = 0usize;
        for event in events {
            let kind = event["type"].as_str().unwrap_or_default();
            let seq = event["seq"].as_u64().unwrap_or(0);
            if replacements
                .iter()
                .any(|(start, end)| seq >= *start && seq <= *end)
            {
                continue;
            }
            let data = &event["data"];
            let task_id = data["task_id"].as_str().unwrap_or_default();
            if kind == "compaction" {
                summary = data["summary"].as_str().map(ToOwned::to_owned);
                compaction_through = data["through_turn"]
                    .as_u64()
                    .map(|value| value as usize)
                    .unwrap_or(0)
                    .max(compaction_through);
                continue;
            }
            if !task_id.is_empty() && task_id == before_task {
                break;
            }
            if kind == "user/message" {
                index.insert(task_id.to_owned(), turns.len());
                turns.push(Turn {
                    task_id: task_id.to_owned(),
                    prompt: data["prompt"].as_str().unwrap_or_default().into(),
                    answer: String::new(),
                    calls: Vec::new(),
                    results: Vec::new(),
                });
                continue;
            }
            let Some(position) = index.get(task_id).copied() else {
                continue;
            };
            match kind {
                "assistant/message" => {
                    turns[position].answer = data["content"].as_str().unwrap_or_default().into();
                }
                "tool/call" => turns[position].calls.push((
                    data["call_id"].as_str().unwrap_or("tool").into(),
                    data["name"].as_str().unwrap_or("unknown").into(),
                    data["arguments"]
                        .as_str()
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .unwrap_or_else(|| json!({})),
                )),
                "tool/result" => {
                    let mut content = data["content"].as_str().unwrap_or_default().to_owned();
                    let mut images = Vec::new();
                    if let Some(refs) = data["images"].as_array() {
                        for reference in refs {
                            if let Some(image) = self.load_image(session_id, reference) {
                                images.push(image);
                            }
                        }
                    } else if let Some(count) = data["images"].as_u64()
                        && count > 0
                    {
                        content.push_str(&format!("\n[截图已省略 {count} 张]"));
                    }
                    turns[position].results.push((
                        data["call_id"].as_str().unwrap_or("tool").into(),
                        content,
                        data["is_error"].as_bool().unwrap_or(false),
                        images,
                    ));
                }
                _ => {}
            }
        }
        let mut messages = Vec::new();
        if let Some(summary) = summary {
            messages.push(Message::User(format!("[压缩历史摘要]\n{summary}")));
            messages.push(Message::Assistant {
                text: "历史已完成压缩。".into(),
                calls: Vec::new(),
            });
        }
        for turn in turns.into_iter().skip(compaction_through) {
            messages.push(Message::User(turn.prompt));
            if turn.calls.is_empty() {
                messages.push(Message::Assistant {
                    text: if turn.answer.is_empty() {
                        "[上一轮中断，未产生最终结果]".into()
                    } else {
                        turn.answer
                    },
                    calls: Vec::new(),
                });
                continue;
            }
            let calls = turn
                .calls
                .iter()
                .map(|(id, name, arguments)| ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                })
                .collect::<Vec<_>>();
            let results = turn
                .results
                .iter()
                .map(|(call_id, content, is_error, images)| ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                    images: images.clone(),
                })
                .collect::<Vec<_>>();
            messages.push(Message::Assistant {
                text: turn.answer,
                calls,
            });
            if !results.is_empty() {
                messages.push(Message::ToolResults(results));
            }
        }
        // 只保留最近一次工具截图的视觉内容，旧图保留文字。
        let last_image = messages.iter().rposition(|message| {
            matches!(message, Message::ToolResults(results) if results.iter().any(|result| !result.images.is_empty()))
        });
        for (index, message) in messages.iter_mut().enumerate() {
            if Some(index) == last_image {
                continue;
            }
            if let Message::ToolResults(results) = message {
                for result in results {
                    if result.images.is_empty() {
                        continue;
                    }
                    result.images.clear();
                    result.content.push_str("\n[较早的截图已省略]");
                }
            }
        }
        Ok(messages)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), StoreError> {
        let dir = self.root.join(session_id);
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        Ok(())
    }
}

struct Turn {
    #[allow(dead_code)]
    task_id: String,
    prompt: String,
    answer: String,
    calls: Vec<(String, String, Value)>,
    results: Vec<(String, String, bool, Vec<ImageContent>)>,
}

pub fn root_for_db(db_path: &Path) -> PathBuf {
    if let Ok(root) = std::env::var("YUKII_SESSION_LOG_ROOT") {
        return PathBuf::from(root);
    }
    // 内存库没有持久目录；Session 日志放临时目录，避免测试污染仓库工作区。
    if db_path == Path::new(":memory:") {
        return std::env::temp_dir().join(format!("yukii-memory-sessions-{}", std::process::id()));
    }
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("yukii-session-log-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn ignores_torn_tail_and_keeps_appending() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        let path = log.path("s");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"torn\":").unwrap();
        file.sync_all().unwrap();
        assert_eq!(log.read("s").unwrap().len(), 1);
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "answer"}),
        )
        .unwrap();
        assert_eq!(log.read("s").unwrap().len(), 2);
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(2));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writes_checkpoint_and_closes_open_turn() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "unfinished"}),
        )
        .unwrap();
        let checkpoint: Value =
            serde_json::from_slice(&std::fs::read(root.join("s/checkpoint.json")).unwrap())
                .unwrap();
        assert_eq!(checkpoint["last_seq"], 1);
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(1));
        let history = log.history("s", "t2").unwrap();
        assert!(history.iter().any(
            |message| matches!(message, Message::Assistant { text, .. } if text.contains("中断"))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn surface_replace_hides_events() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "hidden"}),
        )
        .unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "hidden answer"}),
        )
        .unwrap();
        log.append(
            "s",
            "u2",
            "user/message",
            json!({"task_id": "t2", "prompt": "visible"}),
        )
        .unwrap();
        log.append(
            "s",
            "a2",
            "assistant/message",
            json!({"task_id": "t2", "content": "visible answer"}),
        )
        .unwrap();
        log.replace("s", 1, 2).unwrap();
        let history = log.history("s", "t3").unwrap();
        assert!(
            !history
                .iter()
                .any(|message| matches!(message, Message::User(text) if text == "hidden"))
        );
        assert!(
            history
                .iter()
                .any(|message| matches!(message, Message::User(text) if text == "visible"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replace_turns_hides_compacted_events() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "answer one"}),
        )
        .unwrap();
        log.append(
            "s",
            "u2",
            "user/message",
            json!({"task_id": "t2", "prompt": "two"}),
        )
        .unwrap();
        log.append(
            "s",
            "a2",
            "assistant/message",
            json!({"task_id": "t2", "content": "answer two"}),
        )
        .unwrap();
        assert_eq!(log.replace_turns("s", 1).unwrap(), 2);
        // 重复 replace 不重复写事件
        assert_eq!(log.replace_turns("s", 1).unwrap(), 2);
        let history = log.history("s", "t3").unwrap();
        assert!(
            !history
                .iter()
                .any(|m| matches!(m, Message::User(text) if text == "one"))
        );
        assert!(
            history
                .iter()
                .any(|m| matches!(m, Message::User(text) if text == "two"))
        );
        assert_eq!(
            log.read("s")
                .unwrap()
                .iter()
                .filter(|e| e["type"] == "surface/replace")
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn skips_malformed_middle_line() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        let path = log.path("s");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{broken}\n").unwrap();
        file.sync_all().unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "answer"}),
        )
        .unwrap();
        assert_eq!(log.read("s").unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repairs_checkpoint_after_tail_loss() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "answer"}),
        )
        .unwrap();
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(2));
        // 模拟尾事件丢失，只保留 header + 第一条事件
        let path = log.path("s");
        let text = std::fs::read_to_string(&path).unwrap();
        let first = text.lines().take(2).collect::<Vec<_>>().join("\n") + "\n";
        std::fs::write(&path, first).unwrap();
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(2));
        assert_eq!(log.repair_checkpoint("s").unwrap(), Some(1));
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_marks_omitted_tool_images() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "look"}),
        )
        .unwrap();
        log.append(
            "s",
            "tc1",
            "tool/call",
            json!({"task_id": "t1", "call_id": "c1", "name": "cua_observe", "arguments": "{}"}),
        )
        .unwrap();
        log.append("s", "tr1", "tool/result", json!({"task_id": "t1", "call_id": "c1", "content": "observed", "is_error": false, "images": 2})).unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "done"}),
        )
        .unwrap();
        let history = log.history("s", "t2").unwrap();
        assert!(history.iter().any(|message| matches!(message, Message::ToolResults(results) if results[0].content.contains("截图已省略 2 张"))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replays_only_latest_tool_image() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        std::fs::create_dir_all(root.join("s/images")).unwrap();
        std::fs::write(root.join("s/images/one.png"), b"png").unwrap();
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "look"}),
        )
        .unwrap();
        log.append(
            "s",
            "tc1",
            "tool/call",
            json!({"task_id": "t1", "call_id": "c1", "name": "observe", "arguments": "{}"}),
        )
        .unwrap();
        log.append("s", "tr1", "tool/result", json!({"task_id": "t1", "call_id": "c1", "content": "old", "is_error": false, "images": [{"file": "one.png", "media_type": "image/png", "width": 1, "height": 1}]})).unwrap();
        log.append(
            "s",
            "tc2",
            "tool/call",
            json!({"task_id": "t2", "call_id": "c2", "name": "observe", "arguments": "{}"}),
        )
        .unwrap();
        log.append("s", "tr2", "tool/result", json!({"task_id": "t2", "call_id": "c2", "content": "new", "is_error": false, "images": [{"file": "one.png", "media_type": "image/png", "width": 1, "height": 1}]})).unwrap();
        log.append(
            "s",
            "u2",
            "user/message",
            json!({"task_id": "t2", "prompt": "again"}),
        )
        .unwrap();
        let history = log.history("s", "t3").unwrap();
        let with_images: Vec<_> = history
            .iter()
            .filter_map(|message| match message {
                Message::ToolResults(results) => Some(
                    results
                        .iter()
                        .filter(|result| !result.images.is_empty())
                        .count(),
                ),
                _ => None,
            })
            .collect();
        assert_eq!(with_images.iter().sum::<usize>(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replace_turns_removes_hidden_images() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        std::fs::create_dir_all(root.join("s/images")).unwrap();
        std::fs::write(root.join("s/images/old.png"), b"png").unwrap();
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        log.append("s", "tr1", "tool/result", json!({"task_id": "t1", "call_id": "c1", "content": "old", "is_error": false, "images": [{"file": "old.png", "media_type": "image/png", "width": 1, "height": 1}]})).unwrap();
        log.append(
            "s",
            "u2",
            "user/message",
            json!({"task_id": "t2", "prompt": "two"}),
        )
        .unwrap();
        log.replace_turns("s", 1).unwrap();
        assert!(!root.join("s/images/old.png").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_appends_keep_unique_sequences() {
        use std::sync::Arc;
        let root = temp_root();
        let log = Arc::new(SessionLog::new(root.clone()));
        let mut handles = Vec::new();
        for thread in 0..8 {
            let log = log.clone();
            handles.push(std::thread::spawn(move || {
                for index in 0..25 {
                    log.append(
                        "s",
                        &format!("e-{thread}-{index}"),
                        "usage",
                        json!({"task_id": "t"}),
                    )
                    .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let events = log.read("s").unwrap();
        assert_eq!(events.len(), 200);
        let mut sequences: Vec<u64> = events
            .iter()
            .filter_map(|event| event["seq"].as_u64())
            .collect();
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=200).collect::<Vec<_>>());
        assert_eq!(log.checkpoint_seq("s").unwrap(), Some(200));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn image_names_are_not_model_controlled() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        let refs = log
            .save_images(
                "s",
                "task",
                "../../evil",
                &[ImageContent {
                    media_type: "image/png".into(),
                    base64: "cG5n".into(),
                    width: 1,
                    height: 1,
                }],
            )
            .unwrap();
        let name = refs[0]["file"].as_str().unwrap();
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
        assert!(root.join("s/images").join(name).is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_format_header() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        std::fs::create_dir_all(root.join("s")).unwrap();
        std::fs::write(
            root.join("s/session.jsonl"),
            "{\"format\":2,\"type\":\"session/header\",\"session_id\":\"s\"}\n",
        )
        .unwrap();
        assert!(matches!(log.read("s"), Err(StoreError::SessionFormat(_))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn appends_idempotently_and_skips_compacted_turns() {
        let root = temp_root();
        let log = SessionLog::new(root.clone());
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        log.append(
            "s",
            "a1",
            "assistant/message",
            json!({"task_id": "t1", "content": "answer one"}),
        )
        .unwrap();
        log.append(
            "s",
            "u2",
            "user/message",
            json!({"task_id": "t2", "prompt": "two"}),
        )
        .unwrap();
        log.append(
            "s",
            "a2",
            "assistant/message",
            json!({"task_id": "t2", "content": "answer two"}),
        )
        .unwrap();
        // 重复 append 不得增加事件
        log.append(
            "s",
            "u1",
            "user/message",
            json!({"task_id": "t1", "prompt": "one"}),
        )
        .unwrap();
        assert_eq!(log.read("s").unwrap().len(), 4);
        log.append(
            "s",
            "c1",
            "compaction",
            json!({"summary": "summary", "through_turn": 1}),
        )
        .unwrap();
        let history = log.history("s", "t3").unwrap();
        assert!(matches!(&history[0], Message::User(text) if text.contains("summary")));
        assert!(
            history
                .iter()
                .any(|message| matches!(message, Message::User(text) if text == "two"))
        );
        assert!(
            !history
                .iter()
                .any(|message| matches!(message, Message::User(text) if text == "one"))
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
