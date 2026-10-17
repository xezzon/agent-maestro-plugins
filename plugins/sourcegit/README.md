# sourcegit

[Maestro](https://github.com/xezzon/agent-maestro) 的 SourceGit 插件：把 Maestro 中录入的 Provider 投影进 `~/.config/SourceGit/preference.json`，使录入的 Provider / 模型直接出现在 SourceGit AI 页的服务下拉里，用于生成 commit message。

SourceGit 的 `OpenAIServices` 是**一模型一条**的服务条目（`Model` 是单个字符串，不是列表），所以一个 Provider 有 N 个模型就写入 N 条条目，名字为 `maestro-<slug>/<model-id>`。

构建、调试与发布见仓库根 [README](../../README.md)。

## 投影规则

写入 `preference.json` 顶层 `OpenAIServices` 数组：

| Maestro | SourceGit `preference.json` |
| --- | --- |
| Provider slug `foo` 与 Model `gpt-5` | 条目 `Name` = `maestro-foo/gpt-5` |
| 该 Provider 第一个 `openai-completions` 端点的 Base URL | `Server`（原样透传，不补 `/v1`） |
| API Key | `ApiKey`（无凭证时省略该键） |
| Provider `custom_header` | `ExtraHeaders`，每行 `Name: Value` |

其余键恒写固定值，与 SourceGit 自己保存的形状一致：`ReadApiKeyFromEnv` = `false`、`Model` = 模型 id、`AutoFetchAvailableModels` = `false`、`ReasoningEffortLevel` = `"unspecified"`、`AdditionalPrompt` = `""`。

键的顺序对齐 SourceGit `src/AI/Service.cs` 的属性声明，便于与用户手工维护的条目对照。

### 端点选择

SourceGit 硬编码官方 OpenAI SDK，没有 anthropic-messages 分支，故本插件只投影 `openai-completions` 端点：取该 Provider 的第一个此类端点，**忽略界面所选协议**（[ADR 0016](https://github.com/xezzon/agent-maestro/blob/main/docs/adr/0016-plugin-side-endpoint-selection.md) 把端点选择交给插件，本插件的选择规则由「SourceGit 只支持一种协议」这一事实决定）。只配了 `anthropic-messages` 端点的 Provider 整条跳过并记一条警告日志。

`Server` 原样透传端点 Base URL：端点语义归用户，插件自作主张补 `/v1` 会写坏 `/v1` 不在结尾的网关路径（`https://gw.example.com/openai/v1` 等）。

### 自定义 header 的过滤

`ExtraHeaders` 是多行文本，SourceGit 按行拆分后逐行加成 HTTP 头，故两类 header 不投影（各记一条警告日志）：

- **名字与 `Authorization` 撞名**（大小写不敏感；按 SourceGit 的规则先去掉名字两侧空白再比较，`" Authorization "` 同样挡下）：SourceGit 的非 Azure 路径恒发 `Authorization: Bearer <ApiKey>`，同名自定义头会与之冲突。API Key 已经由 `ApiKey` 承载，无需再投影一份。
- **名字或值含 `CR` / `LF`**：会被 SourceGit 按行拆开，一条头变两条，等于让用户配置伪造出一个新请求头。

警告日志只带 header 的**名字**，绝不带值——值可能是凭证。

### `AutoFetchAvailableModels` 恒为 `false`

为 `true` 时 SourceGit 启动会对每个服务发起 `GET {Server}/models` 网络探测：录入 N 个 Provider × M 个模型就是 N×M 次请求，指向的还是可能需要鉴权的网关。本插件已按模型粒度投影，无需再探测。

### 解析从严

`preference.json` 是 SourceGit 源生成的序列化结果，只认严格 JSON。任何 jsonc-parser 默认放行的宽松写法——注释、尾随逗号、单引号字符串、十六进制数、一元加号、裸小数点、非有限数——一律判为解析失败、放弃写入并报错。

原因有二：SourceGit 的反序列化会抛异常，而更糟的是**读取失败时它会静默重置为一份全新默认配置**，用户下次保存即整份覆盖既有设置。宁可什么都不写，也不写一份 SourceGit 读不回的 JSON。

（实现细节：`CstRootNode::parse` 按 `CommentCollectionStrategy::AsTokens` 收集注释，`ParseOptions::allow_comments` 对它无效，故注释由 `parse_document` 自行遍历 CST 挡掉。）

`OpenAIServices` 在顶层重复出现时同样放弃写入：CST 取第一个同名键，SourceGit 的 `System.Text.Json` 取最后一个，无法确定该改哪一个；继续投影会「成功」而 SourceGit 仍读旧数组。

## 设计要点

- **只重建 `maestro-` 命名空间**。`preference.json` 是 SourceGit 与用户共享的单文件全局设置（50 余个顶层键：主题、布局、外部合并工具、仓库列表、工作区，`OpenAIServices` 只是其中之一），本插件以 [`jsonc-parser`](https://crates.io/crates/jsonc-parser) 的 CST 无损编辑（JSONC 侧的 `toml_edit` 等价物）：先删除上一轮投影的 `maestro-` 条目再写入当前列表，其余顶层键与用户手写的服务条目**逐字保留**，包括缩进与缩进风格。Maestro 中已删除的 Provider 不会残留。
  - 代价：用户自己建的服务若恰好以 `maestro-` 开头（如 `maestro-internal`），会在重投影时被删掉。改个名字即可。
- **需要重启 SourceGit 才生效**。SourceGit 只在启动时读一次 `preference.json`，运行中不重载。投影成功后插件也把这条（连同下一条）以 info 档写进日志。
- **运行中的 SourceGit 会覆盖本次投影**。用户在 SourceGit 界面里改设置并保存时，它按内存里的旧配置整份写回，会抹掉投影结果——下次 Maestro 重投影即可恢复。
- **模型需支持 tool calling**。SourceGit 生成 commit message 时以工具调用方式请求模型；不支持 tool calling 的模型即使出现在下拉里也会失败。
- **只支持全局配置**。不支持 SourceGit 的 portable 模式与 Flatpak 沙箱内的配置目录（manifest 的 `config_dir` 为 `$XDG_CONFIG_HOME/SourceGit`，宿主据此预开放路径）。
- **旧版 SourceGit 会静默丢弃 `ExtraHeaders`**：`ExtraHeaders` 是 v2026.20 之后才有的字段，更早的版本反序列化时忽略它（自定义 header 失效，不报错）。其余字段的形状与 `Service.cs` 长期稳定。
- **Model 的 limit 与 capabilities 不投影**：SourceGit 的 `Service` 没有对应字段，`Model` 只是一个字符串。
- **不碰仓库级配置**：`preferredOpenAIService` 等按仓库记录的偏好不受影响，投影只写全局默认服务列表。
- **不支持 `anthropic-messages`**：SourceGit 无对应分支，见上文「端点选择」。
- **写入采用 tmp + rename 原子替换**；解析失败或结构不符合预期时放弃写入并报错，不破坏既有配置。

## 测试

测试只断言 `edit_config` 的外部行为（写出的配置文本 + 警告列表），不碰私有函数。`plugins/sourcegit/tests/fixtures/preference.json` 是本机真实 `preference.json` 的**脱敏副本**：保留全部顶层键、缩进风格与两条用户手写服务，API Key 与本机路径逐键替换为占位值。
