use crossagents_core as core;
use crossagents_core::MessageId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireOutputStream {
    Text,
    Thought,
}
impl From<&core::OutputStream> for WireOutputStream {
    fn from(v: &core::OutputStream) -> Self {
        match v {
            core::OutputStream::Text => Self::Text,
            core::OutputStream::Thought => Self::Thought,
        }
    }
}
impl From<WireOutputStream> for core::OutputStream {
    fn from(v: WireOutputStream) -> Self {
        match v {
            WireOutputStream::Text => Self::Text,
            WireOutputStream::Thought => Self::Thought,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
impl From<&core::ToolStatus> for WireToolStatus {
    fn from(v: &core::ToolStatus) -> Self {
        match v {
            core::ToolStatus::Pending => Self::Pending,
            core::ToolStatus::InProgress => Self::InProgress,
            core::ToolStatus::Completed => Self::Completed,
            core::ToolStatus::Failed => Self::Failed,
        }
    }
}
impl From<WireToolStatus> for core::ToolStatus {
    fn from(v: WireToolStatus) -> Self {
        match v {
            WireToolStatus::Pending => Self::Pending,
            WireToolStatus::InProgress => Self::InProgress,
            WireToolStatus::Completed => Self::Completed,
            WireToolStatus::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireExitCause {
    Exited,
    Crashed,
    Killed,
    Lost,
}
impl From<&core::ExitCause> for WireExitCause {
    fn from(v: &core::ExitCause) -> Self {
        match v {
            core::ExitCause::Exited => Self::Exited,
            core::ExitCause::Crashed => Self::Crashed,
            core::ExitCause::Killed => Self::Killed,
            core::ExitCause::Lost => Self::Lost,
        }
    }
}
impl From<WireExitCause> for core::ExitCause {
    fn from(v: WireExitCause) -> Self {
        match v {
            WireExitCause::Exited => Self::Exited,
            WireExitCause::Crashed => Self::Crashed,
            WireExitCause::Killed => Self::Killed,
            WireExitCause::Lost => Self::Lost,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireStopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    Error,
    Interrupted,
}
impl From<&core::StopReason> for WireStopReason {
    fn from(v: &core::StopReason) -> Self {
        match v {
            core::StopReason::EndTurn => Self::EndTurn,
            core::StopReason::MaxTokens => Self::MaxTokens,
            core::StopReason::MaxTurnRequests => Self::MaxTurnRequests,
            core::StopReason::Refusal => Self::Refusal,
            core::StopReason::Cancelled => Self::Cancelled,
            core::StopReason::Error => Self::Error,
            core::StopReason::Interrupted => Self::Interrupted,
        }
    }
}
impl From<WireStopReason> for core::StopReason {
    fn from(v: WireStopReason) -> Self {
        match v {
            WireStopReason::EndTurn => Self::EndTurn,
            WireStopReason::MaxTokens => Self::MaxTokens,
            WireStopReason::MaxTurnRequests => Self::MaxTurnRequests,
            WireStopReason::Refusal => Self::Refusal,
            WireStopReason::Cancelled => Self::Cancelled,
            WireStopReason::Error => Self::Error,
            WireStopReason::Interrupted => Self::Interrupted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WirePermissionDecision {
    Allow,
    AllowForSession,
    Deny,
    Cancel,
}
impl From<&core::PermissionDecision> for WirePermissionDecision {
    fn from(v: &core::PermissionDecision) -> Self {
        match v {
            core::PermissionDecision::Allow => Self::Allow,
            core::PermissionDecision::AllowForSession => Self::AllowForSession,
            core::PermissionDecision::Deny => Self::Deny,
            core::PermissionDecision::Cancel => Self::Cancel,
        }
    }
}
impl From<WirePermissionDecision> for core::PermissionDecision {
    fn from(v: WirePermissionDecision) -> Self {
        match v {
            WirePermissionDecision::Allow => Self::Allow,
            WirePermissionDecision::AllowForSession => Self::AllowForSession,
            WirePermissionDecision::Deny => Self::Deny,
            WirePermissionDecision::Cancel => Self::Cancel,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WirePermissionRequest {
    pub id: String,
    pub turn_id: Option<String>,
    pub kind: String,
    pub title: String,
}
impl From<&core::PermissionRequest> for WirePermissionRequest {
    fn from(v: &core::PermissionRequest) -> Self {
        Self {
            id: v.id.clone(),
            turn_id: v.turn_id.clone(),
            kind: v.kind.clone(),
            title: v.title.clone(),
        }
    }
}
impl From<WirePermissionRequest> for core::PermissionRequest {
    fn from(v: WirePermissionRequest) -> Self {
        Self {
            id: v.id,
            turn_id: v.turn_id,
            kind: v.kind,
            title: v.title,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireContextPart {
    pub id: String,
    pub name: String,
    pub tokens: u64,
    pub kind: String,
}
impl From<&core::ContextPart> for WireContextPart {
    fn from(v: &core::ContextPart) -> Self {
        Self {
            id: v.id.clone(),
            name: v.name.clone(),
            tokens: v.tokens.clone(),
            kind: v.kind.clone(),
        }
    }
}
impl From<WireContextPart> for core::ContextPart {
    fn from(v: WireContextPart) -> Self {
        Self {
            id: v.id,
            name: v.name,
            tokens: v.tokens,
            kind: v.kind,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTokenUsage {
    pub input: u64,
    pub cached_input: u64,
    #[serde(default)]
    pub cache_write_input: u64,
    pub output: u64,
    pub reasoning_output: u64,
    pub total: u64,
    #[serde(default)]
    pub requests: Option<u64>,
}
impl From<&core::TokenUsage> for WireTokenUsage {
    fn from(v: &core::TokenUsage) -> Self {
        Self {
            input: v.input.clone(),
            cached_input: v.cached_input.clone(),
            cache_write_input: v.cache_write_input.clone(),
            output: v.output.clone(),
            reasoning_output: v.reasoning_output.clone(),
            total: v.total.clone(),
            requests: v.requests.clone(),
        }
    }
}
impl From<WireTokenUsage> for core::TokenUsage {
    fn from(v: WireTokenUsage) -> Self {
        Self {
            input: v.input,
            cached_input: v.cached_input,
            cache_write_input: v.cache_write_input,
            output: v.output,
            reasoning_output: v.reasoning_output,
            total: v.total,
            requests: v.requests,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFileChange {
    pub path: String,
    pub kind: String,
    pub diff: String,
}
impl From<&core::FileChange> for WireFileChange {
    fn from(v: &core::FileChange) -> Self {
        Self {
            path: v.path.clone(),
            kind: v.kind.clone(),
            diff: v.diff.clone(),
        }
    }
}
impl From<WireFileChange> for core::FileChange {
    fn from(v: WireFileChange) -> Self {
        Self {
            path: v.path,
            kind: v.kind,
            diff: v.diff,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireToolDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub changes: Vec<WireFileChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl From<&core::ToolDetail> for WireToolDetail {
    fn from(v: &core::ToolDetail) -> Self {
        Self {
            command: v.command.clone(),
            cwd: v.cwd.clone(),
            exit_code: v.exit_code.clone(),
            duration_ms: v.duration_ms.clone(),
            output: v.output.clone(),
            changes: v.changes.iter().map(Into::into).collect(),
            arguments: v.arguments.clone(),
            result: v.result.clone(),
            error: v.error.clone(),
        }
    }
}
impl From<WireToolDetail> for core::ToolDetail {
    fn from(v: WireToolDetail) -> Self {
        Self {
            command: v.command,
            cwd: v.cwd,
            exit_code: v.exit_code,
            duration_ms: v.duration_ms,
            output: v.output,
            changes: v.changes.into_iter().map(Into::into).collect(),
            arguments: v.arguments,
            result: v.result,
            error: v.error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRateWindow {
    pub name: String,
    pub used_percent: f64,
    pub window_seconds: Option<u64>,
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub remaining: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
}
impl From<&core::RateWindow> for WireRateWindow {
    fn from(v: &core::RateWindow) -> Self {
        Self {
            name: v.name.clone(),
            used_percent: v.used_percent.clone(),
            window_seconds: v.window_seconds.clone(),
            resets_at: v.resets_at.clone(),
            remaining: v.remaining.clone(),
            total: v.total.clone(),
        }
    }
}
impl From<WireRateWindow> for core::RateWindow {
    fn from(v: WireRateWindow) -> Self {
        Self {
            name: v.name,
            used_percent: v.used_percent,
            window_seconds: v.window_seconds,
            resets_at: v.resets_at,
            remaining: v.remaining,
            total: v.total,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}
impl From<&core::Credits> for WireCredits {
    fn from(v: &core::Credits) -> Self {
        Self {
            has_credits: v.has_credits.clone(),
            unlimited: v.unlimited.clone(),
            balance: v.balance.clone(),
        }
    }
}
impl From<WireCredits> for core::Credits {
    fn from(v: WireCredits) -> Self {
        Self {
            has_credits: v.has_credits,
            unlimited: v.unlimited,
            balance: v.balance,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireLimitGroup {
    pub id: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub windows: Vec<WireRateWindow>,
    pub reached: bool,
}
impl From<&core::LimitGroup> for WireLimitGroup {
    fn from(v: &core::LimitGroup) -> Self {
        Self {
            id: v.id.clone(),
            name: v.name.clone(),
            model: v.model.clone(),
            windows: v.windows.iter().map(Into::into).collect(),
            reached: v.reached.clone(),
        }
    }
}
impl From<WireLimitGroup> for core::LimitGroup {
    fn from(v: WireLimitGroup) -> Self {
        Self {
            id: v.id,
            name: v.name,
            model: v.model,
            windows: v.windows.into_iter().map(Into::into).collect(),
            reached: v.reached,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireResetCredit {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}
impl From<&core::ResetCredit> for WireResetCredit {
    fn from(v: &core::ResetCredit) -> Self {
        Self {
            id: v.id.clone(),
            title: v.title.clone(),
            description: v.description.clone(),
            status: v.status.clone(),
            expires_at: v.expires_at.clone(),
        }
    }
}
impl From<WireResetCredit> for core::ResetCredit {
    fn from(v: WireResetCredit) -> Self {
        Self {
            id: v.id,
            title: v.title,
            description: v.description,
            status: v.status,
            expires_at: v.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRateLimits {
    pub windows: Vec<WireRateWindow>,
    pub credits: Option<WireCredits>,
    pub plan: Option<String>,
    pub reached: Option<String>,
    #[serde(default)]
    pub by_limit: Vec<WireLimitGroup>,
    #[serde(default)]
    pub reset_credits: Option<Vec<WireResetCredit>>,
}
impl From<&core::RateLimits> for WireRateLimits {
    fn from(v: &core::RateLimits) -> Self {
        Self {
            windows: v.windows.iter().map(Into::into).collect(),
            credits: v.credits.as_ref().map(Into::into),
            plan: v.plan.clone(),
            reached: v.reached.clone(),
            by_limit: v.by_limit.iter().map(Into::into).collect(),
            reset_credits: v
                .reset_credits
                .as_ref()
                .map(|xs| xs.iter().map(Into::into).collect()),
        }
    }
}
impl From<WireRateLimits> for core::RateLimits {
    fn from(v: WireRateLimits) -> Self {
        Self {
            windows: v.windows.into_iter().map(Into::into).collect(),
            credits: v.credits.map(Into::into),
            plan: v.plan,
            reached: v.reached,
            by_limit: v.by_limit.into_iter().map(Into::into).collect(),
            reset_credits: v
                .reset_credits
                .map(|xs| xs.into_iter().map(Into::into).collect()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum WireSessionEventKind {
    Started {
        session_id: String,
    },
    TurnStarted {
        turn_id: Option<String>,
    },
    Output {
        stream: WireOutputStream,
        text: String,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: Option<String>,
        status: WireToolStatus,
    },
    ToolCallUpdated {
        tool_call_id: String,
        title: Option<String>,
        status: Option<WireToolStatus>,
    },
    ToolOutput {
        tool_call_id: String,
        text: String,
    },
    ToolDetail {
        tool_call_id: String,
        detail: WireToolDetail,
    },
    TurnDiff {
        turn_id: Option<String>,
        diff: String,
    },
    Usage {
        used: u64,
        size: u64,
        cost_usd: Option<f64>,
        tokens: Option<WireTokenUsage>,
    },
    RateLimits {
        limits: WireRateLimits,
    },
    ContextBreakdown {
        parts: Vec<WireContextPart>,
    },
    PermissionRequested {
        request: WirePermissionRequest,
        decision: WirePermissionDecision,
    },
    TurnCompleted {
        turn_id: Option<String>,
        stop_reason: WireStopReason,
    },
    SteerRead {
        message_id: MessageId,
    },
    Compacted,
    PromptViaFile,
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
    Exited {
        message: String,
        cause: Option<WireExitCause>,
        code: Option<i32>,
    },
    #[serde(other)]
    Unknown,
}
impl From<&core::SessionEventKind> for WireSessionEventKind {
    fn from(v: &core::SessionEventKind) -> Self {
        match v {
            core::SessionEventKind::Started { session_id } => Self::Started {
                session_id: session_id.clone(),
            },
            core::SessionEventKind::TurnStarted { turn_id } => Self::TurnStarted {
                turn_id: turn_id.clone(),
            },
            core::SessionEventKind::Output { stream, text } => Self::Output {
                stream: stream.into(),
                text: text.clone(),
            },
            core::SessionEventKind::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
            } => Self::ToolCall {
                tool_call_id: tool_call_id.clone(),
                title: title.clone(),
                kind: kind.clone(),
                status: status.into(),
            },
            core::SessionEventKind::ToolCallUpdated {
                tool_call_id,
                title,
                status,
            } => Self::ToolCallUpdated {
                tool_call_id: tool_call_id.clone(),
                title: title.clone(),
                status: status.as_ref().map(Into::into),
            },
            core::SessionEventKind::ToolOutput { tool_call_id, text } => Self::ToolOutput {
                tool_call_id: tool_call_id.clone(),
                text: text.clone(),
            },
            core::SessionEventKind::ToolDetail {
                tool_call_id,
                detail,
            } => Self::ToolDetail {
                tool_call_id: tool_call_id.clone(),
                detail: detail.into(),
            },
            core::SessionEventKind::TurnDiff { turn_id, diff } => Self::TurnDiff {
                turn_id: turn_id.clone(),
                diff: diff.clone(),
            },
            core::SessionEventKind::Usage {
                used,
                size,
                cost_usd,
                tokens,
            } => Self::Usage {
                used: used.clone(),
                size: size.clone(),
                cost_usd: cost_usd.clone(),
                tokens: tokens.as_ref().map(Into::into),
            },
            core::SessionEventKind::RateLimits { limits } => Self::RateLimits {
                limits: limits.into(),
            },
            core::SessionEventKind::ContextBreakdown { parts } => Self::ContextBreakdown {
                parts: parts.iter().map(Into::into).collect(),
            },
            core::SessionEventKind::PermissionRequested { request, decision } => {
                Self::PermissionRequested {
                    request: request.into(),
                    decision: decision.into(),
                }
            }
            core::SessionEventKind::TurnCompleted {
                turn_id,
                stop_reason,
            } => Self::TurnCompleted {
                turn_id: turn_id.clone(),
                stop_reason: stop_reason.into(),
            },
            core::SessionEventKind::SteerRead { message_id } => Self::SteerRead {
                message_id: message_id.clone(),
            },
            core::SessionEventKind::Compacted => Self::Compacted,
            core::SessionEventKind::PromptViaFile => Self::PromptViaFile,
            core::SessionEventKind::Warning { message } => Self::Warning {
                message: message.clone(),
            },
            core::SessionEventKind::Error { message } => Self::Error {
                message: message.clone(),
            },
            core::SessionEventKind::Exited {
                message,
                cause,
                code,
            } => Self::Exited {
                message: message.clone(),
                cause: cause.as_ref().map(Into::into),
                code: code.clone(),
            },
        }
    }
}
impl From<WireSessionEventKind> for Option<core::SessionEventKind> {
    fn from(v: WireSessionEventKind) -> Self {
        Some(match v {
            WireSessionEventKind::Started { session_id } => core::SessionEventKind::Started {
                session_id: session_id,
            },
            WireSessionEventKind::TurnStarted { turn_id } => {
                core::SessionEventKind::TurnStarted { turn_id: turn_id }
            }
            WireSessionEventKind::Output { stream, text } => core::SessionEventKind::Output {
                stream: stream.into(),
                text: text,
            },
            WireSessionEventKind::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
            } => core::SessionEventKind::ToolCall {
                tool_call_id: tool_call_id,
                title: title,
                kind: kind,
                status: status.into(),
            },
            WireSessionEventKind::ToolCallUpdated {
                tool_call_id,
                title,
                status,
            } => core::SessionEventKind::ToolCallUpdated {
                tool_call_id: tool_call_id,
                title: title,
                status: status.map(Into::into),
            },
            WireSessionEventKind::ToolOutput { tool_call_id, text } => {
                core::SessionEventKind::ToolOutput {
                    tool_call_id: tool_call_id,
                    text: text,
                }
            }
            WireSessionEventKind::ToolDetail {
                tool_call_id,
                detail,
            } => core::SessionEventKind::ToolDetail {
                tool_call_id: tool_call_id,
                detail: detail.into(),
            },
            WireSessionEventKind::TurnDiff { turn_id, diff } => core::SessionEventKind::TurnDiff {
                turn_id: turn_id,
                diff: diff,
            },
            WireSessionEventKind::Usage {
                used,
                size,
                cost_usd,
                tokens,
            } => core::SessionEventKind::Usage {
                used: used,
                size: size,
                cost_usd: cost_usd,
                tokens: tokens.map(Into::into),
            },
            WireSessionEventKind::RateLimits { limits } => core::SessionEventKind::RateLimits {
                limits: limits.into(),
            },
            WireSessionEventKind::ContextBreakdown { parts } => {
                core::SessionEventKind::ContextBreakdown {
                    parts: parts.into_iter().map(Into::into).collect(),
                }
            }
            WireSessionEventKind::PermissionRequested { request, decision } => {
                core::SessionEventKind::PermissionRequested {
                    request: request.into(),
                    decision: decision.into(),
                }
            }
            WireSessionEventKind::TurnCompleted {
                turn_id,
                stop_reason,
            } => core::SessionEventKind::TurnCompleted {
                turn_id: turn_id,
                stop_reason: stop_reason.into(),
            },
            WireSessionEventKind::SteerRead { message_id } => core::SessionEventKind::SteerRead {
                message_id: message_id,
            },
            WireSessionEventKind::Compacted => core::SessionEventKind::Compacted,
            WireSessionEventKind::PromptViaFile => core::SessionEventKind::PromptViaFile,
            WireSessionEventKind::Warning { message } => {
                core::SessionEventKind::Warning { message: message }
            }
            WireSessionEventKind::Error { message } => {
                core::SessionEventKind::Error { message: message }
            }
            WireSessionEventKind::Exited {
                message,
                cause,
                code,
            } => core::SessionEventKind::Exited {
                message: message,
                cause: cause.map(Into::into),
                code: code,
            },
            WireSessionEventKind::Unknown => return None,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireSessionEvent {
    pub kind: WireSessionEventKind,
    pub raw: Option<serde_json::Value>,
}
impl From<&core::SessionEvent> for WireSessionEvent {
    fn from(v: &core::SessionEvent) -> Self {
        Self {
            kind: (&v.kind).into(),
            raw: v.raw.clone(),
        }
    }
}
impl From<WireSessionEvent> for Option<core::SessionEvent> {
    fn from(v: WireSessionEvent) -> Self {
        Some(core::SessionEvent {
            kind: Option::<core::SessionEventKind>::from(v.kind)?,
            raw: v.raw,
        })
    }
}
