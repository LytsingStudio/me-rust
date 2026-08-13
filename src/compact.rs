use serde_json::{Value, json};

use crate::{
    event::CompactStage,
    toolbox::{ToolboxExecutionError, ToolboxTool},
};

pub const TOOL_NAME: &str = "Compact";
pub const TOOLBOX_NAME: &str = "Compact";

const TOOLBOX_BRIEF: &str = r#"Compact replaces the conversation accumulated so far with one detailed continuation summary when context space is running low.

Call Compact only after the runtime explicitly warns that context space is running low, and only at a safe point: finish the current atomic action, persist any valuable WorkMap state, and make Compact the only tool call in that model response. Compact has no arguments. The runtime rejects Compact when no warning is active and reports the current context usage. After accepting the request, the runtime will ask you for a text-only summary, activate it only after successful completion, then continue the same Agent turn. WorkMap survives compaction independently of the summary; after compaction succeeds, call WorkMap.Read before any further non-WorkMap action and repeat any final audit. Do not call Compact merely to shorten a healthy context, and do not narrate or imitate compaction in assistant text."#;

const INSTRUCTIONS: &str = r#"Call with an empty object only after the runtime explicitly issues a context-low warning. Compact must be the sole tool call in the response. A call made without an active warning is rejected with the current context usage. After the tool is accepted, the runtime performs the summary request automatically; do not issue other tools in that response."#;
const ROUTE: &str = "Compress the accumulated conversation at a safe point only after the runtime explicitly warns that context is running low. It must be the sole tool call in the response.";
const EXAMPLES: &str = r#"Input: {}
Meaning: request one context compaction at the current safe point."#;

pub const SEGMENTED_ANALYSIS_PROMPT: &str = r#"CRITICAL: Respond with raw text only. Do not call any tools.

This is stage 1 of a six-stage context compaction process.

The complete pre-compaction conversation is still available and unchanged. Your task in this stage is to analyze and organize that conversation for the five summary sections defined below.

Do not write the final summary.
Do not write any of the five final sections.
Do not use XML tags such as <analysis> or <summary>.
Output only your compaction preparation analysis. This analysis will remain visible during the following five stages and will be used to produce each final section separately.

The final summary will contain these five sections:

1. Primary Request and Intent
   Preserve the user's explicit objectives, constraints, preferences, acceptance requirements, corrections, and direction changes. Distinguish active requirements from requirements that were superseded or withdrawn. Preserve security-sensitive, permission-related, credential-handling, and data-handling constraints exactly where their wording matters.

2. Key Technical Context and Decisions
   Preserve important technical concepts, architecture, runtime behavior, confirmed facts, material assumptions, interfaces, protocols, invariants, design decisions, trade-offs, rejected approaches, and the reasons behind decisions.

3. Files, Code, and Artifacts
   Preserve relevant files, directories, functions, types, interfaces, code locations, commands, configurations, generated artifacts, important code snippets, actual changes, and why each item matters for continuing the work.

4. Problems, Investigations, and Resolutions
   Treat each material problem as one lifecycle: observed symptom, evidence, investigation, confirmed or suspected cause, unsuccessful attempts, chosen resolution, reason for that resolution, verification result, and anything still unresolved. Do not separately duplicate the same problem as both an error and a problem-solving entry.

5. Current State and Continuation Plan
   Preserve the exact current state: completed work, active work, precise stopping point, remaining work, blockers, required evidence or prerequisites, and the next operation that follows directly from the latest active request. If the requested work is already complete, state that no continuation step remains.

Analyze the conversation chronologically, then prepare a coverage plan for these five sections.

Your analysis must:

- identify every active user request and every material correction or constraint;
- distinguish observed facts from inference and unresolved uncertainty;
- distinguish completed, active, pending, cancelled, and superseded work;
- preserve exact technical identifiers, paths, function names, commands, errors, interfaces, and important code where needed;
- identify contradictions and resolve them using the latest applicable user instruction;
- assign each material fact to the most appropriate final section;
- avoid unnecessary duplication between sections;
- identify details that must be quoted exactly to prevent semantic drift;
- give extra attention to the latest conversation and the exact continuation point;
- include enough detail that the following stages can write each section without re-analyzing or guessing.

Output only the preparation analysis. Do not produce the final compacted summary in this stage."#;

const PRIMARY_REQUEST_PROMPT: &str = r#"This is stage 2 of the context compaction process. Output only the complete final section `1. Primary Request and Intent` as raw Markdown, including that exact heading and its body. Use the preparation analysis and the unchanged full conversation. Do not output analysis, any other section, XML tags, commentary, or tool calls."#;

const TECHNICAL_CONTEXT_PROMPT: &str = r#"This is stage 3 of the context compaction process. Output only the complete final section `2. Key Technical Context and Decisions` as raw Markdown, including that exact heading and its body. Use the preparation analysis, previously completed section, and the unchanged full conversation. Do not output analysis, any other section, XML tags, commentary, or tool calls."#;

const FILES_AND_ARTIFACTS_PROMPT: &str = r#"This is stage 4 of the context compaction process. Output only the complete final section `3. Files, Code, and Artifacts` as raw Markdown, including that exact heading and its body. Use the preparation analysis, previously completed sections, and the unchanged full conversation. Do not output analysis, any other section, XML tags, commentary, or tool calls."#;

const PROBLEMS_PROMPT: &str = r#"This is stage 5 of the context compaction process. Output only the complete final section `4. Problems, Investigations, and Resolutions` as raw Markdown, including that exact heading and its body. Treat each problem as one lifecycle rather than recreating separate error and problem-solving sections. Use the preparation analysis, previously completed sections, and the unchanged full conversation. Do not output analysis, any other section, XML tags, commentary, or tool calls."#;

const CURRENT_STATE_PROMPT: &str = r#"This is stage 6 of the context compaction process. Output only the complete final section `5. Current State and Continuation Plan` as raw Markdown, including that exact heading and its body. Semantically integrate completed work, active work, the exact stopping point, pending work, blockers, prerequisites, and the directly applicable next operation. If the request is complete, explicitly state that no continuation step remains. Use the preparation analysis, previously completed sections, and the unchanged full conversation. Do not output analysis, any other section, XML tags, commentary, or tool calls."#;

pub fn segmented_prompt(stage: CompactStage) -> &'static str {
    match stage {
        CompactStage::Analysis => SEGMENTED_ANALYSIS_PROMPT,
        CompactStage::PrimaryRequestAndIntent => PRIMARY_REQUEST_PROMPT,
        CompactStage::KeyTechnicalContextAndDecisions => TECHNICAL_CONTEXT_PROMPT,
        CompactStage::FilesCodeAndArtifacts => FILES_AND_ARTIFACTS_PROMPT,
        CompactStage::ProblemsInvestigationsAndResolutions => PROBLEMS_PROMPT,
        CompactStage::CurrentStateAndContinuationPlan => CURRENT_STATE_PROMPT,
    }
}

pub fn merge_segmented_summary<'a>(sections: impl IntoIterator<Item = &'a str>) -> String {
    sections
        .into_iter()
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub const WORKER_COMPACT_PROMPT: &str = r#"CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

- Do NOT call any tool, regardless of which tools are currently available.
- You already have all the context you need in the conversation above.
- Tool calls will be REJECTED and will waste your only turn — you will fail the task.
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.

Your task is to create a detailed summary of the Worker conversation so far, paying close attention to the Manager's explicit instructions and your previous actions.
This summary should be thorough in capturing technical details, code patterns, execution evidence, and operational state that would be essential for continuing the Manager's requested work without losing context.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts and ensure you've covered all necessary points. In your analysis process:

1. Chronologically analyze each Manager instruction and section of the conversation. For each section thoroughly identify:
   - The Manager's explicit instruction, intended operational result, scope, and constraints
   - Your approach to executing the Manager's instruction
   - Key decisions already supplied by the Manager, technical concepts, code patterns, and mechanical execution details
   - Specific details like:
     - file names
     - full code snippets
     - function signatures
     - file edits
     - commands, tool state, paths, identifiers, and evidence needed to continue
   - Errors that you ran into and how you handled or reported them
   - Pay special attention to specific corrections or feedback from the Manager, especially if the Manager told you to do something differently.
   - Note any security-relevant instructions or constraints communicated by the Manager or the system (e.g., sensitive files or data to avoid, operations that must not be performed, credential or secret handling rules). These MUST be preserved verbatim in the summary so they continue to apply after compaction.
2. Double-check for technical accuracy and completeness, addressing each required element thoroughly. Preserve observed facts and evidence without inventing interpretation, review judgments, acceptance judgments, or substantive content that the Manager did not supply.

Your summary should include the following sections:

1. Manager Request and Scope: Capture the Manager's current instruction, intended operational result, boundaries, supplied content, and continuing constraints in detail.
2. Key Technical Concepts: List all important technical concepts, technologies, and code patterns already established by the Manager or observed during execution.
3. Files, Code, and Evidence: Enumerate specific files, code sections, logs, commands, tool state, paths, identifiers, and evidence examined, modified, created, or still needed. Pay special attention to the most recent work and include full code snippets where applicable and a summary of why each item matters for continuation.
4. Errors and fixes: List all errors that you ran into, how you handled or reported them, and any relevant correction from the Manager.
5. Execution Progress: Document completed operations, material observations, changes made, checks executed, evidence collected, and ongoing operational work without adding review or acceptance conclusions.
6. Pending Operations: Outline any operations the Manager explicitly requested that are not yet complete, including blockers and required evidence.
7. Current Work: Describe in detail precisely what was being executed immediately before this summary request, paying special attention to the most recent Manager instruction and Worker actions. Include file names, code snippets, tool state, and exact continuation points where applicable.
8. Optional Next Step: List the next operational step only when it follows directly from the Manager's most recent explicit instruction and the work already in progress. Do not invent a new objective, solution, judgment, or adjacent task.
                       If there is a next step, include direct quotes from the most recent Manager instruction showing exactly what operation was requested and where execution stopped. This should be verbatim to ensure there is no drift.

Here's an example of how your output should be structured:

<example>
<analysis>
[Your analysis, ensuring all operational details are covered thoroughly and accurately]
</analysis>

<summary>
1. Manager Request and Scope:
   [Detailed description]

2. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]
   - [...]

3. Files, Code, and Evidence:
   - [File Name or Evidence Item 1]
      - [Summary of why this item is important]
      - [Summary of observations or exact changes, if any]
      - [Important Code Snippet, Path, Identifier, or Tool State]
   - [File Name or Evidence Item 2]
      - [Important detail]
   - [...]

4. Errors and fixes:
    - [Detailed description of error 1]:
      - [How it was handled or reported]
      - [Manager correction or feedback if any]
    - [...]

5. Execution Progress:
   [Description of completed operations, evidence, and ongoing work]

6. Pending Operations:
   - [Operation 1]
   - [Operation 2]

7. Current Work:
   [Precise description of the current operation and continuation point]

8. Optional Next Step:
   [Optional next operational step]

</summary>
</example>

Please provide your summary based on the Worker conversation so far, following this structure and ensuring precision and thoroughness in your response.

REMINDER: Do NOT call any tools. Respond with plain text only — an <analysis> block followed by a <summary> block. Tool calls will be rejected and you will fail the task."#;

pub fn catalog_parts() -> (Vec<ToolboxTool>, (String, String)) {
    (
        vec![ToolboxTool {
            toolbox: TOOLBOX_NAME.into(),
            local_name: TOOL_NAME.into(),
            full_name: TOOL_NAME.into(),
            api_name: TOOL_NAME.into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "required": ["status"],
                "properties": {"status": {"const": "accepted"}},
                "additionalProperties": false
            }),
            instructions: INSTRUCTIONS.into(),
            route: ROUTE.into(),
            examples: EXAMPLES.into(),
        }],
        (TOOLBOX_NAME.into(), TOOLBOX_BRIEF.into()),
    )
}

pub fn execute(
    arguments: &str,
    warning_active: bool,
    used_tokens: Option<u64>,
    context_window: u64,
    output_reservation: u64,
) -> std::result::Result<Value, ToolboxExecutionError> {
    let value: Value =
        serde_json::from_str(arguments).map_err(|error| ToolboxExecutionError::Tool {
            code: "invalid_arguments".into(),
            message: error.to_string(),
            retryable: false,
        })?;
    if value.as_object().is_none_or(|object| !object.is_empty()) {
        return Err(ToolboxExecutionError::Tool {
            code: "invalid_arguments".into(),
            message: "Compact accepts only an empty object".into(),
            retryable: false,
        });
    }
    if !warning_active {
        let message = match used_tokens {
            Some(used_tokens) => {
                let remaining = usable_remaining(
                    used_tokens,
                    context_window,
                    output_reservation,
                );
                let percentage = if context_window == 0 {
                    0.0
                } else {
                    used_tokens as f64 * 100.0 / context_window as f64
                };
                if advisory(used_tokens, context_window, output_reservation).is_none() {
                    format!(
                        "Context is healthy: {used_tokens}/{context_window} tokens used ({percentage:.1}%), with {remaining} usable tokens remaining after the response budget. No compaction warning is active, so Compact is not allowed or needed. Continue the task without compacting."
                    )
                } else {
                    format!(
                        "No compaction warning was active when this response began. Current context usage is {used_tokens}/{context_window} tokens ({percentage:.1}%), with {remaining} usable tokens remaining after the response budget. Compact is not allowed in this response; continue and wait for the runtime warning before calling Compact."
                    )
                }
            }
            None => "Current context usage is not yet available and no compaction warning is active. Compact is not allowed or needed; continue the task without compacting and wait for an explicit runtime warning.".into(),
        };
        return Err(ToolboxExecutionError::Tool {
            code: "compact_not_needed".into(),
            message,
            retryable: false,
        });
    }
    Ok(json!({"status": "accepted"}))
}

pub fn format_summary(summary: &str) -> String {
    let without_analysis = strip_first_tagged_section(summary, "analysis");
    let formatted = replace_first_summary(&without_analysis);
    collapse_blank_lines(formatted.trim())
}

pub fn continuation_message(summary: &str) -> String {
    format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\nThe persistent WorkMap survives compaction separately from this summary. The summary is not authoritative for the Current Objective, its Plan IDs, Notes, or completion state. Before any further non-WorkMap action, call WorkMap.Read and resume from that result. Any final-answer audit performed before compaction is stale and must be repeated.\n\n{}",
        summary.trim()
    )
}

fn strip_first_tagged_section(value: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = value.find(&open) else {
        return value.to_owned();
    };
    let Some(relative_end) = value[start + open.len()..].find(&close) else {
        return value.to_owned();
    };
    let end = start + open.len() + relative_end + close.len();
    format!("{}{}", &value[..start], &value[end..])
}

fn replace_first_summary(value: &str) -> String {
    let Some(start) = value.find("<summary>") else {
        return value.to_owned();
    };
    let content_start = start + "<summary>".len();
    let Some(relative_end) = value[content_start..].find("</summary>") else {
        return value.to_owned();
    };
    let end = content_start + relative_end;
    format!(
        "{}Summary:\n{}{}",
        &value[..start],
        value[content_start..end].trim(),
        &value[end + "</summary>".len()..]
    )
}

fn collapse_blank_lines(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut blank = false;
    for line in value.lines() {
        if line.trim().is_empty() {
            if !blank && !output.is_empty() {
                output.push('\n');
            }
            blank = true;
        } else {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(line);
            blank = false;
        }
    }
    output
}

pub fn advisory(used_tokens: u64, context_window: u64, output_reservation: u64) -> Option<String> {
    let remaining = usable_remaining(used_tokens, context_window, output_reservation);
    let (mild, urgent) = if context_window < 500_000 {
        (48_000, 32_000)
    } else {
        (176_000, 128_000)
    };
    if remaining < urgent {
        Some(format!(
            "Only {remaining} usable context tokens remain after reserving the response budget. Context is nearly exhausted. At the next safe point, you must call Compact immediately as the sole tool call before continuing further work."
        ))
    } else if remaining < mild {
        Some("Usable context space after the response budget is running low. Consider calling Compact as the sole tool call at the next safe point before continuing substantial work.".into())
    } else {
        None
    }
}

pub fn usable_remaining(used_tokens: u64, context_window: u64, output_reservation: u64) -> u64 {
    context_window.saturating_sub(used_tokens.saturating_add(output_reservation))
}

pub fn emergency_output_limit(
    used_tokens: u64,
    context_window: u64,
    configured_output: u64,
) -> Option<u64> {
    if configured_output == 0 {
        return None;
    }
    let safety_margin = if context_window < 500_000 {
        32_000
    } else {
        128_000
    };
    let safe_limit = context_window
        .saturating_sub(used_tokens)
        .saturating_sub(safety_margin)
        .max(1);
    (safe_limit < configured_output).then_some(safe_limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_is_one_empty_input_native_tool() {
        let (tools, _) = catalog_parts();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].full_name, TOOL_NAME);
        assert_eq!(
            execute("{}", true, Some(90_000), 100_000, 0).unwrap(),
            json!({"status": "accepted"})
        );
        assert!(execute(r#"{"extra":true}"#, true, Some(90_000), 100_000, 0).is_err());
    }

    #[test]
    fn compact_requires_an_active_warning_and_reports_usage() {
        let error = execute("{}", false, Some(52_000), 100_000, 0).unwrap_err();
        assert!(matches!(
            error,
            ToolboxExecutionError::Tool {
                code,
                message,
                retryable: false,
            } if code == "compact_not_needed"
                && message.contains("52000/100000")
                && message.contains("52.0%")
                && message.contains("48000 usable tokens remaining")
                && message.contains("Context is healthy")
        ));

        let crossed = execute("{}", false, Some(52_001), 100_000, 0).unwrap_err();
        assert!(matches!(
            crossed,
            ToolboxExecutionError::Tool { message, .. }
                if message.contains("No compaction warning was active when this response began")
                    && message.contains("wait for the runtime warning")
        ));
    }

    #[test]
    fn segmented_and_worker_prompts_have_distinct_contracts() {
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("stage 1 of a six-stage"));
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("five summary sections"));
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("1. Primary Request and Intent"));
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("5. Current State and Continuation Plan"));
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("Output only the preparation analysis"));
        assert!(SEGMENTED_ANALYSIS_PROMPT.contains("Do not use XML tags"));
        assert!(!SEGMENTED_ANALYSIS_PROMPT.contains("All user messages"));
        for stage in CompactStage::SEGMENTED.into_iter().skip(1) {
            let prompt = segmented_prompt(stage);
            assert!(prompt.contains("raw Markdown"));
            assert!(prompt.contains("Do not output analysis"));
            assert!(prompt.contains("XML tags"));
        }
        assert!(WORKER_COMPACT_PROMPT.contains("Do NOT call any tool"));
        assert!(WORKER_COMPACT_PROMPT.contains("Manager Request and Scope"));
        assert!(WORKER_COMPACT_PROMPT.contains("Worker conversation"));
        assert!(WORKER_COMPACT_PROMPT.contains("Manager correction or feedback"));
        assert!(WORKER_COMPACT_PROMPT.contains("\n6. Pending Operations:"));
        assert!(WORKER_COMPACT_PROMPT.contains("\n7. Current Work:"));
        assert!(WORKER_COMPACT_PROMPT.contains("\n8. Optional Next Step:"));
        assert!(!WORKER_COMPACT_PROMPT.contains("All user messages"));
        assert!(!WORKER_COMPACT_PROMPT.to_ascii_lowercase().contains("user"));
        assert!(
            !WORKER_COMPACT_PROMPT
                .to_ascii_lowercase()
                .contains("assistant")
        );
        let formatted =
            format_summary("<analysis>draft</analysis>\n\n<summary>\nalpha\n\n\n beta\n</summary>");
        assert_eq!(formatted, "Summary:\nalpha\n\n beta");
        assert!(!formatted.contains("draft"));
        let merged = merge_segmented_summary(["1. First\nbody", " 2. Second\nbody "]);
        assert_eq!(merged, "1. First\nbody\n\n2. Second\nbody");
        let continuation = continuation_message(&merged);
        assert!(continuation.contains("call WorkMap.Read"));
        assert!(continuation.contains("final-answer audit performed before compaction is stale"));
        assert!(continuation.ends_with(&merged));
    }

    #[test]
    fn advisory_uses_both_context_window_classes() {
        assert!(advisory(140_000, 272_000, 0).is_none());
        assert!(advisory(224_000, 272_000, 0).is_none());
        assert!(advisory(224_001, 272_000, 0).is_some());
        assert!(advisory(52_001, 100_000, 0).is_some());
        assert!(advisory(52_000, 100_000, 0).is_none());
        assert!(advisory(68_001, 100_000, 0).unwrap().contains("must call"));
        assert!(
            advisory(824_001, 1_000_000, 0)
                .unwrap()
                .contains("Consider")
        );
        assert!(
            advisory(872_001, 1_000_000, 0)
                .unwrap()
                .contains("must call")
        );
    }

    #[test]
    fn advisory_subtracts_the_reserved_output_budget() {
        let context_window = 1_000_000;
        let output_reservation = 393_216;
        assert!(advisory(430_784, context_window, output_reservation).is_none());
        assert!(
            advisory(430_785, context_window, output_reservation)
                .unwrap()
                .contains("running low")
        );
        assert!(
            advisory(478_785, context_window, output_reservation)
                .unwrap()
                .contains("must call")
        );
        assert_eq!(
            usable_remaining(638_779, context_window, output_reservation),
            0
        );
    }

    #[test]
    fn emergency_output_limit_preserves_the_urgent_safety_margin() {
        assert_eq!(
            emergency_output_limit(638_779, 1_000_000, 393_216),
            Some(233_221)
        );
        assert_eq!(emergency_output_limit(100_000, 1_000_000, 393_216), None);
        assert_eq!(emergency_output_limit(99_000, 100_000, 64_000), Some(1));
        assert_eq!(emergency_output_limit(90_000, 100_000, 0), None);
    }
}
