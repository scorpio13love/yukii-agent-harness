# TypeScript 容器工具示例

这个示例演示：**工具用 TypeScript 写，不需要写 Rust**。

当前 Yukii 的第三方工具运行在 Docker 容器中：

```text
模型参数 JSON → 容器 stdin
容器 TypeScript → stdout 单个 JSON
ToolGateway → 模型
```

## 1. 构建镜像

需要 Node 22、TypeScript、Docker。

```bash
cd examples/plugins/ts-text-tool
npm install
npm run build
docker build -t ghcr.io/<your-account>/yukii-ts-text-tool:0.1.0 .
docker push ghcr.io/<your-account>/yukii-ts-text-tool:0.1.0
docker pull ghcr.io/<your-account>/yukii-ts-text-tool@sha256:<digest>
```

获取 digest：

```bash
docker inspect --format='{{index .RepoDigests 0}}' \
  ghcr.io/<your-account>/yukii-ts-text-tool:0.1.0
```

把 `plugin.json` 的 `image` 替换为带真实 digest 的镜像引用。

## 2. 测试容器协议

```bash
echo '{"text":"hello 世界\nsecond line","mode":"all"}' \
  | docker run --rm -i --network=none --read-only \
      ghcr.io/<your-account>/yukii-ts-text-tool@sha256:<digest>
```

应输出一个 JSON：

```json
{ "characters": 18, "characters_without_spaces": 15, "words": 3, "lines": 2, "mode": "all" }
```

## 3. 发布到市场

把 `plugin.json` 复制为本地市场的 package 文件，例如：

```text
examples/marketplace/packages/ts_text_tool-1.0.0.json
```

在 `examples/marketplace/index.json` 增加条目：

```json
{
  "id": "ts_text_tool",
  "version": "1.0.0",
  "name": "TypeScript 文本统计",
  "description": "示例：用 TypeScript 写的容器工具。",
  "package": "packages/ts_text_tool-1.0.0.json",
  "sha256": "<plugin.json 的 SHA-256>"
}
```

计算：

```bash
shasum -a 256 examples/marketplace/packages/ts_text_tool-1.0.0.json
```

## 4. 安装、审核、启用

```bash
# 安装（保持禁用）
curl -X POST http://127.0.0.1:8765/api/plugins/install \
  -H 'Content-Type: application/json' \
  -d '{
    "id":"ts_text_tool",
    "version":"1.0.0",
    "sha256":"<plugin.json sha256>",
    "approved_permissions":["sandbox_compute"],
    "expected_previous_sha256":null
  }'

# 启用
curl -X POST http://127.0.0.1:8765/api/plugins/ts_text_tool/enabled \
  -H 'Content-Type: application/json' \
  -d '{
    "approved_permissions":["sandbox_compute"],
    "enabled":true,
    "sha256":"<plugin.json sha256>"
  }'
```

启用后，Agent 可见工具：

```text
ext__ts_text_tool__text_stats
```

## 5. 当前限制

- 只能用 API/office 任务；Desktop/Browser host 尚未加载插件工具；
- Docker 必须可用；
- 镜像必须预先准备（`--pull=never`）；
- 容器无网络、无宿主文件、只读、非 root；
- 输入/输出各 64KB；
- 一次性调用，不支持流式/状态；
- 没有第三方签名；
- 需要用户批准 `sandbox_compute` 后启用。
