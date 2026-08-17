use serde_json::{Value, json};
use tiktoken_rs::o200k_base_singleton;

use crate::{
    event::{ApiUsage, ContextTokenUsage},
    model::ModelContext,
};

pub fn estimate_request(context: &ModelContext, usage: ApiUsage) -> ContextTokenUsage {
    let mut values = estimate_input(context);
    values.normalize_to(usage.input_tokens);
    values.model = values.model.saturating_add(usage.output_tokens);
    values.normalize_to(usage.total_tokens);
    values
}

pub fn estimate_current_context(context: &ModelContext, total_tokens: u64) -> ContextTokenUsage {
    let mut values = estimate_input(context);
    values.normalize_to(total_tokens);
    values
}

fn estimate_input(context: &ModelContext) -> ContextTokenUsage {
    let mut values = ContextTokenUsage::default();
    if !context.tools.is_empty() {
        values.system = values
            .system
            .saturating_add(estimate_json(&Value::Array(context.tools.clone())));
    }
    for message in &context.messages {
        let tokens = estimate_json(message);
        match message_category(message) {
            Category::System => values.system = values.system.saturating_add(tokens),
            Category::Compact => values.compact = values.compact.saturating_add(tokens),
            Category::Memory => values.memory = values.memory.saturating_add(tokens),
            Category::User => values.user = values.user.saturating_add(tokens),
            Category::Model => values.model = values.model.saturating_add(tokens),
            Category::Tool => values.tool = values.tool.saturating_add(tokens),
        }
    }
    let full_context = json!({
        "messages": &context.messages,
        "tools": &context.tools,
    });
    values.normalize_to(estimate_json(&full_context));
    values
}

#[derive(Clone, Copy)]
enum Category {
    System,
    Compact,
    Memory,
    User,
    Model,
    Tool,
}

fn message_category(message: &Value) -> Category {
    match message.get("role").and_then(Value::as_str) {
        Some("system") => Category::System,
        Some("tool") => Category::Tool,
        Some("assistant") => Category::Model,
        Some("user") => {
            if message_contains_image(message) {
                return Category::Tool;
            }
            let content = message.get("content").and_then(Value::as_str).unwrap_or("");
            if content.starts_with("<system_prompt_injection type=\"compact_summary\">")
                || content.starts_with("<system_prompt_injection type=\"compact\">")
            {
                Category::Compact
            } else if content.starts_with("<system_prompt_injection type=\"turn_history\">") {
                Category::Memory
            } else if content.starts_with("<system_prompt_injection ") {
                Category::System
            } else {
                Category::User
            }
        }
        _ => Category::Model,
    }
}

fn message_contains_image(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|part| {
                matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("image_url" | "input_image")
                ) || part.get("image_url").is_some()
            })
        })
}

fn estimate_json(value: &Value) -> u64 {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| json!(null).to_string());
    u64::try_from(o200k_base_singleton().encode_ordinary(&encoded).len()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_request_uses_real_total_and_keeps_every_category() {
        let context = ModelContext {
            messages: vec![
                json!({"role":"system","content":"system policy"}),
                json!({"role":"user","content":"<system_prompt_injection type=\"compact_summary\">\nsummary\n</system_prompt_injection>"}),
                json!({"role":"user","content":"<system_prompt_injection type=\"turn_history\">\nhistory\n</system_prompt_injection>"}),
                json!({"role":"user","content":"question"}),
                json!({"role":"assistant","content":"answer"}),
                json!({"role":"tool","content":"tool result"}),
            ],
            tools: vec![json!({"type":"function","function":{"name":"Read"}})],
        };
        let values = estimate_request(
            &context,
            ApiUsage {
                input_tokens: 9_000,
                output_tokens: 1_000,
                total_tokens: 10_000,
            },
        );
        assert_eq!(values.sum(), 10_000);
        assert!(values.system > 0);
        assert!(values.compact > 0);
        assert!(values.memory > 0);
        assert!(values.user > 0);
        assert!(values.model >= 1_000);
        assert!(values.tool > 0);
    }

    #[test]
    fn image_messages_count_as_tool_input() {
        let context = ModelContext {
            messages: vec![json!({
                "role":"user",
                "content":[
                    {"type":"text","text":"stored image"},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}
                ]
            })],
            tools: Vec::new(),
        };
        let values = estimate_current_context(&context, 500);
        assert_eq!(values.sum(), 500);
        assert_eq!(values.tool, 500);
    }
}
