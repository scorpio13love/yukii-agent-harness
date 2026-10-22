//! 有界长度前缀 JSON 帧。纯传输，不创建监听端口，不负责授权。
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
use thiserror::Error;

// 1 MiB 文件经 JSON 控制字符转义可能放大六倍，仍必须有明确总帧上限。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("IPC 帧长度无效或超出上限")]
    Length,
    #[error("IPC 传输中断：{0}")]
    Io(#[from] std::io::Error),
    #[error("IPC JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
}

/// 仅帧边界上的 EOF 表示正常关闭；半个头或半个正文都按断连错误处理。
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>, FrameError> {
    let mut header = [0; 4];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    reader.read_exact(&mut header[1..])?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(FrameError::Length);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// 读取原始 JSON 值，供协议层在类型反序列化前拒绝未知字段。
pub fn read_frame_value(reader: &mut impl Read) -> Result<Option<serde_json::Value>, FrameError> {
    let mut header = [0; 4];
    loop {
        match reader.read(&mut header[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    reader.read_exact(&mut header[1..])?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(FrameError::Length);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Length);
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn multiple_frames_and_clean_eof_are_distinct_from_truncation() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &json!({"text":"你好\n"})).unwrap();
        write_frame(&mut bytes, &json!({"next":true})).unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(
            read_frame::<Value>(&mut reader).unwrap(),
            Some(json!({"text":"你好\n"}))
        );
        assert_eq!(
            read_frame::<Value>(&mut reader).unwrap(),
            Some(json!({"next":true}))
        );
        assert!(read_frame::<Value>(&mut reader).unwrap().is_none());
        for input in [&[0u8][..], &[0, 0, 0, 3, b'{'][..]] {
            assert!(matches!(
                read_frame::<Value>(&mut &input[..]),
                Err(FrameError::Io(_))
            ));
        }
    }

    #[test]
    fn invalid_lengths_and_json_fail_without_reading_unbounded_body() {
        for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
            assert!(matches!(
                read_frame::<Value>(&mut length.to_be_bytes().as_slice()),
                Err(FrameError::Length)
            ));
        }
        assert!(matches!(
            read_frame::<Value>(&mut &[0, 0, 0, 1, b'{'][..]),
            Err(FrameError::Json(_))
        ));
    }
}
