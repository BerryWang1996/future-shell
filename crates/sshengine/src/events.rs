use crate::hostkey::{Decision, PresentedKey};

#[derive(Debug, Clone)]
pub struct Prompt {
    pub text: String,
    pub echo: bool,
}

/// 主机密钥裁决的用户选择。`Default` 为 `Refuse`——失败闭合：
/// 任何忘记赋值 / 反序列化缺字段 / 前端超时未答的路径，都落到「拒绝」而不是「放行」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostKeyChoice {
    AcceptAndRecord,
    AcceptOnce,
    #[default]
    Refuse,
}

pub trait SessionEvents: Send + Sync {
    /// 主机密钥首次/变更裁决（阻塞至用户响应；hint 为 `Decision::Changed` 时携带 old_fingerprint，
    /// 供 UI 做新旧指纹对比，R15）。
    fn host_key_decision(
        &self,
        host: &str,
        port: u16,
        presented: &PresentedKey,
        hint: &Decision,
    ) -> HostKeyChoice;
    /// name/instruction 原样透传自 russh InfoRequest（字段 instructions → 载荷键 instruction；总设计 §2.1 v3，不得以 `..` 丢弃）
    fn kbd_interactive(&self, name: &str, instruction: &str, prompts: &[Prompt]) -> Vec<String>;
    /// 客户端**主动发起**的口令询问：档案没配口令、服务器又通告 `password` 时由连接层调用。
    /// 返回用户输入的口令；空串 = 用户取消——调用方必须按「无凭据」收尾，**不得**拿空口令去试。
    fn password_prompt(&self) -> String;

    /// 带缘由的口令询问（2026-08-31）：保险库锁着、档案绑的口令取不到时，
    /// 引擎用它把「为什么问」说清——固定那句「该连接未配置口令」在那个场景
    /// 是错的（配置了，只是锁着），用户会以为自己存的凭据丢了。
    ///
    /// 带默认实现：测试桩与本引擎自测的假实现不必逐个扩签名；真实实现
    /// （app 层 GuiEvents）覆盖它把 instruction 送进事件载荷。
    fn password_prompt_reason(&self, instruction: &str) -> String {
        let _ = instruction;
        self.password_prompt()
    }
    fn status(&self, msg: &str);
}
