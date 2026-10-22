use crate::manifest;
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf, time::Duration};
use yukii_contracts::extensions::{
    MarketplaceCatalog, MarketplaceEntry, PluginManifest, PluginPreview,
};

const MAX_PACKAGE: usize = 512 * 1024;
enum Source {
    Local(PathBuf),
    Remote(Url),
}
pub struct Marketplace {
    source: Source,
    client: reqwest::Client,
}
impl Marketplace {
    /// 市场地址只能由服务端配置指定；Agent 不能传入任意下载 URL。
    pub fn new(local: PathBuf, remote: Option<&str>) -> Result<Self, String> {
        let source = if let Some(remote) = remote {
            let url = Url::parse(remote).map_err(|_| "市场 URL 无效")?;
            let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
            if url.scheme() != "https" && !(url.scheme() == "http" && local)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("市场必须使用无认证信息的 HTTPS URL，本机测试可用 HTTP".into());
            }
            Source::Remote(url)
        } else {
            Source::Local(local)
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "市场客户端初始化失败")?;
        Ok(Self { source, client })
    }
    pub async fn catalog(&self) -> Result<MarketplaceCatalog, String> {
        let bytes = self.read(None).await?;
        let catalog: MarketplaceCatalog =
            serde_json::from_slice(&bytes).map_err(|_| "市场索引格式无效")?;
        if catalog.schema_version != 1 || catalog.plugins.len() > 200 {
            return Err("市场索引版本或数量不支持".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &catalog.plugins {
            if !manifest::identifier(&entry.id, 24)
                || !manifest::version(&entry.version)
                || !manifest::checksum(&entry.sha256)
                || !seen.insert((&entry.id, &entry.version))
                || !safe_package_path(&entry.package)
            {
                return Err("市场条目标识、路径或摘要无效".into());
            }
        }
        Ok(catalog)
    }
    pub async fn preview(&self, id: &str, version: &str) -> Result<PluginPreview, String> {
        let entry = self
            .catalog()
            .await?
            .plugins
            .into_iter()
            .find(|entry| entry.id == id && entry.version == version)
            .ok_or("市场中没有指定插件版本")?;
        self.fetch(&entry).await
    }
    async fn fetch(&self, entry: &MarketplaceEntry) -> Result<PluginPreview, String> {
        let bytes = self.read(Some(&entry.package)).await?;
        let checksum = format!("{:x}", Sha256::digest(&bytes));
        if checksum != entry.sha256 {
            return Err("插件包 SHA-256 不匹配，拒绝安装".into());
        }
        let manifest: PluginManifest =
            serde_json::from_slice(&bytes).map_err(|_| "插件清单格式无效")?;
        manifest::validate(&manifest)?;
        if manifest.id != entry.id || manifest.version != entry.version {
            return Err("包身份与市场索引不一致".into());
        }
        Ok(PluginPreview {
            manifest,
            sha256: checksum,
            source: match &self.source {
                Source::Local(_) => "bundled".into(),
                Source::Remote(url) => url.as_str().into(),
            },
        })
    }
    async fn read(&self, package: Option<&str>) -> Result<Vec<u8>, String> {
        if package.is_some_and(|path| !safe_package_path(path)) {
            return Err("非法插件包路径".into());
        }
        match &self.source {
            Source::Local(root) => {
                let root = root.clone();
                let package = package.unwrap_or("index.json").to_owned();
                tokio::task::spawn_blocking(move || {
                    let root = root.canonicalize().map_err(|_| "本地市场目录不存在")?;
                    let path = root
                        .join(package)
                        .canonicalize()
                        .map_err(|_| "市场文件不存在")?;
                    if !path.starts_with(root) {
                        return Err("市场路径越界".into());
                    }
                    let mut bytes = Vec::new();
                    std::fs::File::open(path)
                        .map_err(|_| "市场文件不可读")?
                        .take((MAX_PACKAGE + 1) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "市场文件读取失败")?;
                    if bytes.len() > MAX_PACKAGE {
                        return Err("市场文件超过大小限制".into());
                    }
                    Ok(bytes)
                })
                .await
                .map_err(|_| "市场读取线程异常".to_owned())?
            }
            Source::Remote(index) => {
                let url = match package {
                    Some(path) => index.join(path).map_err(|_| "市场包 URL 无效")?,
                    None => index.clone(),
                };
                if url.origin() != index.origin() {
                    return Err("插件包必须来自同一市场源".into());
                }
                let mut response = self
                    .client
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| "市场连接失败")?;
                if !response.status().is_success() {
                    return Err("市场返回失败状态".into());
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| "市场下载中断")? {
                    if bytes.len() + chunk.len() > MAX_PACKAGE {
                        return Err("市场文件超过大小限制".into());
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(bytes)
            }
        }
    }
}
fn safe_package_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() < 200
        && !path.starts_with('/')
        && path.ends_with(".json")
        && path
            .bytes()
            .all(|value| value.is_ascii_alphanumeric() || b"-_/.".contains(&value))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_rejects_traversal_and_unsafe_remote_sources() {
        for path in [
            "../secret.json",
            "/a.json",
            "a//b.json",
            "a/%2e%2e/b.json",
            "https://other/a.json",
        ] {
            assert!(!safe_package_path(path), "{path}");
        }
        assert!(safe_package_path("packages/office_writing-1.0.0.json"));
        for url in [
            "http://example.com/index.json",
            "https://user:secret@example.com/index.json",
            "https://example.com/index.json?key=secret",
        ] {
            assert!(Marketplace::new(PathBuf::new(), Some(url)).is_err());
        }
    }
}
