use std::collections::BTreeSet;
use yukii_agent_ports::knowledge::IndexedChunk;

/// 按 Unicode 字符切片并保留重叠，不在 UTF-8 字节中间切断中文。
pub fn chunks(title: &str, content: &str) -> Vec<IndexedChunk> {
    let characters: Vec<char> = content.chars().collect();
    let mut result = Vec::new();
    let mut start = 0;
    while start < characters.len() {
        let end = (start + 800).min(characters.len());
        let text: String = characters[start..end].iter().collect();
        let terms = tokenize(&format!("{title} {text}")).join(" ");
        result.push(IndexedChunk { text, terms });
        if end == characters.len() {
            break;
        }
        start = end - 120;
    }
    result
}

/// 中文用连续双字词，英文用单词；所有 FTS 查询项都由这里产生，拒绝查询语法注入。
pub fn query_expression(query: &str) -> String {
    tokenize(query)
        .into_iter()
        .filter(|term| !term.is_empty())
        .take(64)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut cjk = Vec::new();
    for character in text.chars().chain(std::iter::once(' ')) {
        if matches!(character,'\u{2e80}'..='\u{9fff}'|'\u{ac00}'..='\u{d7af}') {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            cjk.push(character);
        } else {
            if cjk.len() == 1 {
                tokens.push(cjk[0].to_string());
            } else {
                for pair in cjk.windows(2) {
                    tokens.push(pair.iter().collect());
                }
            }
            cjk.clear();
            if character.is_alphanumeric() {
                word.extend(character.to_lowercase());
            } else if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
        }
    }
    tokens
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_chunking_overlaps_and_queries_cannot_inject_fts() {
        let input = "报".repeat(1000);
        let parts = chunks("中文", &input);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].text.chars().count(), 800);
        assert_eq!(parts[1].text.chars().count(), 320);
        let query = query_expression("报销标准\" OR NOT *");
        assert!(query.contains("\"报销\""));
        assert!(!query.contains('*'));
        assert!(query.contains("\"or\""));
    }
}
