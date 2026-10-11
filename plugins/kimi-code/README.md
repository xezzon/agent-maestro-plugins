# kimi-code

[Maestro](https://github.com/xezzon/agent-maestro) 的 Kimi Code 插件：把 Maestro 中录入的 Provider 投影进 `~/.kimi-code/config.toml`。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

每个 Provider 写入一条 `[providers."maestro:<slug>"]`，其每个 Model 写入一条 `[models."maestro:<slug>/<model-id>"]`。Provider 可同时携带多个协议端点，插件按 [ADR 0016](https://github.com/xezzon/agent-maestro/blob/main/docs/adr/0016-plugin-side-endpoint-selection.md) 选定唯一端点（见下）：

| Maestro | Kimi Code `config.toml` |
| --- | --- |
| Provider slug `foo` | provider 名 `maestro:foo`，模型别名的 `provider` 字段 |
| 所选端点协议 `openai-completions` | `type = "openai"` |
| 所选端点协议 `anthropic-messages` | `type = "anthropic"` |
| 所选端点 Base URL | `base_url` |
| API Key | `api_key`（无凭证时省略，可自行补填或用 `[providers.*.env]` 兜底） |
| Provider `custom_header` | `[providers."maestro:<slug>".custom_headers]`（名→值，空则省略整段） |
| Model ID | `model` 字段，别名为 `maestro:<slug>/<model-id>` |
| Model `limit.context_window` | `max_context_size`（未设置时回落 131072，见下） |
| Model `limit.max_input` | `max_input_size`（未设置则省略） |
| Model `limit.max_output` | `max_output_size`（未设置则省略；Kimi Code 目前仅 anthropic provider 读取） |
| Model `capabilities` | `capabilities = [...]`：`tool_use` / `image_in` / `thinking`（未设置则省略） |
| Model 显示名 | `display_name`（空则省略） |

## 设计要点

- **端点选择在插件侧**。Provider 可携带多个协议端点，Kimi Code 的 provider `type` 只由其中一个决定，因此本插件按 [ADR 0016](https://github.com/xezzon/agent-maestro/blob/main/docs/adr/0016-plugin-side-endpoint-selection.md) 选定唯一端点：优先界面所选协议对应的端点；所选协议缺失时，若该 Provider 只有唯一端点则用它，否则多端点优先 `openai-completions`。
- **只重建 `maestro:` 命名空间**。`config.toml` 是 Kimi Code 与用户共享的配置文件（`default_model`、`thinking`、`permission` 等都在里面），因此本插件不像 pi 插件那样整文件重写，而是以 [`toml_edit`](https://crates.io/crates/toml_edit) 无损编辑：先删除上一轮投影的 `maestro:` 条目再写入当前列表，注释、格式与其余条目原样保留。Maestro 中已删除的 Provider 不会残留。
- **`max_context_size` 取自 `limit.context_window`，缺省回落 131072**。该字段在 Kimi Code 中必填；Model 未声明上下文长度时写保守值 128k——声明偏小只会让 Kimi Code 提前压缩上下文，不会引发服务端报错；如与实际不符，可在 `[models."<别名>".overrides]` 中覆盖。
- **上限、能力与自定义 header 按声明投影**。Model 的 `limit.max_input` / `limit.max_output` 分别写 `max_input_size` / `max_output_size`，`capabilities` 的 `tool_use` / `image_in` / `thinking` 映射为同名标签，Provider 的 `custom_header` 写进 `custom_headers` 表（宿主已按名排序）。未设置项一律省略——不写空表、空数组或 0。
- **不写 `default_model`**。投影后用 `/model` 选择即可。
- **不支持 `KIMI_CODE_HOME` 重定向**。插件可写的目录由 manifest 的 `config_dir` 静态声明为 `$HOME/.kimi-code`。
- 写入采用 tmp + rename 原子替换；文件解析失败时放弃写入并报错，不破坏既有配置。
