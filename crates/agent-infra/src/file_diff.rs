//! 行级 diff：用于“已编辑文件”卡片和右侧 diff 查看器。纯内存实现，不依赖 git。
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct DiffLine {
    pub kind: &'static str,
    pub text: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
}

/// 返回 diff 行、增加行数、删除行数；大文件中间段超过预算时退化为整段替换。
pub fn line_diff(old: &str, new: &str) -> (Vec<DiffLine>, usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_mid = &old_lines[prefix..old_lines.len() - suffix];
    let new_mid = &new_lines[prefix..new_lines.len() - suffix];
    let mut ops: Vec<(&'static str, &str, Option<usize>, Option<usize>)> = Vec::new();
    for (index, line) in old_lines[..prefix].iter().enumerate() {
        ops.push(("context", line, Some(index + 1), Some(index + 1)));
    }
    let mut old_no = prefix;
    let mut new_no = prefix;
    let budget = 1_000_000usize;
    if old_mid.len().saturating_mul(new_mid.len()) <= budget {
        let rows = old_mid.len() + 1;
        let cols = new_mid.len() + 1;
        let mut table = vec![0u32; rows * cols];
        for i in (0..old_mid.len()).rev() {
            for j in (0..new_mid.len()).rev() {
                table[i * cols + j] = if old_mid[i] == new_mid[j] {
                    table[(i + 1) * cols + (j + 1)] + 1
                } else {
                    table[(i + 1) * cols + j].max(table[i * cols + (j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0usize, 0usize);
        while i < old_mid.len() && j < new_mid.len() {
            if old_mid[i] == new_mid[j] {
                ops.push(("context", old_mid[i], Some(old_no + 1), Some(new_no + 1)));
                i += 1;
                j += 1;
                old_no += 1;
                new_no += 1;
            } else if table[(i + 1) * cols + j] >= table[i * cols + (j + 1)] {
                ops.push(("removed", old_mid[i], Some(old_no + 1), None));
                i += 1;
                old_no += 1;
            } else {
                ops.push(("added", new_mid[j], None, Some(new_no + 1)));
                j += 1;
                new_no += 1;
            }
        }
        while i < old_mid.len() {
            ops.push(("removed", old_mid[i], Some(old_no + 1), None));
            i += 1;
            old_no += 1;
        }
        while j < new_mid.len() {
            ops.push(("added", new_mid[j], None, Some(new_no + 1)));
            j += 1;
            new_no += 1;
        }
    } else {
        for line in old_mid {
            ops.push(("removed", line, Some(old_no + 1), None));
            old_no += 1;
        }
        for line in new_mid {
            ops.push(("added", line, None, Some(new_no + 1)));
            new_no += 1;
        }
    }
    for (offset, line) in old_lines[old_lines.len() - suffix..].iter().enumerate() {
        ops.push((
            "context",
            line,
            Some(old_lines.len() - suffix + offset + 1),
            Some(new_lines.len() - suffix + offset + 1),
        ));
    }
    let added = ops.iter().filter(|(kind, ..)| *kind == "added").count();
    let removed = ops.iter().filter(|(kind, ..)| *kind == "removed").count();
    let lines = ops
        .into_iter()
        .map(|(kind, text, old_line, new_line)| DiffLine {
            kind,
            text: text.to_owned(),
            old_line,
            new_line,
        })
        .collect();
    (lines, added, removed)
}

#[cfg(test)]
mod tests {
    use super::line_diff;

    #[test]
    fn counts_added_and_removed_lines() {
        let (lines, added, removed) = line_diff("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
        assert!(
            lines
                .iter()
                .any(|line| line.kind == "added" && line.text == "B")
        );
        assert!(
            lines
                .iter()
                .any(|line| line.kind == "removed" && line.text == "b")
        );
    }
}
