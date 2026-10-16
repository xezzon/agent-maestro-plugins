# opencode

[Maestro](https://github.com/xezzon/agent-maestro) 的 OpenCode 插件：把 Maestro 中录入的 Provider 投影进 `~/.config/opencode/opencode.json`，使录入的 Provider / 模型直接出现在 OpenCode 的 `/models` 选择器中。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

每个 Provider 写入一条 `providers["maestro-<slug>"]`：

| Maestro | OpenCode `opencode.json` |
| --- | --- |
| Provider slug `foo`，所选端点协议 `openai-completions` | provider id `maestro-foo`，`package: "aisdk:@ai-sdk/openai-compatible"` |
| Provider slug `foo`，所选端点协议 `anthropic-messages` | provider id `maestro-foo`，`package: "aisdk:@ai-sdk/anthropic"` |
| 所选端点 Base URL | `settings.baseURL` |
| API Key | `settings.apiKey`（无凭证时省略） |
| Provider `custom_header` | `headers`（名→值对象，空则省略） |
| Model ID | `models` 的键 |
| Model 显示名 | `models.<id>.name`（空则省略） |
| Model `capabilities.tool_use` | `models.<id>.capabilities.tools = true` |
| Model `capabilities.image_in` | `models.<id>.capabilities.input = ["text","image"]` |
| Model `limit.context_window` | `models.<id>.limit.context` |
| Model `limit.max_input` | `models.<id>.limit.input` |
| Model `limit.max_output` | `models.<id>.limit.output` |

新建 `opencode.json` 时会写入 `"$schema": "https://opencode.ai/config.json"`（同一地址也声明在 `manifest.json` 的 `upstream.schema`）；已存在的文件不做改动。

### 命名规则

provider id 一律为 `maestro-<slug>`，协议只决定挂载的 AI SDK 包：

- `openai-completions` → `package: "aisdk:@ai-sdk/openai-compatible"`（`/v1/chat/completions`）
- `anthropic-messages` → `package: "aisdk:@ai-sdk/anthropic"`

OpenCode 2 用 `aisdk:` 前缀标记 AI SDK 包：其 v1→v2 迁移即把 v1 的 `npm` 写成 `aisdk:<npm>`（`package` 也接受原生的 `@opencode/ai/providers/*` 形式）。

OpenCode 一个 provider 只能挂一个 npm 包，而 Maestro 的 Provider 可同时携带两种协议端点，因此插件按 [ADR 0016](https://github.com/xezzon/agent-maestro/blob/main/docs/adr/0016-plugin-side-endpoint-selection.md) 为每个 Provider 选定唯一端点：优先界面所选协议对应的端点；所选协议缺失时，若该 Provider 只有唯一端点则用它，否则多端点优先 `openai-completions`。slug 在 Maestro 中全局唯一，每个 Provider 只投影一个 provider id，`package` 只挂所选端点协议对应的包。

`maestro-` 前缀同时充当本插件条目的命名空间：OpenCode 的 provider id 与 models.dev 内置目录共享（`anthropic`、`openai` 等是内置 id），裸 slug 可能撞名。

### Anthropic 网关端点填法

Base URL 应填到协议根（如 `https://gateway.example.com` 或含 `/v1` 的地址）。`@ai-sdk/anthropic` 对自定义 `baseURL` 的请求路径拼接规则（是否自动补 `/v1/messages`）官方文档未写明，使用 Anthropic Messages 协议网关时请先真机验证一轮对话，必要时调整 Base URL 的路径部分。

## 设计要点

- **写 OpenCode 2 的 `providers`**。v1 的顶层 `provider` 在 OpenCode 2 中已废弃，插件只写非废弃字段。由于 OpenCode 会把 v1 `provider` 迁移合并进 `providers`，插件还会清理早期版本遗留在 `provider` 里的 `maestro-` 条目（整表被清空时一并移除该字段），以免迁移后重复。
- **只重建 `maestro-` 命名空间**。`opencode.json` 是 OpenCode 与用户共享的配置文件（`permission`、`mcp`、`agents` 等都在里面），本插件以 [`jsonc-parser`](https://crates.io/crates/jsonc-parser) 的 CST 无损编辑（JSONC 侧的 `toml_edit` 等价物）：先删除上一轮投影的 `maestro-` 条目再写入当前列表，注释、格式、尾逗号与其余条目原样保留。Maestro 中已删除的 Provider 不会残留，用户手写的条目不受影响。
- **API Key 写明文 `settings.apiKey`**。OpenCode 另有两类凭证承载方式：`{env:VAR}` / `{file:path}` 插值（可写入本插件的字段值），以及 `/connect` 写入的 `~/.local/share/opencode/auth.json`（不在配置目录内，插件够不到）。本期写明文，与 Maestro / kimi-code 的明文存储一致；介意明文可把 `settings.apiKey` 手工改为插值形式，投影不会触碰非 `maestro-` 前缀条目。
- **不写 `model` / `small_model`**。投影后用 `/models` 选择即可。
- **上限与能力按声明投影**。`limit.context_window` / `max_input` / `max_output` 有正值就各自写 `models.<id>.limit.context` / `input` / `output`（OpenCode 2 的 `limit` 三字段均可选，不再要求 `context` 与 `output` 同时存在）。能力映射：`tool_use` → `capabilities.tools = true`，`image_in` → `capabilities.input = ["text","image"]`（对齐 models.dev 的模型对象形状）。Provider 的 `custom_header` 写 provider 级 `headers`。未设置项一律省略——不写 `false` / 空数组 / 空对象；省略时 OpenCode 对自定义模型套用兜底值（工具、文本+图像输入、`200k`/`32k` 上限），与 v1 配置迁移后的结果一致。
- **不再投影 `thinking`**。OpenCode 2 移除了模型级 reasoning 开关（v1 的模型字段 `attachment` / `reasoning` 等被标记为不支持），模型是否推理改由 provider 包与模型 id 判定；故 Maestro 的 `capabilities.thinking` 无对应字段。
- **不支持 Windows**。文档只写 `~/.config/opencode/opencode.json`（macOS 与 Linux 同路径），未给 Windows 分列路径，manifest 的 `config_dir` 声明为 `$HOME/.config/opencode`；待宿主支持按平台声明（[agent-maestro#80](https://github.com/xezzon/agent-maestro/issues/80)）后补。
- 写入采用 tmp + rename 原子替换；文件解析失败时放弃写入并报错，不破坏既有配置。
