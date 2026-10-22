//! DeepSeek 原生 web_search provider：只接受结构化 web_search_tool_result，不抓取模型正文。
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::Arc};
use yukii_agent_ports::{
    model::ToolDefinition,
    repository::RepositoryError,
    tools::{ExecutionTarget, Tool, ToolCatalog, ToolContext, ToolEffect},
};
use yukii_model_gateway::config::Settings;

pub struct WebSearchCatalog {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
    model: String,
    max_uses: u32,
    max_results: usize,
}

#[derive(Deserialize)]
struct SearchInput {
    query: String,
}

impl WebSearchCatalog {
    /// 环境变量缺失时返回 None，能力不注册，避免把未配置能力暴露成可用。
    pub fn from_env() -> Result<Option<Self>, String> {
        let settings = Settings::load(Path::new(".env")).map_err(|error| error.to_string())?;
        let api_key = [
            settings.get("YUKII_WEB_SEARCH_API_KEY"),
            settings.get("YUKII_API_KEY"),
        ]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
        let Some(api_key) = api_key else {
            return Ok(None);
        };
        let base = if settings.get("YUKII_WEB_SEARCH_BASE_URL").is_empty() {
            "https://api.deepseek.com/anthropic/v1"
        } else {
            settings.get("YUKII_WEB_SEARCH_BASE_URL")
        };
        let model = if settings.get("YUKII_WEB_SEARCH_MODEL").is_empty() {
            "deepseek-v4-flash"
        } else {
            settings.get("YUKII_WEB_SEARCH_MODEL")
        };
        Ok(Some(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "无法初始化搜索客户端")?,
            endpoint: format!("{}/messages", base.trim_end_matches('/')),
            api_key,
            model: model.to_owned(),
            max_uses: settings
                .get("YUKII_WEB_SEARCH_MAX_USES")
                .parse()
                .unwrap_or(5),
            max_results: settings
                .get("YUKII_WEB_SEARCH_MAX_RESULTS")
                .parse()
                .unwrap_or(10),
        }))
    }

    async fn search(&self, query: &str) -> Result<Value, String> {
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": format!("Perform a web search for the query: {query}")}]
            }],
            "tools": [{
                "type": "web_search_20250305",
                "name": "web_search",
                "max_uses": self.max_uses
            }],
            "thinking": {"type": "disabled"}
        });
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|_| "搜索服务连接失败")?;
        if !response.status().is_success() {
            return Err(format!("搜索服务返回 HTTP {}", response.status().as_u16()));
        }
        let payload: Value = response.json().await.map_err(|_| "搜索响应协议不兼容")?;
        map_response(&payload, self.max_results)
    }
}

fn map_response(payload: &Value, max_results: usize) -> Result<Value, String> {
    let blocks = payload["content"]
        .as_array()
        .ok_or("搜索响应缺少 content")?;
    let result_blocks: Vec<_> = blocks
        .iter()
        .filter(|block| block["type"] == "web_search_tool_result")
        .collect();
    if result_blocks.is_empty() {
        return Err("搜索服务没有返回 web_search_tool_result".into());
    }
    let mut snippets = std::collections::BTreeMap::new();
    for block in blocks.iter().filter(|block| block["type"] == "text") {
        for citation in block["citations"].as_array().into_iter().flatten() {
            if let (Some(url), Some(text)) =
                (citation["url"].as_str(), citation["cited_text"].as_str())
            {
                snippets
                    .entry(url.to_owned())
                    .or_insert_with(|| text.to_owned());
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut sources = Vec::new();
    for block in result_blocks {
        for item in block["content"].as_array().into_iter().flatten() {
            if item["type"] != "web_search_result" {
                continue;
            }
            let Some(url) = item["url"].as_str().filter(|value| !value.is_empty()) else {
                continue;
            };
            if !seen.insert(url.to_owned()) {
                continue;
            }
            sources.push(json!({
                "url": url,
                "title": item["title"],
                "snippet": snippets.get(url),
                "published_at": item["page_age"]
            }));
        }
    }
    let truncated = sources.len() > max_results;
    sources.truncate(max_results);
    Ok(json!({"sources": sources, "truncated": truncated}))
}

struct WebSearchTool(Arc<WebSearchCatalog>);

#[async_trait]
impl ToolCatalog for WebSearchCatalog {
    async fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, RepositoryError> {
        Ok(vec![Arc::new(WebSearchTool(Arc::new(Self {
            client: self.client.clone(),
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            model: self.model.clone(),
            max_uses: self.max_uses,
            max_results: self.max_results,
        })))])
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".into(),
            description: "使用配置的 DeepSeek 原生搜索检索公开网页；返回结构化 sources（url/title/snippet），不抓取网页全文。".into(),
            schema: json!({
                "type":"object",
                "properties":{"query":{"type":"string","maxLength":1000}},
                "required":["query"],
                "additionalProperties":false
            }),
        }
    }

    fn execution_target(&self) -> ExecutionTarget {
        ExecutionTarget::CloudOffice
    }

    async fn execute(&self, _: &ToolContext, arguments: Value) -> Result<ToolEffect, String> {
        let input: SearchInput =
            serde_json::from_value(arguments).map_err(|_| "web_search 参数无效")?;
        if input.query.trim().is_empty() || input.query.chars().count() > 1000 {
            return Err("web_search 需要 1–1000 字 query".into());
        }
        Ok(ToolEffect::Result(self.0.search(input.query.trim()).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_deduped_sources_and_truncates() {
        let payload = json!({"content":[
            {"type":"text","citations":[{"url":"https://a.example","cited_text":"A snippet"}]},
            {"type":"web_search_tool_result","content":[
                {"type":"web_search_result","url":"https://a.example","title":"A","page_age":"2026-01-01"},
                {"type":"web_search_result","url":"https://b.example","title":"B"}
            ]}
        ]});
        let value = map_response(&payload, 1).unwrap();
        assert_eq!(value["sources"].as_array().unwrap().len(), 1);
        assert_eq!(value["sources"][0]["snippet"], "A snippet");
        assert_eq!(value["truncated"], true);
    }
}
