/// 终端剪贴板通道（Task 20 Step 4 前置，S287）：与 vault_copy_to_clipboard 同通道 arboard IPC，
/// 替换 term.ts navigator.clipboard 占位。前端 clipboardWrite/clipboardRead 经此统一通道，
/// 实现期择一定稿并红测固化（注 43 carry-over 承诺）。
/// 写入剪贴板（终端复制路径：Ctrl+C/Ctrl+Shift+C、选择即复制、右键菜单；空串 no-op 已由前端 clipboardWrite 短路）
#[tauri::command]
pub async fn clipboard_write(text: String) -> Result<(), String> {
    if text.is_empty() {
        return Ok(()); // 防御性兜底：前端已有 S277 守卫，此处冗余保护
    }
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())?;
    Ok(())
}

/// 读取剪贴板（终端粘贴路径：Ctrl+V/Ctrl+Shift+V、右键粘贴、中键粘贴、菜单粘贴；失败回空串降级）
#[tauri::command]
pub async fn clipboard_read() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.get_text().map_err(|e| e.to_string())
}
