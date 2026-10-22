//! Computer Use 的纯规则：控制期限、窗口身份、观察新鲜度与坐标映射。
//!
//! 这些检查不触碰 OS、不访问数据库，也不代表系统 TCC 权限已经授予。
use thiserror::Error;
use yukii_contracts::desktop::{DesktopWindow, GuiAction};

/// 一次用户同意最多覆盖 30 分钟；宿主必须在到期前结束或重新征得同意。
pub const MAX_CONTROL_SECONDS: i64 = 1800;
/// 截图只对紧邻的一次动作有效，避免根据过期画面盲点。
pub const MAX_OBSERVATION_SECONDS: i64 = 120;
pub const MAX_SCREENSHOT_PIXELS: u64 = 4_000_000;
pub const MAX_SCREENSHOT_BASE64_BYTES: usize = 7 * 1024 * 1024;
pub const MAX_WINDOW_SIDE: u32 = 16_384;
pub const MAX_TEXT_UTF16_UNITS: usize = 256;
pub const ALLOWED_KEYS: [&str; 8] = [
    "enter",
    "tab",
    "escape",
    "backspace",
    "left",
    "right",
    "down",
    "up",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ComputerError {
    #[error("窗口身份无效")]
    InvalidWindow,
    #[error("桌面控制期限无效或超出上限")]
    InvalidExpiration,
    #[error("截图尺寸、内容或窗口几何信息无效")]
    InvalidObservation,
    #[error("GUI 动作参数无效")]
    InvalidAction,
    #[error("观察已过期、已被使用或不属于当前控制会话")]
    ObservationUnavailable,
    #[error("桌面控制未激活、已暂停或已结束")]
    ControlUnavailable,
}

pub fn validate_window(window: &DesktopWindow) -> Result<(), ComputerError> {
    if window.window_id == 0 || window.pid <= 0 {
        return Err(ComputerError::InvalidWindow);
    }
    if window.application.trim().is_empty()
        || window.application.chars().count() > 256
        || window.title.chars().count() > 1024
        || window
            .application
            .chars()
            .chain(window.title.chars())
            .any(|character| character.is_control())
    {
        return Err(ComputerError::InvalidWindow);
    }
    Ok(())
}

pub fn validate_control_expiration(expires_at: i64, now: i64) -> Result<(), ComputerError> {
    if expires_at <= now || expires_at.saturating_sub(now) > MAX_CONTROL_SECONDS {
        return Err(ComputerError::InvalidExpiration);
    }
    Ok(())
}

/// 原生桥负责像素与 PNG 编码；这里拒绝异常尺寸，避免把超大图像送入模型或数据库。
pub fn validate_observation(
    width: u32,
    height: u32,
    png_base64_len: usize,
    frame: [f64; 4],
) -> Result<(), ComputerError> {
    if width == 0
        || height == 0
        || width > MAX_WINDOW_SIDE
        || height > MAX_WINDOW_SIDE
        || u64::from(width) * u64::from(height) > MAX_SCREENSHOT_PIXELS
        || png_base64_len == 0
        || png_base64_len > MAX_SCREENSHOT_BASE64_BYTES
    {
        return Err(ComputerError::InvalidObservation);
    }
    if !frame.iter().all(|value| value.is_finite())
        || frame[2] <= 0.0
        || frame[3] <= 0.0
        || frame[2] > MAX_WINDOW_SIDE as f64
        || frame[3] > MAX_WINDOW_SIDE as f64
    {
        return Err(ComputerError::InvalidObservation);
    }
    Ok(())
}

pub fn observation_is_usable(
    captured_at: i64,
    expires_at: i64,
    consumed_at: Option<i64>,
    now: i64,
) -> bool {
    consumed_at.is_none()
        && captured_at <= now
        && expires_at > now
        && expires_at.saturating_sub(captured_at) <= MAX_OBSERVATION_SECONDS
}

/// 校验动作并把截图像素坐标换算为窗口逻辑点。原生桥再独立校验窗口位置与前台应用。
pub fn to_window_action(
    action: &GuiAction,
    image_width: u32,
    image_height: u32,
    frame: [f64; 4],
) -> Result<GuiAction, ComputerError> {
    validate_observation(image_width, image_height, 1, frame)?;
    match action {
        GuiAction::Click { x, y } => {
            let (x, y) = map_point(*x, *y, image_width, image_height, frame)?;
            Ok(GuiAction::Click { x, y })
        }
        GuiAction::Scroll { x, y, delta_y } => {
            if !delta_y.is_finite() || delta_y.abs() < 1.0 || delta_y.abs() > 1000.0 {
                return Err(ComputerError::InvalidAction);
            }
            let (x, y) = map_point(*x, *y, image_width, image_height, frame)?;
            Ok(GuiAction::Scroll {
                x,
                y,
                delta_y: *delta_y,
            })
        }
        GuiAction::TypeText { text } | GuiAction::ReplaceText { text } => {
            if text.is_empty()
                || text.encode_utf16().count() > MAX_TEXT_UTF16_UNITS
                || text.chars().any(|character| character.is_control())
            {
                return Err(ComputerError::InvalidAction);
            }
            Ok(match action {
                GuiAction::ReplaceText { .. } => GuiAction::ReplaceText { text: text.clone() },
                _ => GuiAction::TypeText { text: text.clone() },
            })
        }
        GuiAction::PressKey { key } => {
            if !ALLOWED_KEYS.contains(&key.as_str()) {
                return Err(ComputerError::InvalidAction);
            }
            Ok(GuiAction::PressKey { key: key.clone() })
        }
        GuiAction::ClickElement { element_id } => {
            if !valid_element_id(element_id) {
                return Err(ComputerError::InvalidAction);
            }
            Ok(GuiAction::ClickElement {
                element_id: element_id.clone(),
            })
        }
        GuiAction::TypeElement {
            element_id,
            text,
            clear,
        } => {
            if !valid_element_id(element_id)
                || text.is_empty()
                || text.encode_utf16().count() > MAX_TEXT_UTF16_UNITS
                || text.chars().any(|character| character.is_control())
            {
                return Err(ComputerError::InvalidAction);
            }
            Ok(GuiAction::TypeElement {
                element_id: element_id.clone(),
                text: text.clone(),
                clear: *clear,
            })
        }
    }
}

/// 只接受原生树生成的稳定前缀；具体路径由平台适配器解析，坐标动作仍保留为回退。
fn valid_element_id(value: &str) -> bool {
    value.len() <= 128
        && ["ax-", "uia-", "atspi-"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn map_point(
    x: f64,
    y: f64,
    image_width: u32,
    image_height: u32,
    frame: [f64; 4],
) -> Result<(f64, f64), ComputerError> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return Err(ComputerError::InvalidAction);
    }
    // 原生桥按窗口长边做等比例缩放，宽度取整会带来极小的纵横比例差异。
    let scale_x = f64::from(image_width) / frame[2];
    let scale_y = f64::from(image_height) / frame[3];
    if scale_x <= 0.0 || scale_y <= 0.0 || (scale_x - scale_y).abs() > 0.02 {
        return Err(ComputerError::InvalidAction);
    }
    if x >= f64::from(image_width) || y >= f64::from(image_height) {
        return Err(ComputerError::InvalidAction);
    }
    let window_x = x / scale_x;
    // 截图、SCWindow.frame、CGEvent 与 AX 命中测试均使用上原点，y 不需要翻转。
    let window_y = y / scale_y;
    if window_x >= frame[2] || window_y >= frame[3] {
        return Err(ComputerError::InvalidAction);
    }
    Ok((window_x, window_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> DesktopWindow {
        DesktopWindow {
            window_id: 42,
            pid: 123,
            application: "Yukii Test".into(),
            title: "测试窗口".into(),
        }
    }

    #[test]
    fn rejects_unknown_windows_and_unbounded_control_lifetime() {
        assert!(validate_window(&window()).is_ok());
        let mut invalid = window();
        invalid.window_id = 0;
        assert_eq!(validate_window(&invalid), Err(ComputerError::InvalidWindow));
        invalid = window();
        invalid.title = "a\nb".into();
        assert_eq!(validate_window(&invalid), Err(ComputerError::InvalidWindow));
        assert!(validate_control_expiration(101, 100).is_ok());
        assert_eq!(
            validate_control_expiration(100, 100),
            Err(ComputerError::InvalidExpiration)
        );
        assert_eq!(
            validate_control_expiration(100 + MAX_CONTROL_SECONDS + 1, 100),
            Err(ComputerError::InvalidExpiration)
        );
    }

    #[test]
    fn observation_must_be_fresh_small_and_single_use() {
        assert!(validate_observation(1024, 768, 4096, [0.0, 0.0, 1024.0, 768.0]).is_ok());
        assert!(validate_observation(0, 768, 4096, [0.0, 0.0, 1024.0, 768.0]).is_err());
        assert!(validate_observation(4000, 4000, 4096, [0.0, 0.0, 1.0, 1.0]).is_err());
        assert!(validate_observation(10, 10, 0, [0.0, 0.0, 10.0, 10.0]).is_err());
        assert!(observation_is_usable(100, 200, None, 150));
        assert!(!observation_is_usable(100, 200, Some(120), 150));
        assert!(!observation_is_usable(100, 200, None, 200));
        assert!(!observation_is_usable(100, 400, None, 150));
    }

    #[test]
    fn pixel_coordinates_are_mapped_to_window_points_and_bounded() {
        let frame = [100.0, 200.0, 800.0, 600.0];
        let action =
            to_window_action(&GuiAction::Click { x: 400.0, y: 300.0 }, 800, 600, frame).unwrap();
        assert_eq!(action, GuiAction::Click { x: 400.0, y: 300.0 });
        let scaled =
            to_window_action(&GuiAction::Click { x: 200.0, y: 150.0 }, 400, 300, frame).unwrap();
        assert_eq!(scaled, GuiAction::Click { x: 400.0, y: 300.0 });
        for action in [
            GuiAction::Click { x: -1.0, y: 0.0 },
            GuiAction::Click { x: 400.0, y: 600.0 },
            GuiAction::Click {
                x: f64::NAN,
                y: 0.0,
            },
            GuiAction::Scroll {
                x: 0.0,
                y: 0.0,
                delta_y: 0.0,
            },
            GuiAction::TypeText {
                text: "换\n行".into(),
            },
            GuiAction::TypeText {
                text: "x".repeat(257),
            },
            GuiAction::PressKey {
                key: "volume_up".into(),
            },
        ] {
            assert_eq!(
                to_window_action(&action, 800, 600, frame),
                Err(ComputerError::InvalidAction),
                "{action:?}"
            );
        }
        assert!(
            to_window_action(
                &GuiAction::PressKey {
                    key: "enter".into()
                },
                800,
                600,
                frame
            )
            .is_ok()
        );
    }

    #[test]
    fn element_actions_require_platform_ids_and_bounded_text() {
        let frame = [100.0, 200.0, 800.0, 600.0];
        assert!(
            to_window_action(
                &GuiAction::ClickElement {
                    element_id: "ax-0.3".into()
                },
                800,
                600,
                frame
            )
            .is_ok()
        );
        assert!(
            to_window_action(
                &GuiAction::TypeElement {
                    element_id: "uia-1.2".into(),
                    text: "hello".into(),
                    clear: false,
                },
                800,
                600,
                frame
            )
            .is_ok()
        );
        for action in [
            GuiAction::ClickElement {
                element_id: "ax-0.3;rm".into(),
            },
            GuiAction::ClickElement {
                element_id: "element-1".into(),
            },
            GuiAction::TypeElement {
                element_id: "ax-0.3".into(),
                text: "a\nb".into(),
                clear: true,
            },
            GuiAction::TypeElement {
                element_id: "ax-0.3".into(),
                text: "x".repeat(257),
                clear: true,
            },
        ] {
            assert_eq!(
                to_window_action(&action, 800, 600, frame),
                Err(ComputerError::InvalidAction),
                "{action:?}"
            );
        }
    }
}
