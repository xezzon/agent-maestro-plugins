//! SourceGit 插件：把 Maestro 录入的 Provider 投影为 SourceGit 全局设置
//! `preference.json` 中 `OpenAIServices` 数组的条目，每个 Model 一条，
//! 使录入的 Provider / 模型直接出现在 SourceGit 的 AI 页服务下拉里，用于生成 commit message。
//!
//! `preference.json` 是 SourceGit 的全局设置单文件（50 余个顶层键：主题、布局、
//! 外部合并工具、仓库列表、工作区，以及 `OpenAIServices`），与 kimi-code / opencode
//! 插件同样的理由：它与用户共享，本插件只重建 `maestro-` 命名空间下的条目，
//! 以 [`jsonc_parser`] 的 CST 无损编辑，其余顶层键与用户手写的服务条目逐字保留。
//!
//! SourceGit 硬编码官方 OpenAI SDK，没有 anthropic-messages 分支，故本插件只投影
//! `openai-completions` 端点：取该 Provider 的第一个此类端点，忽略界面所选协议；
//! 只配了 `anthropic-messages` 的 Provider 跳过并记日志（ADR 0016 把端点选择交给插件，
//! 本插件的选择规则由「SourceGit 只支持一种协议」这一事实决定）。
//!
//! 依赖 [`maestro-plugin_sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$XDG_CONFIG_HOME/SourceGit`）
//! 预开放为 "/"，插件以相对路径读写。

use std::fs;
use std::path::Path;

use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstArray, CstInputValue, CstNode, CstObject, CstRootNode};
use maestro_plugin_sdk::{
    CustomHeader, Endpoint, Guest, Level, Model, Protocol, Provider, export, log,
};

/// 插件写入的唯一文件（相对 config_dir）。
const CONFIG_FILE: &str = "preference.json";
/// 插件条目的命名空间前缀：服务 `Name` 以此为开头，投影时据此识别并清理上一轮
/// 条目，避免误伤用户手工配置的服务。
const NAMESPACE: &str = "maestro-";

const KEY_SERVICES: &str = "OpenAIServices";
const KEY_NAME: &str = "Name";
const KEY_SERVER: &str = "Server";
const KEY_API_KEY: &str = "ApiKey";
const KEY_READ_API_KEY_FROM_ENV: &str = "ReadApiKeyFromEnv";
const KEY_MODEL: &str = "Model";
const KEY_AUTO_FETCH: &str = "AutoFetchAvailableModels";
const KEY_REASONING_EFFORT: &str = "ReasoningEffortLevel";
const KEY_ADDITIONAL_PROMPT: &str = "AdditionalPrompt";
const KEY_EXTRA_HEADERS: &str = "ExtraHeaders";

/// SourceGit 硬编码官方 OpenAI SDK，非 Azure 路径恒由它发
/// `Authorization: Bearer <ApiKey>`：同名自定义 header 会与之冲突，不投影。
const AUTH_HEADER: &str = "Authorization";
/// `ReasoningEffortLevel` 写 SourceGit 的枚举默认值（取值域
/// `unspecified` / `none` / `minimal` / `low` / `medium` / `high`）。
const REASONING_EFFORT_DEFAULT: &str = "unspecified";

/// 解析选项：SourceGit 用源生成序列化上下文，`ReadCommentHandling` 与
/// `AllowTrailingCommas` 取默认值——注释与尾随逗号会让它的反序列化抛异常，
/// 而读取失败时它会静默重置为一份全新默认配置（下次保存即整份覆盖用户设置）。
/// 故解析从严：任何 jsonc-parser 默认放行的宽松语法一律拒绝，绝不写出 SourceGit
/// 读不回的内容。
///
/// `allow_comments` 对 `CstRootNode::parse` 无效（它把注释收成 CST 节点而不是报错，
/// 见 [`parse_document`] 里自己查一遍），仍置 `false` 以表意。
const PARSE_OPTIONS: ParseOptions = ParseOptions {
    allow_comments: false,
    allow_trailing_commas: false,
    allow_loose_object_property_names: false,
    allow_missing_commas: false,
    allow_single_quoted_strings: false,
    allow_hexadecimal_numbers: false,
    allow_unary_plus_numbers: false,
    allow_bare_decimal_point_numbers: false,
    allow_non_finite_numbers: false,
    allow_extended_string_escapes: false,
};

struct SourceGitPlugin;

impl Guest for SourceGitPlugin {
    fn write_providers(providers: Vec<Provider>) -> Result<Vec<String>, String> {
        write_config(&providers)?;
        Ok(vec![CONFIG_FILE.to_owned()])
    }
}

/// 一次投影的结果：写回 `preference.json` 的文本，外加本次跳过的条目对应的日志消息。
///
/// 投影逻辑保持为纯函数、日志统一在 `write_config` 发出：`log` 是宿主 import，
/// 在宿主的测试 target 上不可调用，让投影在记录警告时才碰它等于无法测试。
struct Projection {
    text: String,
    warnings: Vec<String>,
}

/// 读取 `preference.json`（不存在则新建）并写入投影结果。
fn write_config(providers: &[Provider]) -> Result<(), String> {
    let path = Path::new(CONFIG_FILE);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取 {CONFIG_FILE} 失败\n原因：{e}")),
    };

    let projection = edit_config(&text, providers)?;

    // 原子写入：先写临时文件成功后再 rename，I/O 错误不会留下半截的 preference.json。
    let tmp = Path::new("preference.json.tmp");
    fs::write(tmp, &projection.text)
        .map_err(|e| format!("写入 {CONFIG_FILE} 临时文件失败\n原因：{e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("替换 {CONFIG_FILE} 失败\n原因：{e}"))?;

    // 配置已经就位后再补日志：日志是投影过程的外况，丢失不影响结果。
    for warning in &projection.warnings {
        log(Level::Warning, warning);
    }
    // 投影报告只有两条可用通道（返回的文件路径、logger），SourceGit 的生效前提与
    // 覆盖风险属于「非致命事件」，走 info 档外显。
    log(
        Level::Info,
        "restart SourceGit to load the projected services; \
         a running SourceGit overwrites the whole file when it saves its own settings",
    );

    Ok(())
}

/// 把投影应用到配置文本：解析 → 删除 `maestro-` 前缀的旧条目 → 写入当前列表。
/// 文件不存在或为空时以空文档为种子；解析失败或结构不符合预期时返回 Err，
/// 调用方保持原文件不动。
fn edit_config(text: &str, providers: &[Provider]) -> Result<Projection, String> {
    let root = match text.trim().is_empty() {
        true => new_document()?,
        false => parse_document(text)?,
    };
    let root_obj = root
        .object_value()
        .ok_or_else(|| "根节点不是对象，为避免破坏既有配置已放弃写入".to_owned())?;

    let services = services_array(&root_obj)?;
    drop_own_entries(&services);

    let mut warnings = Vec::new();
    for provider in providers {
        for entry in service_entries(provider, &mut warnings) {
            services.append(entry);
        }
    }

    Ok(Projection {
        text: root.to_string(),
        warnings,
    })
}

/// 解析既有配置文本，任何 SourceGit 读不回的写法都在这里挡掉。
///
/// 注释单独查一遍：`CstRootNode::parse` 按 `CommentCollectionStrategy::AsTokens`
/// 收集注释，`ParseOptions::allow_comments` 对它形同虚设，注释会一路留在 CST 里
/// 被原样写回。
fn parse_document(text: &str) -> Result<CstRootNode, String> {
    let root = CstRootNode::parse(text, &PARSE_OPTIONS)
        .map_err(|e| format!("解析 {CONFIG_FILE} 失败，为避免破坏既有配置已放弃写入\n原因：{e}"))?;

    let mut pending: Vec<CstNode> = root.children();
    while let Some(node) = pending.pop() {
        if node.is_comment() {
            return Err(format!(
                "{CONFIG_FILE} 含注释，SourceGit 读不回，为避免破坏既有配置已放弃写入"
            ));
        }
        pending.extend(node.children());
    }

    Ok(root)
}

/// 以空对象为种子，构造新配置文档。
fn new_document() -> Result<CstRootNode, String> {
    CstRootNode::parse("{}", &PARSE_OPTIONS)
        .map_err(|e| format!("初始化新 {CONFIG_FILE} 失败\n原因：{e}"))
}

/// 取出（或创建）`OpenAIServices` 数组；已存在但不是数组时放弃写入。
fn services_array(root_obj: &CstObject) -> Result<CstArray, String> {
    match root_obj.get(KEY_SERVICES) {
        Some(prop) => prop.value().and_then(|v| v.as_array()).ok_or_else(|| {
            format!("{KEY_SERVICES} 已存在但不是数组，为避免破坏既有配置已放弃写入")
        }),
        None => Ok(root_obj
            .append(KEY_SERVICES, CstInputValue::Array(Vec::new()))
            .array_value_or_set()),
    }
}

/// 删掉上一轮投影写入的条目，让数组只留用户手写的服务。
///
/// 判据是 `Name` 的 `maestro-` 前缀：用户手写的服务名通常不含它，借此可以
/// 重投影而不动用户自己的配置（代价见 README——用户自建 `maestro-…` 服务会被删）。
/// `Name` 缺失或不是字符串的元素不是本插件写的，留着。
fn drop_own_entries(services: &CstArray) {
    for element in services.elements() {
        if is_own_entry(&element) {
            element.remove();
        }
    }
}

fn is_own_entry(element: &CstNode) -> bool {
    element
        .as_object()
        .and_then(|entry| entry.get(KEY_NAME))
        .and_then(|prop| prop.value())
        .and_then(|value| value.as_string_lit())
        .and_then(|name| name.decoded_value().ok())
        .is_some_and(|name| name.starts_with(NAMESPACE))
}

/// 单个 Provider 的全部服务条目（每个 Model 一条）；跳过的条目与被丢弃的自定义头
/// 各记一条警告进 `warnings`。
fn service_entries(provider: &Provider, warnings: &mut Vec<String>) -> Vec<CstInputValue> {
    let Some(endpoint) = effective_endpoint(provider) else {
        warnings.push(format!(
            "Provider \"{}\" has no openai-completions endpoint; skipping",
            provider.slug
        ));
        return Vec::new();
    };
    if provider.models.is_empty() {
        warnings.push(format!(
            "Provider \"{}\" has no model; nothing to project",
            provider.slug
        ));
        return Vec::new();
    }
    let extra_headers = extra_headers(provider, warnings);
    provider
        .models
        .iter()
        .map(|model| service_entry(provider, endpoint, model, &extra_headers))
        .collect()
}

/// SourceGit 硬编码官方 OpenAI SDK，没有 anthropic-messages 分支，故只投影
/// `openai-completions` 端点：取第一个此类端点，界面所选协议一律忽略。
fn effective_endpoint(provider: &Provider) -> Option<&Endpoint> {
    provider
        .endpoints
        .iter()
        .find(|endpoint| matches!(endpoint.protocol, Protocol::OpenaiCompletions))
}

/// 单个 Model 的服务条目。键与顺序对齐 SourceGit 的 `src/AI/Service.cs` 属性声明。
fn service_entry(
    provider: &Provider,
    endpoint: &Endpoint,
    model: &Model,
    extra_headers: &str,
) -> CstInputValue {
    let mut entry = vec![
        (
            KEY_NAME.to_owned(),
            CstInputValue::from(format!("{NAMESPACE}{}/{}", provider.slug, model.id)),
        ),
        // Server 原样透传：端点语义归用户，插件自作主张补 `/v1` 会写坏
        // `/v1` 不在结尾的网关路径。
        (
            KEY_SERVER.to_owned(),
            CstInputValue::from(endpoint.base_url.as_str()),
        ),
    ];
    // 无凭证时省略该键（本地网关场景）：宿主传 None，
    // SourceGit 对无 ApiKey 的服务照常可用。
    if let Some(api_key) = &provider.api_key {
        entry.push((
            KEY_API_KEY.to_owned(),
            CstInputValue::from(api_key.as_str()),
        ));
    }
    entry.push((KEY_READ_API_KEY_FROM_ENV.to_owned(), false.into()));
    entry.push((KEY_MODEL.to_owned(), CstInputValue::from(model.id.as_str())));
    // 不让 SourceGit 启动时对每个服务发起 `GET {Server}/models` 网络探测：
    // 模型已由 Maestro 侧枚举到模型粒度。
    entry.push((KEY_AUTO_FETCH.to_owned(), false.into()));
    entry.push((
        KEY_REASONING_EFFORT.to_owned(),
        CstInputValue::from(REASONING_EFFORT_DEFAULT),
    ));
    entry.push((KEY_ADDITIONAL_PROMPT.to_owned(), CstInputValue::from("")));
    entry.push((
        KEY_EXTRA_HEADERS.to_owned(),
        CstInputValue::from(extra_headers),
    ));
    CstInputValue::Object(entry)
}

/// Provider 的 `custom_header` 投影为 `ExtraHeaders` 的多行文本：每行 `Name: Value`。
/// 无自定义头（或全部被丢弃）时写空串——`ExtraHeaders` 恒写，形状与 SourceGit 自己保存的一致。
fn extra_headers(provider: &Provider, warnings: &mut Vec<String>) -> String {
    let mut lines = Vec::with_capacity(provider.custom_header.len());
    for header in &provider.custom_header {
        match unprojectable(header) {
            // 日志只带 header 的名字，绝不带它的值：WIT 合同明示值是宿主插值后的
            // 字面量、可能含凭证（`custom-header` 字段注释）。
            Some(reason) => warnings.push(format!(
                "Provider \"{}\": custom header \"{}\" not projected: {reason}",
                provider.slug, header.name
            )),
            None => lines.push(format!("{}: {}", header.name, header.value)),
        }
    }
    lines.join("\n")
}

/// 该自定义 header 不可投影的原因（`None` 即投影）。
///
/// 两类：`Authorization` 与 SourceGit 恒发的 `Authorization: Bearer <ApiKey>` 撞名
/// （HTTP 头名大小写不敏感，故大小写都要挡）；名字或值带 `CR`/`LF` 会被 SourceGit
/// 按行拆开——一条头变两条，等于让用户配置伪造出一个新请求头。
fn unprojectable(header: &CustomHeader) -> Option<&'static str> {
    if header.name.eq_ignore_ascii_case(AUTH_HEADER) {
        return Some("SourceGit already sends it from ApiKey");
    }
    if has_line_break(&header.name) || has_line_break(&header.value) {
        return Some("name or value contains CR or LF");
    }
    None
}

fn has_line_break(text: &str) -> bool {
    text.contains('\r') || text.contains('\n')
}

export!(SourceGitPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use maestro_plugin_sdk::ModelLimit;

    /// 本机真实 `preference.json` 的脱敏副本：含全部顶层键与两条用户手写的服务。
    const FIXTURE: &str = include_str!("../tests/fixtures/preference.json");

    fn provider(slug: &str, base_url: &str, api_key: Option<&str>, models: &[&str]) -> Provider {
        Provider {
            slug: slug.to_owned(),
            endpoints: vec![Endpoint {
                protocol: Protocol::OpenaiCompletions,
                base_url: base_url.to_owned(),
            }],
            selected_protocol: None,
            api_key: api_key.map(str::to_owned),
            custom_header: Vec::new(),
            models: models
                .iter()
                .map(|id| Model {
                    id: (*id).to_owned(),
                    display_name: None,
                    limit: ModelLimit {
                        context_window: None,
                        max_input: None,
                        max_output: None,
                    },
                    capabilities: Vec::new(),
                })
                .collect(),
        }
    }

    /// 多数用例共用的 Provider：slug `foo`、单个 `openai-completions` 端点、
    /// 单模型 `gpt-5`、带凭证。
    fn sample() -> Provider {
        provider("foo", "https://gw.example.com/v1", Some("sk-x"), &["gpt-5"])
    }

    fn provider_with_headers(slug: &str, headers: &[(&str, &str)]) -> Provider {
        let mut p = provider(slug, "https://gw.example.com/v1", Some("sk-x"), &["gpt-5"]);
        p.custom_header = headers
            .iter()
            .map(|(name, value)| CustomHeader {
                name: (*name).to_owned(),
                value: (*value).to_owned(),
            })
            .collect();
        p
    }

    /// 投影并取回写出的配置文本。
    fn project(text: &str, providers: &[Provider]) -> String {
        edit_config(text, providers).unwrap().text
    }

    /// 写出的 `OpenAIServices` 条目（解析写出的 JSON 文本，不看插件的内部结构）。
    fn services_of(text: &str) -> Vec<serde_json::Value> {
        let doc: serde_json::Value = serde_json::from_str(text).unwrap();
        doc[KEY_SERVICES]
            .as_array()
            .expect("投影结果应含 OpenAIServices 数组")
            .clone()
    }

    fn names_of(text: &str) -> Vec<String> {
        services_of(text)
            .iter()
            .map(|service| service[KEY_NAME].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn projects_one_service_per_model_with_fixed_shape() {
        let out = project(
            "{}",
            &[provider(
                "foo",
                "https://gw.example.com/v1",
                Some("sk-x"),
                &["gpt-5", "gpt-5-mini"],
            )],
        );
        let services = services_of(&out);
        assert_eq!(services.len(), 2);
        assert_eq!(
            services[0],
            serde_json::json!({
                "Name": "maestro-foo/gpt-5",
                "Server": "https://gw.example.com/v1",
                "ApiKey": "sk-x",
                "ReadApiKeyFromEnv": false,
                "Model": "gpt-5",
                "AutoFetchAvailableModels": false,
                "ReasoningEffortLevel": "unspecified",
                "AdditionalPrompt": "",
                "ExtraHeaders": ""
            })
        );
        assert_eq!(services[1]["Name"], "maestro-foo/gpt-5-mini");
        assert_eq!(services[1]["Model"], "gpt-5-mini");
    }

    /// 条目的键与顺序对齐 SourceGit `Service.cs` 的属性声明。顺序是外部契约的一部分
    /// （用户手工条目与插件条目在同一个数组里对照着看），`serde_json::Value` 的 Map 无序，
    /// 故直接对写出的文本断言键的先后。
    #[test]
    fn writes_service_keys_in_sourcegit_declaration_order() {
        let out = project("{}", &[sample()]);
        let array = &out[out.find(&format!("\"{KEY_SERVICES}\": [")).unwrap()..];
        let entry = &array[array.find('\n').unwrap()..];
        let entry = &entry[..entry.find("\n    }").unwrap()];
        let keys: Vec<&str> = entry
            .lines()
            .filter_map(|line| line.split_once(": "))
            .map(|(key, _)| key.trim().trim_matches('"'))
            .collect();
        assert_eq!(
            keys,
            [
                KEY_NAME,
                KEY_SERVER,
                KEY_API_KEY,
                KEY_READ_API_KEY_FROM_ENV,
                KEY_MODEL,
                KEY_AUTO_FETCH,
                KEY_REASONING_EFFORT,
                KEY_ADDITIONAL_PROMPT,
                KEY_EXTRA_HEADERS,
            ]
        );
    }

    /// 无凭证的 Provider（本地网关等）省掉 `ApiKey` 键：SourceGit 对没有
    /// `ApiKey` 的服务照常可用，写一个空串反而会让它带上 `Bearer `。
    #[test]
    fn omits_api_key_when_absent() {
        let out = project(
            "{}",
            &[provider(
                "foo",
                "http://127.0.0.1:8080/v1",
                None,
                &["local"],
            )],
        );
        let service = &services_of(&out)[0];
        assert!(service.get(KEY_API_KEY).is_none());
        assert_eq!(service[KEY_SERVER], "http://127.0.0.1:8080/v1");
    }

    #[test]
    fn projects_custom_headers_one_per_line() {
        let out = project(
            "{}",
            &[provider_with_headers(
                "foo",
                &[("X-Tenant", "acme"), ("X-Trace-Id", "on")],
            )],
        );
        assert_eq!(
            services_of(&out)[0][KEY_EXTRA_HEADERS],
            "X-Tenant: acme\nX-Trace-Id: on"
        );
    }

    /// SourceGit 非 Azure 路径恒由官方 OpenAI SDK 发 `Authorization: Bearer <ApiKey>`，
    /// 再投影一个同名头只会在请求里出现两个 Authorization。
    #[test]
    fn omits_authorization_header_whatever_its_case() {
        for name in ["Authorization", "authorization", "AUTHORIZATION"] {
            let out = project(
                "{}",
                &[provider_with_headers(
                    "foo",
                    &[(name, "Bearer other"), ("X-Ok", "1")],
                )],
            );
            assert_eq!(services_of(&out)[0][KEY_EXTRA_HEADERS], "X-Ok: 1");
        }
    }

    /// `ExtraHeaders` 是 SourceGit 按 `\r` / `\n` 拆行的纯文本：名字或值里带换行，
    /// 一条头会变成两条，甚至伪造出第二个请求头。
    #[test]
    fn omits_headers_carrying_line_breaks() {
        let projection = edit_config(
            "{}",
            &[provider_with_headers(
                "foo",
                &[
                    ("X-Split", "a\nX-Injected: yes"),
                    ("X-Wrapped\r\n", "a"),
                    ("X-Ok", "keep"),
                ],
            )],
        )
        .unwrap();
        assert_eq!(
            services_of(&projection.text)[0][KEY_EXTRA_HEADERS],
            "X-Ok: keep"
        );
        assert_eq!(projection.warnings.len(), 2);
    }

    /// 警告只带 header 的名字与原因，绝不带值——值可能含凭证。
    #[test]
    fn warnings_never_quote_a_header_value() {
        let projection = edit_config(
            "{}",
            &[provider_with_headers(
                "foo",
                &[("Authorization", "sk-super-secret")],
            )],
        )
        .unwrap();
        assert_eq!(projection.warnings.len(), 1);
        assert!(!projection.warnings[0].contains("sk-super-secret"));
        assert!(projection.warnings[0].contains("Authorization"));
    }

    #[test]
    fn seeds_a_new_document_when_the_file_is_missing_or_blank() {
        for text in ["", "  \n\t"] {
            let out = project(text, &[sample()]);
            assert_eq!(names_of(&out), ["maestro-foo/gpt-5"]);
        }
    }

    #[test]
    fn appends_services_key_when_missing() {
        let out = project("{\n  \"Locale\": \"zh-CN\"\n}", &[sample()]);
        assert_eq!(names_of(&out), ["maestro-foo/gpt-5"]);
        assert!(out.contains("\"Locale\": \"zh-CN\""));
    }

    /// 投影只动 `OpenAIServices`：把它之外的字节原样搬过去。fixture 里其余
    /// 50 余个顶层键全在数组之前或之后，前后缀逐字相等即证明无一被改写。
    #[test]
    fn keeps_every_byte_outside_openai_services() {
        let out = project(FIXTURE, &[sample()]);
        let (before, after) = outside_services(FIXTURE);
        assert!(
            out.starts_with(before),
            "{before}\n---\n{}",
            &out[..40.min(out.len())]
        );
        assert!(
            out.ends_with(after),
            "{after}\n---\n{}",
            &out[out.len().saturating_sub(40)..]
        );
        // SourceGit 自己保存的文件以 `}` 结尾、无尾随换行；沿用同一形状。
        assert!(!out.ends_with('\n'));
    }

    /// 用户手写的服务条目逐字保留、且仍排在插件条目之前：插件一个字节都不该动它们。
    #[test]
    fn keeps_handwritten_services_verbatim() {
        let out = project(FIXTURE, &[sample()]);
        let handwritten = handwritten_of(FIXTURE);
        assert!(out.contains(handwritten));
        assert!(
            out.find(handwritten).unwrap() < out.find("maestro-foo/").unwrap(),
            "手写条目应排在插件条目之前"
        );
    }

    #[test]
    fn reprojection_replaces_only_own_entries() {
        let first = project(
            FIXTURE,
            &[provider(
                "foo",
                "https://gw.example.com/v1",
                Some("sk-x"),
                &["gpt-5", "gpt-5-mini"],
            )],
        );
        assert_eq!(
            names_of(&first),
            [
                "redacted-handwritten-service-0",
                "redacted-handwritten-service-1",
                "maestro-foo/gpt-5",
                "maestro-foo/gpt-5-mini",
            ]
        );

        let second = project(
            &first,
            &[provider(
                "bar",
                "https://gw2.example.com/v1",
                None,
                &["gpt-4"],
            )],
        );
        assert_eq!(
            names_of(&second),
            [
                "redacted-handwritten-service-0",
                "redacted-handwritten-service-1",
                "maestro-bar/gpt-4",
            ]
        );
        // 重投影不带上一轮的模型：Maestro 侧删掉的模型不该留在 SourceGit 里。
        assert!(!second.contains("gpt-5-mini"));
    }

    #[test]
    fn empty_provider_list_clears_only_own_namespace() {
        let first = project(FIXTURE, &[sample()]);
        let cleared = project(&first, &[]);
        assert_eq!(
            names_of(&cleared),
            [
                "redacted-handwritten-service-0",
                "redacted-handwritten-service-1"
            ]
        );
    }

    /// SourceGit 硬编码官方 OpenAI SDK，没有 anthropic-messages 分支：界面选的协议
    /// 一律忽略，取该 Provider 的第一个 openai-completions 端点。
    #[test]
    fn ignores_selected_protocol_and_takes_first_openai_completions() {
        let mut p = provider("foo", "unused", Some("sk-x"), &["gpt-5"]);
        p.endpoints = vec![
            Endpoint {
                protocol: Protocol::AnthropicMessages,
                base_url: "https://anth.example.com".to_owned(),
            },
            Endpoint {
                protocol: Protocol::OpenaiCompletions,
                base_url: "https://first.example.com/v1".to_owned(),
            },
            Endpoint {
                protocol: Protocol::OpenaiCompletions,
                base_url: "https://second.example.com/v1".to_owned(),
            },
        ];
        p.selected_protocol = Some(Protocol::AnthropicMessages);
        let out = project("{}", &[p]);
        assert_eq!(
            services_of(&out)[0][KEY_SERVER],
            "https://first.example.com/v1"
        );
    }

    #[test]
    fn skips_provider_without_openai_completions() {
        let mut p = provider("foo", "unused", Some("sk-x"), &["gpt-5"]);
        p.endpoints = vec![Endpoint {
            protocol: Protocol::AnthropicMessages,
            base_url: "https://anth.example.com".to_owned(),
        }];
        let projection = edit_config("{}", &[p]).unwrap();
        assert!(services_of(&projection.text).is_empty());
        assert_eq!(projection.warnings.len(), 1);
        assert!(projection.warnings[0].contains("foo"));
    }

    #[test]
    fn projects_nothing_for_provider_without_models() {
        let projection = edit_config(
            "{}",
            &[provider(
                "foo",
                "https://gw.example.com/v1",
                Some("sk-x"),
                &[],
            )],
        )
        .unwrap();
        assert!(services_of(&projection.text).is_empty());
        assert_eq!(projection.warnings.len(), 1);
    }

    /// SourceGit 用源生成序列化上下文读 `preference.json`（`ReadCommentHandling` 与
    /// `AllowTrailingCommas` 取默认值）：注释、尾随逗号等 jsonc-parser 默认放行的
    /// 宽松语法它读不回，而读不回时它会静默重置为一份默认配置并在下次保存时整份覆盖
    /// 用户设置。故一律拒绝，宁可不投影。
    #[test]
    fn rejects_json_sourcegit_cannot_read_back() {
        for (case, text) in [
            ("坏 JSON", "{\"Locale\": "),
            ("非对象根", "[1, 2]"),
            ("OpenAIServices 非数组", "{\"OpenAIServices\": {}}"),
            ("行注释", "// c\n{\"a\": 1}"),
            ("块注释", "{\"a\": 1 /* c */}"),
            ("顶层尾随逗号", "{\"a\": 1,}"),
            ("数组内尾随逗号", "{\"OpenAIServices\": [],}"),
            ("单引号字符串", "{'a': 1}"),
            ("十六进制数", "{\"a\": 0x1F}"),
            ("一元加号", "{\"a\": +1}"),
            ("裸小数点", "{\"a\": .5}"),
            ("非有限数", "{\"a\": NaN}"),
        ] {
            assert!(edit_config(text, &[]).is_err(), "应拒绝：{case}：{text}");
        }
    }

    /// fixture 里 `OpenAIServices` 数组之前、之后的两段文本。
    fn outside_services(text: &str) -> (&str, &str) {
        let open = text.find("  \"OpenAIServices\": [").unwrap();
        let close = text[open..].find("\n  ],").unwrap() + open + "\n  ],".len();
        (&text[..open], &text[close..])
    }

    /// `OpenAIServices` 数组之内、用户手写条目的那段文本（不含首尾方括号）。
    fn handwritten_of(text: &str) -> &str {
        const OPEN: &str = "  \"OpenAIServices\": [\n";
        let start = text.find(OPEN).unwrap() + OPEN.len();
        let end = text[start..].find("\n  ]").unwrap() + start;
        &text[start..end]
    }
}
