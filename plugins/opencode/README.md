# opencode

[Maestro](https://github.com/xezzon/agent-maestro) 的 OpenCode 插件：把 Maestro 中录入的 Provider 投影进 `~/.config/opencode/opencode.json`，使录入的 Provider / 模型直接出现在 OpenCode 的 `/models` 选择器中。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

每个单协议 Provider 写入一条 `provider["maestro-<slug>"]`：

| Maestro | OpenCode `opencode.json` |
| --- | --- |
| Provider slug `foo`，协议 `openai-completions` | provider id `maestro-foo`，`npm: "@ai-sdk/openai-compatible"` |
| Provider slug `foo`，协议 `anthropic-messages` | provider id `maestro-foo-anthropic`，`npm: "@ai-sdk/anthropic"` |
| Base URL | `options.baseURL` |
| API Key | `options.apiKey`（无凭证时省略） |
| Model ID | `models` 的键 |
| Model 显示名 | `models.<id>.name`（空则省略） |

新建 `opencode.json` 时会写入 `"$schema": "https://opencode.ai/config.json"`；已存在的文件不做改动。

### 命名规则

OpenCode 的自定义 provider 挂在一个 AI SDK npm 包上，一种协议一个 provider；Maestro Provider 同时配置两种协议端点时，宿主按单协议分别下发，插件写成两个条目：

- `openai-completions` → `maestro-<slug>`（挂 `@ai-sdk/openai-compatible`，`/v1/chat/completions`）
- `anthropic-messages` → `maestro-<slug>-anthropic`（挂 `@ai-sdk/anthropic`）

`maestro-` 前缀同时充当本插件条目的命名空间：OpenCode 的 provider id 与 models.dev 内置目录共享（`anthropic`、`openai` 等是内置 id），裸 slug 可能撞名。

### Anthropic 网关端点填法

Base URL 应填到协议根（如 `https://gateway.example.com` 或含 `/v1` 的地址）。`@ai-sdk/anthropic` 对自定义 `baseURL` 的请求路径拼接规则（是否自动补 `/v1/messages`）官方文档未写明，使用 Anthropic Messages 协议网关时请先真机验证一轮对话，必要时调整 Base URL 的路径部分。

## 设计要点

- **只重建 `maestro-` 命名空间**。`opencode.json` 是 OpenCode 与用户共享的配置文件（`permission`、`mcp`、`agent` 等都在里面），本插件以 [`jsonc-parser`](https://crates.io/crates/jsonc-parser) 的 CST 无损编辑（JSONC 侧的 `toml_edit` 等价物）：先删除上一轮投影的 `maestro-` 条目再写入当前列表，注释、格式、尾逗号与其余条目原样保留。Maestro 中已删除的 Provider 不会残留，用户手写的条目不受影响。
- **API Key 写明文 `options.apiKey`**。OpenCode 另有两类凭证承载方式：`{env:VAR}` / `{file:path}` 插值（可写入本插件的字段值），以及 `/connect` 写入的 `~/.local/share/opencode/auth.json`（不在配置目录内，插件够不到）。本期写明文，与 Maestro / kimi-code 的明文存储一致；介意明文可把 `options.apiKey` 手工改为插值形式，投影不会触碰非 `maestro-` 前缀条目。
- **不写 `model` / `small_model`**。投影后用 `/models` 选择即可。
- **模型上下文限额暂不投影**。`models.<id>.limit.context` / `limit.output` 需要 Model 的 `context_window` / `max_output` 字段（[agent-maestro#58](https://github.com/xezzon/agent-maestro/issues/58)），当前 `maestro:plugin` 合同尚未携带，待 SDK 暴露后补；provider 展示名（`name` 字段）同理。
- **不支持 Windows**。文档只写 `~/.config/opencode/opencode.json`（macOS 与 Linux 同路径），未给 Windows 分列路径，manifest 的 `config_dir` 声明为 `$HOME/.config/opencode`；待宿主支持按平台声明（[agent-maestro#80](https://github.com/xezzon/agent-maestro/issues/80)）后补。
- 写入采用 tmp + rename 原子替换；文件解析失败时放弃写入并报错，不破坏既有配置。
