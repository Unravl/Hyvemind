//! Worker agent for feature implementation.
//!
//! The worker takes a feature and a scout plan, then executes the
//! implementation. It produces a transcript and a structured handoff
//! that summarizes what was done.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::core::handoff::{handoff_from_session, HandoffParseFailed, WorkerHandoff};
use crate::core::swarm_context::SwarmContext;
use crate::domain::swarm::Feature;
use crate::pi::session::PiSession;

/// System prompt for the worker agent, loaded at compile time.
const WORKER_SYSTEM_PROMPT: &str = include_str!("../../prompts/worker_system.md");

/// Result of a worker execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResult {
    /// The parsed handoff from the worker.
    pub handoff: WorkerHandoff,
    /// The full transcript of the worker session.
    pub transcript: String,
}

/// Returns the default system prompt for the worker role.
///
/// This is intended to be passed to `PiSessionOptions::for_worker()` when
/// spawning the session, so that the Pi SDK delivers it via its
/// `systemPromptOverride` mechanism.
pub fn default_system_prompt() -> &'static str {
    WORKER_SYSTEM_PROMPT
}

/// Run the worker agent on a feature with a scout plan.
///
/// The Pi session MUST have been spawned with the worker system prompt
/// already configured via `PiSessionOptions::for_worker()`. This function
/// sends only the user-facing implementation request -- the system prompt is
/// delivered by the Pi SDK at session creation time, not embedded in the
/// user message.
///
/// Sends the feature details and scout plan to the Pi session, collects
/// the full response, parses the handoff from the transcript, and returns
/// the structured result.
#[tracing::instrument(
    skip_all,
    fields(
        agent = "worker",
        feature_id = %feature.id,
        run_id = tracing::field::Empty,
    )
)]
pub async fn run_worker(
    session: &Arc<PiSession>,
    feature: &Feature,
    plan: &str,
    working_dir: &Path,
    _system_prompt: &str,
    swarm_context: Option<&SwarmContext>,
) -> Result<WorkerResult> {
    // NOTE: The system prompt is now delivered via PiSessionOptions::for_worker()
    // at session spawn time. The `_system_prompt` parameter is retained for
    // backward compatibility but is no longer embedded in the user message.

    let user_message = build_worker_prompt(feature, plan, working_dir, swarm_context);

    tracing::debug!(
        feature_id = %feature.id,
        prompt_len = user_message.len(),
        plan_len = plan.len(),
        "worker prompt built"
    );
    tracing::trace!(
        feature_id = %feature.id,
        full_prompt = %user_message,
        "worker full prompt"
    );

    tracing::info!(
        feature_id = %feature.id,
        feature_name = %feature.name,
        "starting worker implementation"
    );

    session
        .send_prompt(&user_message, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "worker send_prompt failed for feature '{}'",
            feature.id
        ))?;

    let transcript = session
        .collect_response()
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context(format!(
            "worker collect_response failed for feature '{}'",
            feature.id
        ))?;

    tracing::debug!(
        feature_id = %feature.id,
        transcript_len = transcript.len(),
        "worker transcript received"
    );
    tracing::trace!(
        feature_id = %feature.id,
        full_transcript = %transcript,
        "worker full transcript"
    );

    tracing::info!(
        feature_id = %feature.id,
        transcript_len = transcript.len(),
        "worker session complete, parsing handoff"
    );

    // The Worker MUST emit its handoff via the `submit_handoff` tool. The
    // Queen loop downcasts on `HandoffParseFailed` so Nurse can synthesize a
    // visible intervention before the feature is marked Failed.
    let handoff = handoff_from_session(session, &feature.id).map_err(|e| {
        let reason = e
            .downcast_ref::<HandoffParseFailed>()
            .map(|h| h.reason.clone())
            .unwrap_or_else(|| e.to_string());
        anyhow::Error::new(HandoffParseFailed { reason }).context(format!(
            "failed to capture handoff for feature '{}'",
            feature.id
        ))
    })?;

    tracing::Span::current().record("run_id", handoff.run_id.as_str());

    tracing::debug!(
        feature_id = %feature.id,
        handoff = ?handoff,
        "worker handoff parsed"
    );

    tracing::info!(
        feature_id = %feature.id,
        success_state = %handoff.success_state,
        "worker handoff parsed"
    );

    Ok(WorkerResult {
        handoff,
        transcript,
    })
}

/// Render a `## Swarm Context` block from any per-swarm context artifacts.
///
/// Returns an empty string when there is genuinely nothing to inject (caller
/// then omits the header entirely — no empty section). The block is composed
/// of up to four subsections, each appearing only if its source data exists:
///
/// - `### Project Conventions` — contents of `AGENTS.md`
/// - `### Architecture & Environment Notes` — contents of `notes.md`
/// - `### Available Commands` — bullet list of `name -> shell-string` pairs
///   from `services.yaml :: commands`
/// - `### Services` — bullet list of running ambient services from
///   `services.yaml :: services` (name + port if set)
fn render_swarm_context_block(ctx: Option<&SwarmContext>) -> String {
    let Some(ctx) = ctx else { return String::new() };
    if ctx.is_empty() {
        return String::new();
    }

    let mut out = String::from("## Swarm Context\n\n");

    if let Some(agents) = &ctx.agents_md {
        let trimmed = agents.trim();
        if !trimmed.is_empty() {
            out.push_str("### Project Conventions\n\n");
            out.push_str(trimmed);
            out.push_str("\n\n");
        }
    }

    if let Some(notes) = &ctx.notes_md {
        let trimmed = notes.trim();
        if !trimmed.is_empty() {
            out.push_str("### Architecture & Environment Notes\n\n");
            out.push_str(trimmed);
            out.push_str("\n\n");
        }
    }

    if let Some(services_file) = &ctx.services {
        if !services_file.commands.is_empty() {
            out.push_str("### Available Commands\n\n");
            // Sort keys for stable, reproducible output.
            let mut keys: Vec<&String> = services_file.commands.keys().collect();
            keys.sort();
            for key in keys {
                let value = &services_file.commands[key];
                out.push_str(&format!("- `{}` -> `{}`\n", key, value));
            }
            out.push('\n');
        }

        if !services_file.services.is_empty() {
            out.push_str("### Services\n\n");
            for svc in &services_file.services {
                match svc.port {
                    Some(port) => out.push_str(&format!("- {} (port {})\n", svc.name, port)),
                    None => out.push_str(&format!("- {}\n", svc.name)),
                }
            }
            out.push('\n');
        }
    }

    out
}

/// Build the user prompt for the worker agent.
fn build_worker_prompt(
    feature: &Feature,
    plan: &str,
    working_dir: &Path,
    swarm_context: Option<&SwarmContext>,
) -> String {
    let deps_section = if feature.dependencies.is_empty() {
        "None".to_string()
    } else {
        feature.dependencies.join(", ")
    };

    let attempt_info = if feature.fix_attempt_count > 0 {
        format!(
            "\n**Fix Attempt:** {} of {} (this is a retry after a previous failure)\n",
            feature.fix_attempt_count, feature.max_fix_attempts
        )
    } else {
        String::new()
    };

    let swarm_context_block = render_swarm_context_block(swarm_context);

    format!(
        r#"## Feature to Implement

**ID:** {id}
**Name:** {name}
**Description:** {description}

**Dependencies:** {deps}
**Working Directory:** {working_dir}
{attempt_info}
## Scout Plan

{plan}

{swarm_context_block}## Instructions

Implement this feature according to the scout plan above.
Work within the specified working directory.

When you are finished, call the `submit_handoff` tool with `feature_id="{id}"`, a unique `run_id`, `salient_summary`, `what_was_implemented`, `verification`, and `success_state` set to exactly one of `success`, `failure`, or `partial` (lowercase). The Rust backend reads the tool args directly — do not paste the handoff JSON into your text response.
"#,
        id = feature.id,
        name = feature.name,
        description = feature.description,
        deps = deps_section,
        working_dir = working_dir.display(),
        plan = plan,
        attempt_info = attempt_info,
        swarm_context_block = swarm_context_block,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::services::{Service, ServicesFile};
    use std::collections::HashMap;

    #[test]
    fn test_build_worker_prompt() {
        let feature = Feature::new(
            "feat-1".to_string(),
            "Login Page".to_string(),
            "Build the login page".to_string(),
        );
        let plan = "Step 1: Create login component\nStep 2: Add validation";
        let prompt = build_worker_prompt(&feature, plan, Path::new("/tmp/project"), None);

        assert!(prompt.contains("feat-1"));
        assert!(prompt.contains("Login Page"));
        assert!(prompt.contains("Step 1: Create login component"));
        assert!(prompt.contains("submit_handoff"));
        assert!(!prompt.contains("Fix Attempt"));
        // With no context, the Swarm Context block must be omitted entirely.
        assert!(!prompt.contains("## Swarm Context"));
    }

    #[test]
    fn test_build_worker_prompt_with_retry() {
        let mut feature = Feature::new(
            "feat-1".to_string(),
            "Login Page".to_string(),
            "Build the login page".to_string(),
        );
        feature.fix_attempt_count = 2;

        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), None);
        assert!(prompt.contains("Fix Attempt"));
        assert!(prompt.contains("2 of 3"));
    }

    #[test]
    fn test_build_worker_prompt_with_empty_swarm_context_omits_block() {
        let feature = Feature::new("feat-1".to_string(), "X".to_string(), "y".to_string());
        let ctx = SwarmContext::default();
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        // Backwards compat: an all-None SwarmContext is treated as no context.
        assert!(!prompt.contains("## Swarm Context"));
        assert!(!prompt.contains("### Project Conventions"));
        assert!(!prompt.contains("### Architecture & Environment Notes"));
        assert!(!prompt.contains("### Available Commands"));
        assert!(!prompt.contains("### Services"));
    }

    #[test]
    fn test_build_worker_prompt_only_agents_md() {
        let feature = Feature::new("f".into(), "n".into(), "d".into());
        let ctx = SwarmContext {
            agents_md: Some("Use snake_case identifiers.".to_string()),
            ..Default::default()
        };
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        assert!(prompt.contains("## Swarm Context"));
        assert!(prompt.contains("### Project Conventions"));
        assert!(prompt.contains("Use snake_case identifiers."));
        // The other subsections must NOT appear.
        assert!(!prompt.contains("### Architecture & Environment Notes"));
        assert!(!prompt.contains("### Available Commands"));
        assert!(!prompt.contains("### Services"));
    }

    #[test]
    fn test_build_worker_prompt_only_notes_md() {
        let feature = Feature::new("f".into(), "n".into(), "d".into());
        let ctx = SwarmContext {
            notes_md: Some("Tauri 2 + React 18.".to_string()),
            ..Default::default()
        };
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        assert!(prompt.contains("## Swarm Context"));
        assert!(prompt.contains("### Architecture & Environment Notes"));
        assert!(prompt.contains("Tauri 2 + React 18."));
        assert!(!prompt.contains("### Project Conventions"));
        assert!(!prompt.contains("### Available Commands"));
    }

    #[test]
    fn test_build_worker_prompt_only_commands() {
        let feature = Feature::new("f".into(), "n".into(), "d".into());
        let mut commands = HashMap::new();
        commands.insert("test".to_string(), "cargo test".to_string());
        commands.insert("build".to_string(), "cargo build".to_string());
        let ctx = SwarmContext {
            services: Some(ServicesFile {
                commands,
                services: Vec::new(),
            }),
            ..Default::default()
        };
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        assert!(prompt.contains("## Swarm Context"));
        assert!(prompt.contains("### Available Commands"));
        assert!(prompt.contains("`test` -> `cargo test`"));
        assert!(prompt.contains("`build` -> `cargo build`"));
        assert!(!prompt.contains("### Services"));
        assert!(!prompt.contains("### Project Conventions"));
    }

    #[test]
    fn test_build_worker_prompt_only_services() {
        let feature = Feature::new("f".into(), "n".into(), "d".into());
        let services = vec![
            Service {
                name: "postgres".into(),
                port: Some(5432),
                ..Default::default()
            },
            Service {
                name: "worker".into(),
                port: None,
                ..Default::default()
            },
        ];
        let ctx = SwarmContext {
            services: Some(ServicesFile {
                commands: HashMap::new(),
                services,
            }),
            ..Default::default()
        };
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        assert!(prompt.contains("## Swarm Context"));
        assert!(prompt.contains("### Services"));
        assert!(prompt.contains("- postgres (port 5432)"));
        assert!(prompt.contains("- worker\n"));
        assert!(!prompt.contains("### Available Commands"));
    }

    #[test]
    fn test_build_worker_prompt_full_swarm_context() {
        let feature = Feature::new("f".into(), "n".into(), "d".into());
        let mut commands = HashMap::new();
        commands.insert("install".to_string(), "npm install".to_string());
        commands.insert("test".to_string(), "npm test".to_string());
        let services = vec![Service {
            name: "postgres".into(),
            port: Some(5432),
            ..Default::default()
        }];
        let ctx = SwarmContext {
            agents_md: Some("Conventions go here.".to_string()),
            notes_md: Some("Architecture notes.".to_string()),
            services: Some(ServicesFile { commands, services }),
        };
        let prompt = build_worker_prompt(&feature, "plan", Path::new("/tmp"), Some(&ctx));
        // All four subsections present and in the documented order.
        let h = prompt
            .find("## Swarm Context")
            .expect("Swarm Context header");
        let proj = prompt
            .find("### Project Conventions")
            .expect("Project Conventions");
        let arch = prompt
            .find("### Architecture & Environment Notes")
            .expect("Architecture & Environment Notes");
        let cmds = prompt
            .find("### Available Commands")
            .expect("Available Commands");
        let svcs = prompt.find("### Services").expect("Services");
        assert!(h < proj && proj < arch && arch < cmds && cmds < svcs);
        // The Instructions section still comes AFTER the Swarm Context.
        let instructions = prompt.find("## Instructions").expect("Instructions header");
        assert!(svcs < instructions);
    }

    // -- E2E against MockRpcClient ------------------------------------------

    use crate::pi::events::PiEvent;
    use crate::pi::mock::mock_session;
    use crate::pi::rpc::PiCommand;

    /// Success path: the model calls `submit_handoff` and the captured tool
    /// args are deserialised into the typed `WorkerResult`.
    #[tokio::test]
    async fn worker_e2e_success_against_mock_transport() {
        let (session, mock) = mock_session("worker-success");
        mock.emit_text_chunk("I have implemented the feature.\n");
        mock.emit(PiEvent::ToolExecutionStart {
            tool_call_id: "tc-1".to_string(),
            name: "submit_handoff".to_string(),
            args: serde_json::json!({
                "feature_id": "feat-w",
                "run_id": "run-1",
                "salient_summary": "done",
                "what_was_implemented": "login form",
                "verification": "npm test passes",
                "success_state": "success",
            }),
        });
        mock.emit_agent_end();

        let feature = Feature::new(
            "feat-w".into(),
            "Worker Feature".into(),
            "exercise worker".into(),
        );

        let result = run_worker(
            &session,
            &feature,
            "Step 1\nStep 2",
            Path::new("/tmp/proj"),
            "",
            None,
        )
        .await
        .expect("worker should succeed against mock transport");

        assert_eq!(result.handoff.feature_id, "feat-w");
        assert_eq!(result.handoff.run_id, "run-1");
        assert_eq!(result.handoff.success_state.to_string(), "success");

        // Worker sent exactly one prompt; it embedded the plan.
        let log = mock.sent_commands().await;
        assert_eq!(log.len(), 1);
        match &log[0] {
            PiCommand::Prompt { message, .. } => {
                assert!(message.contains("Step 1"));
                assert!(message.contains("feat-w"));
            }
            other => panic!("expected Prompt, got {:?}", other),
        }
    }

    /// Failure path: the worker streams text but never calls
    /// `submit_handoff`. `run_worker` errors with a typed `HandoffParseFailed`
    /// in the chain so the Queen loop can synthesize a Nurse intervention.
    #[tokio::test]
    async fn worker_e2e_missing_tool_call_against_mock_transport() {
        let (session, mock) = mock_session("worker-missing-tool");
        mock.emit_text_chunk("I forgot to call submit_handoff.\n");
        mock.emit_text_chunk("Sorry about that!\n");
        mock.emit_agent_end();

        let feature = Feature::new("feat-bad".into(), "bad".into(), "y".into());
        let err = run_worker(&session, &feature, "plan", Path::new("/tmp"), "", None)
            .await
            .expect_err("worker should reject a transcript without a submit_handoff call");

        // The error chain MUST carry the typed marker so the Queen loop
        // can identify malformed-handoff failures specifically.
        let downcast = err
            .chain()
            .find_map(|e| e.downcast_ref::<crate::core::handoff::HandoffParseFailed>());
        assert!(
            downcast.is_some(),
            "expected HandoffParseFailed in chain; got {:#}",
            err
        );
    }
}
