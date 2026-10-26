use std::fmt::Write as _;

use ternilo_protocol::{AgentRole, CoordinationConfig, CoordinationMode};

pub(super) fn instructions(config: &CoordinationConfig) -> String {
    let strategy = match config.mode {
        CoordinationMode::Independent => {
            "Complete the task in the current session. Agent delegation is disabled."
        }
        CoordinationMode::Assisted => {
            "Keep ownership of the main task. Delegate substantial, well-defined supporting work when it improves the result. Complete small changes directly."
        }
        CoordinationMode::Parallel => {
            "Identify independent tasks and start them concurrently within the configured limit. Keep tasks with shared edits or output dependencies in order. Complete a single coherent scope directly."
        }
    };
    let mut text = format!("# Coordination\n\n{strategy}");
    if config.mode == CoordinationMode::Independent {
        return text;
    }
    let _ = write!(
        text,
        "\n\nAt most {} agents may run concurrently. The lead does not count toward this limit.",
        config.parallel_limit()
    );
    text.push_str("\n\nGive each agent a bounded objective, relevant inputs, constraints, and expected evidence. Keep write scopes disjoint. Reuse an existing agent for related follow-up work. Review results and verify the integrated outcome before reporting completion. A cheaper model is not a substitute for verification. Respect the user's delegation instructions.");
    text.push_str("\n\nConfigured roles (select through spawn_agent.role):");
    for role in AgentRole::ALL {
        let permission = if config.role_read_only(role) {
            "read-only"
        } else {
            "inherits the session's permission ceiling"
        };
        let purpose = match role {
            AgentRole::Investigator => "Gather and explain evidence",
            AgentRole::Implementer => "Implement an assigned change",
            AgentRole::Reviewer => "Check correctness and verification",
        };
        let _ = write!(text, "\n- {}: {purpose}; {permission}.", role.as_str());
    }
    text.push_str("\n\nRead-only roles cannot use shell, terminals, jobs or file writes. When run_check is present, they may execute only the named checks approved in the shell plugin settings, in a read-only workspace sandbox with writable temporary caches. Inspect the available check names before delegating verification. If no checks are configured, have the lead run verification and give the reviewer its evidence.");
    text
}
