---
name: tui-loopback-ws-proxy-gotcha
description: 本机测试进程带 HTTP_PROXY，tungstenite 会把 ws://127.0.0.1 也走代理，需 NO_PROXY 绕过
metadata:
  node_type: memory
  type: project
  originSessionId: df69a396-fe1c-4729-94c3-754ce53b8dc4
  modified: 2026-11-04T01:15:31.866Z
---

本机 cargo 测试进程环境里存在 `HTTP_PROXY=HTTPS_PROXY=http://127.0.0.1:7897`（该端口常常无人监听），而 codex-rs 工作区启用了 tokio-tungstenite 的 `proxy` feature；它会把 `ws://127.0.0.1:*` / `localhost` 也路由到代理，且默认不绕过 loopback，导致 `Connection refused (os error 61)`。

**Why:** 2026-11-04 调试 tui `session_lifecycle_requests` 录制代理用例时定位：未改动的对照用例同样必挂，绑定端口复查 AddrInUse（监听仍在），设置 `NO_PROXY=127.0.0.1,localhost` 后通过。这是既有环境问题，曾误判为 auth 改写回归。

**How to apply:** 跑涉及 loopback WebSocket 的 tui/app-server 测试时加 `NO_PROXY=127.0.0.1,localhost`。生产上同类风险：用户开着 HTTP_PROXY（如 Clash）时 TUI 连本地 ws remote app-server 会失败，根因修复应放在 app-server-client 的 loopback 绕过。另：plain `cargo test` 跑整个 `chatwidget::tests::slash_commands` 会因共享 helper 内联快照触发 insta “assertions in loops”，项目用 nextest（`codex-rs/.config/nextest.toml`）时无此问题。

2026-11-04 补充：同样的环境代理还会让 reqwest 的 loopback 请求（models catalog、compaction、analytics 等）打到 7897，表现为 wiremock 0 requests / RequestTimeout / ConnectionFailed。跑 core lib 测试用 `NO_PROXY=127.0.0.1,localhost RUST_MIN_STACK=67108864 cargo test -p codex-core --lib` 后 2249 个用例里仅 `session::tests::user_shell_commands_do_not_inherit_managed_network_proxy` 仍失败——该用例断言子 shell 里没有 HTTP_PROXY，而 ambient HTTP_PROXY 本来就该被继承，干净 CI 下通过，属环境噪声。
