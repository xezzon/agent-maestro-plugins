//! OpenCode 插件：把 Maestro 录入的 Provider 投影为
//! `~/.config/opencode/opencode.json` 中的 `provider["maestro-<slug>"]` 条目，
//! 使录入的 Provider / 模型直接出现在 OpenCode 的 `/models` 选择器中。
//!
//! `opencode.json` 是 OpenCode 与用户共享的全局配置（`permission`、`mcp`、
//! `agent` 等都住在里面），且官方支持 JSONC（注释与尾逗号）。与 kimi-code 用
//! `toml_edit` 的做法对齐，本插件经 [`jsonc_parser`] 的 CST 无损编辑：只重建
//! `maestro-` 前缀的 provider 条目，注释、格式与其余内容原样保留。
//!
//! OpenCode 的自定义 provider 挂在一个 AI SDK npm 包上，一种协议一个 provider；
//! provider id 与 models.dev 内置目录共享命名空间，故加 `maestro-` 前缀防撞名，
//! 该前缀同时充当清理旧条目的匹配依据（对齐 kimi-code 的 `maestro:` 命名空间）。
//!
//! 依赖 [`maestro-plugin-sdk`]（`maestro:plugin` 合同的类型化绑定），实现其
//! `Guest` trait。宿主把 manifest 声明的 config_dir（即 `$HOME/.config/opencode`）
//! 预开放为 "/"，插件以相对路径读写。

use std::fs;
use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use maestro_plugin_sdk::{Guest, Protocol, Provider, export};

/// 插件写入的唯一文件（相对 config_dir）。
const CONFIG_FILE: &str = "opencode.json";
/// 新建文件时写入的 schema 声明；已存在的文件不做改动。
const SCHEMA_KEY: &str = "$schema";
const SCHEMA_URL: &str = "https://opencode.ai/config.json";
/// 插件条目的命名空间前缀：provider id 都以此为前缀，投影时据此识别并清理
/// 旧条目，避免误伤用户手工配置的条目（OpenCode 的 provider id 与 models.dev
/// 内置目录共享命名空间，`anthropic`、`openai` 等是内置 id）。
const NAMESPACE: &str = "maestro-";
/// anthropic-messages 协议条目的 id 后缀：OpenCode 一个 provider 只能挂一个
/// AI SDK npm 包，Maestro Provider 同时配置两种协议端点时需拆为两条（宿主按
/// 单协议下发），命名见 [`provider_id`]。
const ANTHROPIC_SUFFIX: &str = "-anthropic";
const KEY_PROVIDER: &str = "provider";
const KEY_NPM: &str = "npm";
const KEY_OPTIONS: &str = "options";
const KEY_BASE_URL: &str = "baseURL";
const KEY_API_KEY: &str = "apiKey";
const KEY_MODELS: &str = "models";
const KEY_NAME: &str = "name";
const NPM_OPENAI_COMPATIBLE: &str = "@ai-sdk/openai-compatible";
const NPM_ANTHROPIC: &str = "@ai-sdk/anthropic";

struct OpenCodePlugin;

impl Guest for OpenCodePlugin {
    fn write_providers(providers: Vec<Provider>) -> Result<Vec<String>, String> {
        write_config(&providers)?;
        Ok(vec![CONFIG_FILE.to_owned()])
    }
}

fn write_config(providers: &[Provider]) -> Result<(), String> {
    let path = Path::new(CONFIG_FILE);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取 {CONFIG_FILE} 失败\n原因：{e}")),
    };

    let new_text = edit_config(&text, providers)?;

    // 原子写入：先写临时文件成功后再 rename，I/O 错误不会留下半截的 opencode.json。
    let tmp = Path::new("opencode.json.tmp");
    fs::write(tmp, new_text).map_err(|e| format!("写入 {CONFIG_FILE} 临时文件失败\n原因：{e}"))?;
    fs::rename(tmp, path).map_err(|e| format!("替换 {CONFIG_FILE} 失败\n原因：{e}"))?;

    Ok(())
}

/// 把投影应用到配置文本：解析（容忍注释与尾逗号）→ 删除 `maestro-` 前缀旧条目
/// → 写入当前列表。文件不存在或为空时以新文档为种子（带官方 schema 声明）；
/// 解析失败或结构不符合预期时返回 Err，调用方保持原文件不动。
fn edit_config(text: &str, providers: &[Provider]) -> Result<String, String> {
    let root = match text.trim().is_empty() {
        true => new_document()?,
        false => CstRootNode::parse(text, &Default::default()).map_err(|e| {
            format!("解析 {CONFIG_FILE} 失败，为避免破坏既有配置已放弃写入\n原因：{e}")
        })?,
    };
    let root_obj = root
        .object_value()
        .ok_or_else(|| "根节点不是对象，为避免破坏既有配置已放弃写入".to_owned())?;

    let provider_obj = match root_obj.get(KEY_PROVIDER) {
        Some(prop) => prop
            .value()
            .and_then(|v| v.as_object())
            .ok_or_else(|| "provider 已存在但不是对象，为避免破坏既有配置已放弃写入".to_owned())?,
        None => root_obj
            .append(KEY_PROVIDER, CstInputValue::Object(vec![]))
            .object_value_or_set(),
    };

    remove_namespace_entries(&provider_obj);
    for provider in providers {
        let id = provider_id(&provider.protocol, &provider.slug);
        provider_obj.append(&id, provider_entry(provider));
    }

    Ok(root.to_string())
}

fn new_document() -> Result<CstRootNode, String> {
    let root = CstRootNode::parse("{}", &Default::default())
        .map_err(|e| format!("初始化新 {CONFIG_FILE} 失败\n原因：{e}"))?;
    root.object_value_or_set()
        .append(SCHEMA_KEY, SCHEMA_URL.into());
    Ok(root)
}

/// OpenCode 的 provider id：openai-completions 为 `maestro-<slug>`，
/// anthropic-messages 为 `maestro-<slug>-anthropic`。
fn provider_id(protocol: &Protocol, slug: &str) -> String {
    match protocol {
        Protocol::OpenaiCompletions => format!("{NAMESPACE}{slug}"),
        Protocol::AnthropicMessages => format!("{NAMESPACE}{slug}{ANTHROPIC_SUFFIX}"),
    }
}

/// 删除表内所有 `maestro-` 前缀条目（上一轮投影的残留）。
fn remove_namespace_entries(provider_obj: &CstObject) {
    for prop in provider_obj.properties() {
        if prop
            .decoded_name()
            .is_some_and(|name| name.starts_with(NAMESPACE))
        {
            prop.remove();
        }
    }
}

/// 协议透传为 OpenCode 挂载的 AI SDK npm 包。
fn provider_npm(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenaiCompletions => NPM_OPENAI_COMPATIBLE,
        Protocol::AnthropicMessages => NPM_ANTHROPIC,
    }
}

fn provider_entry(provider: &Provider) -> CstInputValue {
    let mut options = vec![(
        KEY_BASE_URL.to_owned(),
        CstInputValue::from(provider.base_url.as_str()),
    )];
    // 无凭证时宿主传 None，本地网关模型在 OpenCode 中仍可见；
    // 用户可自行补 apiKey 或用 {env:VAR} / {file:path} 插值兜底。
    if let Some(api_key) = &provider.api_key {
        options.push((
            KEY_API_KEY.to_owned(),
            CstInputValue::from(api_key.as_str()),
        ));
    }

    let mut models = Vec::new();
    for model in &provider.models {
        let mut entry = Vec::new();
        // 合同未携带 Model 展示名时省略 name 字段。
        if let Some(name) = &model.display_name
            && !name.is_empty()
        {
            entry.push((KEY_NAME.to_owned(), CstInputValue::from(name.as_str())));
        }
        models.push((model.id.clone(), CstInputValue::Object(entry)));
    }

    CstInputValue::Object(vec![
        (KEY_NPM.to_owned(), provider_npm(&provider.protocol).into()),
        (KEY_OPTIONS.to_owned(), CstInputValue::Object(options)),
        (KEY_MODELS.to_owned(), CstInputValue::Object(models)),
    ])
}

export!(OpenCodePlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use maestro_plugin_sdk::Model;

    /// 投影结果带注释（JSONC），用 jsonc-parser 解析成 serde_json::Value 再断言，
    /// 同时兼作输出可被 JSONC 解析器读回的往返校验。
    fn parse_jsonc(text: &str) -> serde_json::Value {
        jsonc_parser::parse_to_serde_value::<serde_json::Value>(text, &Default::default()).unwrap()
    }

    fn provider(
        slug: &str,
        protocol: Protocol,
        base_url: &str,
        api_key: Option<&str>,
        models: &[(&str, Option<&str>)],
    ) -> Provider {
        Provider {
            slug: slug.to_owned(),
            protocol,
            base_url: base_url.to_owned(),
            api_key: api_key.map(str::to_owned),
            models: models
                .iter()
                .map(|(id, name)| Model {
                    id: id.to_string(),
                    display_name: name.map(str::to_owned),
                })
                .collect(),
        }
    }

    fn openai(slug: &str, models: &[(&str, Option<&str>)]) -> Provider {
        provider(
            slug,
            Protocol::OpenaiCompletions,
            "https://gateway.example.com/v1",
            Some("sk-..."),
            models,
        )
    }

    #[test]
    fn creates_document_with_schema_when_file_missing() {
        let out = edit_config("", &[openai("foo", &[("gpt-5", Some("GPT-5"))])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc[SCHEMA_KEY], SCHEMA_URL);
        assert_eq!(doc["provider"]["maestro-foo"]["npm"], NPM_OPENAI_COMPATIBLE);
        assert_eq!(
            doc["provider"]["maestro-foo"]["options"]["baseURL"],
            "https://gateway.example.com/v1"
        );
        assert_eq!(
            doc["provider"]["maestro-foo"]["options"]["apiKey"],
            "sk-..."
        );
        assert_eq!(
            doc["provider"]["maestro-foo"]["models"]["gpt-5"]["name"],
            "GPT-5"
        );
    }

    #[test]
    fn preserves_user_config_and_comments() {
        let input = r#"{
  // model config
  "model": "some/model",
  "permission": { "edit": "allow" }, // allow edits
  "provider": {
    "my-own": {
      "npm": "@ai-sdk/openai-compatible", // hand-written entry
      "options": { "baseURL": "https://my.example.com/v1", },
    },
  },
}
"#;
        let out = edit_config(input, &[openai("foo", &[("gpt-5", None)])]).unwrap();
        // 投影本身是合法 JSON，且用户内容原样保留。
        let doc = parse_jsonc(&out);
        assert_eq!(doc["model"], "some/model");
        assert_eq!(doc["permission"]["edit"], "allow");
        assert_eq!(
            doc["provider"]["my-own"]["options"]["baseURL"],
            "https://my.example.com/v1"
        );
        assert!(out.contains("// model config"));
        assert!(out.contains("// hand-written entry"));
        assert!(out.contains("\"maestro-foo\""));
    }

    #[test]
    fn reprojection_replaces_entries_and_removes_stale_ones() {
        let input = r#"{
  "provider": {
    "maestro-old": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://old.example.com/v1" }, "models": {} },
    "my-own": { "npm": "@ai-sdk/openai-compatible", "options": { "baseURL": "https://my.example.com/v1" } }
  }
}"#;
        let out = edit_config(input, &[openai("foo", &[("gpt-5", None)])]).unwrap();
        let doc = parse_jsonc(&out);
        assert!(
            doc["provider"].get("maestro-old").is_none(),
            "旧条目应被清理"
        );
        assert!(doc["provider"].get("my-own").is_some(), "用户条目不应误伤");
        assert_eq!(
            doc["provider"]["maestro-foo"]["options"]["baseURL"],
            "https://gateway.example.com/v1"
        );
    }

    #[test]
    fn anthropic_protocol_gets_suffix_and_npm() {
        let p = provider(
            "foo",
            Protocol::AnthropicMessages,
            "https://gw.example.com",
            None,
            &[("claude-5", Some("Claude 5"))],
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(
            doc["provider"]["maestro-foo-anthropic"]["npm"],
            NPM_ANTHROPIC
        );
        assert_eq!(
            doc["provider"]["maestro-foo-anthropic"]["models"]["claude-5"]["name"],
            "Claude 5"
        );
        assert!(
            doc["provider"]["maestro-foo-anthropic"]["options"]
                .get("apiKey")
                .is_none()
        );
    }

    #[test]
    fn omits_optional_fields() {
        let p = provider(
            "foo",
            Protocol::OpenaiCompletions,
            "https://gw.example.com/v1",
            None,
            &[("m1", None), ("m2", Some(""))],
        );
        let out = edit_config("{}", &[p]).unwrap();
        let doc = parse_jsonc(&out);
        let entry = &doc["provider"]["maestro-foo"];
        assert!(
            entry["options"].get("apiKey").is_none(),
            "无凭证不写 apiKey"
        );
        assert!(
            entry["models"]["m1"].get("name").is_none(),
            "空显示名省略 name"
        );
        assert!(entry["models"]["m2"].get("name").is_none());
    }

    #[test]
    fn empty_provider_list_clears_namespace_only() {
        let input = r#"{
  // comment
  "provider": {
    "maestro-old": { "npm": "@ai-sdk/openai-compatible" },
    "my-own": { "npm": "@ai-sdk/openai-compatible" }
  }
}"#;
        let out = edit_config(input, &[]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc["provider"].as_object().unwrap().len(), 1);
        assert!(out.contains("// comment"));
    }

    #[test]
    fn appends_provider_key_when_missing() {
        let out =
            edit_config("{\n  \"model\": \"some/model\"\n}\n", &[openai("foo", &[])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc["model"], "some/model");
        assert_eq!(doc["provider"]["maestro-foo"]["npm"], NPM_OPENAI_COMPATIBLE);
        assert!(
            doc["provider"]["maestro-foo"]["models"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejects_broken_json() {
        let input = "{ \"provider\": { }\n"; // 缺右括号
        let err = edit_config(input, &[]).unwrap_err();
        assert!(err.contains("解析 opencode.json 失败"), "实际错误：{err}");
    }

    #[test]
    fn treats_empty_existing_file_as_new_document() {
        let out = edit_config("", &[openai("foo", &[])]).unwrap();
        let doc = parse_jsonc(&out);
        assert_eq!(doc[SCHEMA_KEY], SCHEMA_URL);
        assert_eq!(doc["provider"]["maestro-foo"]["npm"], NPM_OPENAI_COMPATIBLE);
    }

    #[test]
    fn rejects_non_object_root() {
        let err = edit_config("[]", &[]).unwrap_err();
        assert!(err.contains("根节点不是对象"), "实际错误：{err}");
    }

    #[test]
    fn rejects_non_object_provider() {
        let err = edit_config("{ \"provider\": 42 }", &[]).unwrap_err();
        assert!(err.contains("provider 已存在但不是对象"), "实际错误：{err}");
    }
}
