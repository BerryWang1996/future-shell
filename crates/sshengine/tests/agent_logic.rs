use fs_sshengine::agent::describe_agent_unavailable;

#[test]
fn unavailable_message_names_all_three_transports() {
    // 无 cfg 门控：诊断文案在所有平台统一列出三种传输（Linux/macOS CI 同样通过）
    let msg = describe_agent_unavailable();
    assert!(msg.contains("SSH_AUTH_SOCK"), "{msg}");
    assert!(msg.contains("Pageant"), "{msg}");
    assert!(msg.contains("openssh-ssh-agent"), "{msg}");
}
