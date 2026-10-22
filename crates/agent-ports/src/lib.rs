//! 内层端口：仅声明协议，不依赖 HTTP、SQLite 或具体模型供应商。
pub mod model;
pub mod repository;

pub mod browser;
pub mod computer;
pub mod context;
pub mod embedding;
pub mod extensions;
pub mod knowledge;
pub mod permissions;
pub mod sandbox;
pub mod tools;
