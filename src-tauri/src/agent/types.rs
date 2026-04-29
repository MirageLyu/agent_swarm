use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// FM-14: Agent paused waiting for human approval on a tool call,
    /// follow-up escalation, or budget threshold. The watchdog (L1/L4)
    /// must NOT count time spent in this state — see `engine.rs`.
    WaitingApproval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStep {
    pub step_number: u32,
    pub kind: StepKind,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    LlmCall,
    ToolUse,
    ToolResult,
    Checkpoint,
    Error,
    Message,
}
