use codex_protocol::models::ContentItem;

/// Convert content items to text representation
pub fn content_items_to_text(content: &[ContentItem]) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    
    let mut text = String::new();
    for item in content {
        match item {
            ContentItem::InputText { text: t } | ContentItem::OutputText { text: t } => {
                text.push_str(t);
            }
            _ => {}
        }
    }
    
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Check if a message is a session prefix message
pub fn is_session_prefix_message(text: &str) -> bool {
    // Simple heuristic - check if it looks like a system/session prefix
    text.starts_with("System:") || text.starts_with("Session:") || text.trim().is_empty()
}