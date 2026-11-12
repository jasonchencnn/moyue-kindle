//! 引擎统一错误类型。
//!
//! 用 thiserror（MIT/Apache-2.0）而不是 anyhow：调用方需要能按错误种类
//! 分支处理，而 anyhow 的 `anyhow::Error` 会把类型信息抹掉。

use thiserror::Error;

/// 引擎相关错误的统一类型。
#[derive(Debug, Error)]
pub enum Error {
    /// 打开文档失败：路径不对、格式不支持、字体缺失等。
    #[error("打开文档失败：{0}")]
    Open(String),

    /// 排版参数不在允许范围内。
    #[error("排版参数无效：{0}")]
    InvalidLayout(String),

    /// 渲染页面失败。
    #[error("渲染页面失败：{0}")]
    Render(String),

    /// 页码越界。
    #[error("页码越界：{0}")]
    PageOutOfRange(String),

    /// XPointer 无效，或按它定位页码失败。
    #[error("XPointer 定位失败：{0}")]
    XPointer(String),

    /// 引擎内部错误：初始化、字体注册、状态不对等。
    #[error("引擎错误：{0}")]
    Engine(String),
}

/// 引擎相关操作的返回类型。
pub type Result<T> = std::result::Result<T, Error>;
