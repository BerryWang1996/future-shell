//! Agent 运行时的 app 装配层（实现规格 §2「app 层」）。
//!
//! `crates/ai/src/agent/` 是纯逻辑（零 fake 全测）；这里是它的四个端口的真实现：
//! russh 通道、UI 确认队列、审计落库、模型 HTTP。装配层不做任何判断——
//! 判断全在纯逻辑那边，这里只把「真的东西」接上去。

pub mod agent_audit;
pub mod confirm_port;
pub mod exec_port;
pub mod model_port;
