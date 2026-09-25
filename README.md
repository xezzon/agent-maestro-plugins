# agent-maestro-plugins

[Maestro](https://github.com/xezzon/agent-maestro) 的第三方插件集合。本仓库是 Cargo 多插件工作区：根 `Cargo.toml` 以 `plugins/*` 通配纳管成员，`plugins/` 下每个子目录是一个自包含插件。

## 插件

| 插件 | 工具 | 说明 |
| --- | --- | --- |
| [`kimi-code`](plugins/kimi-code/README.md) | Kimi Code | 把 Maestro 中录入的 Provider 投影进 `~/.kimi-code/config.toml` |

## 仓库结构

```
plugins/kimi-code/
├── manifest.json        # 插件 manifest，entry 相对本目录解析
├── .cargo/config.toml   # 把构建产物固定到本目录的 target/
├── Cargo.toml           # crate maestro-plugin-kimi-code
├── README.md            # 插件说明（投影规则、设计要点）
└── src/lib.rs
```

SDK 统一锁定在根 `Cargo.toml` 的 `[workspace.dependencies]`（其余依赖由各插件自持），`Cargo.lock` 由工作区共享。

新增插件：复制 `plugins/kimi-code` 为 `plugins/<id>`，改写 crate 名、`manifest.json` 与 README 即可，工作区配置无需改动。

## 构建

工具链由根 `rust-toolchain.toml` 锁定（Rust `1.98.1` 与 `wasm32-wasip2` target），rustup 会自动切换到该工具链并补齐 target，无需手动 `rustup target add`。

**必须在插件目录内构建**：宿主按 manifest 所在目录解析相对 `entry` 且拒绝含 `..` 的路径，产物须落在插件目录之内。工作区默认共享根 `target/`，各插件用 `.cargo/config.toml` 把自己的 `target-dir` 固定在本目录；Cargo 只在从该目录（或其子目录）调用时读取该配置，因此在仓库根构建的产物不会落在正确位置。

```bash
cd plugins/kimi-code
cargo build --release --target wasm32-wasip2
```

产物为 WASM 组件 `plugins/kimi-code/target/wasm32-wasip2/release/maestro_plugin_kimi_code.wasm`。SDK 依赖锁定 agent-maestro 的发布 tag（当前 `v0.4.1`），随 tag 固化与宿主的兼容组合。

## 本地调试

按 [Maestro 插件作者指南](https://github.com/xezzon/agent-maestro/blob/main/docs/plugin-authoring.md)的 file 来源回路：

1. 构建 wasm（插件 manifest 的 `entry` 已指向 cargo 产物路径）。
2. 在 Maestro「添加插件」选「本地文件」，选择 `plugins/kimi-code/manifest.json`。
3. 改码后重新构建，在 Maestro 点「重新加载」。

## 发布

每个插件独立发布。打该插件的 tag（多插件共用本仓库时按插件区分，如 `kimi-code-v0.1.0`）→ 构建产物挂 GitHub Release（资产名固定 `plugin.wasm` 与 `manifest.json`，后者由 `jq` 把插件 `manifest.json` 的 `entry` 改写为本 Release 的 wasm 资产 URL 生成）→ 用户以 `…/releases/download/<tag>/manifest.json` 安装。详细步骤见插件作者指南。
