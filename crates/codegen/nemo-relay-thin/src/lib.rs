use nemo_relay::api::llm::{LlmCallExecuteParams, LlmRequest, llm_call_execute};
use nemo_relay::api::registry::{
    register_mark_sanitize_guardrail, register_scope_sanitize_end_guardrail,
    register_scope_sanitize_start_guardrail,
};
use nemo_relay::api::runtime::EventSanitizeFn;
use nemo_relay::api::scope::{
    PopScopeParams, PushScopeParams, ScopeAttributes, ScopeHandle, ScopeType, pop_scope, push_scope,
};
use nemo_relay::api::subscriber::{flush_subscribers, register_subscriber};
use nemo_relay::api::tool::{ToolCallExecuteParams, ToolExecutionResult, tool_call_execute};
use nemo_relay::error::FlowError;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

const SUBSCRIBER_NAME: &str = "grok.nemo-relay.lifecycle";
const SANITIZER_PRIORITY: i32 = i32::MIN;
const SESSION_NAME: &str = "grok.session";
const SUBAGENT_NAME: &str = "grok.subagent";
const TURN_NAME: &str = "grok.turn";
const TOOL_NAME: &str = "grok.tool";
const LLM_NAME: &str = "grok.llm";

static INSTALLATION: OnceLock<Result<(), String>> = OnceLock::new();
static SESSIONS: OnceLock<Mutex<HashMap<String, ScopeHandle>>> = OnceLock::new();
static TURNS: OnceLock<Mutex<HashMap<String, ScopeHandle>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, ScopeHandle>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn turns() -> &'static Mutex<HashMap<String, ScopeHandle>> {
    TURNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn content_stripper() -> EventSanitizeFn {
    Arc::new(|_event, mut fields| {
        Box::pin(async move {
            fields.data = None;
            fields.category_profile = None;
            fields.metadata = None;
            Ok(fields)
        })
    })
}

fn is_allowlisted(kind: &str, name: &str) -> bool {
    kind == "scope"
        && matches!(
            name,
            SESSION_NAME | SUBAGENT_NAME | TURN_NAME | TOOL_NAME | LLM_NAME
        )
}

fn install() -> Result<(), String> {
    register_mark_sanitize_guardrail(
        "grok.nemo-relay.strip-mark-content",
        SANITIZER_PRIORITY,
        content_stripper(),
    )
    .map_err(|error| error.to_string())?;
    register_scope_sanitize_start_guardrail(
        "grok.nemo-relay.strip-scope-start-content",
        SANITIZER_PRIORITY,
        content_stripper(),
    )
    .map_err(|error| error.to_string())?;
    register_scope_sanitize_end_guardrail(
        "grok.nemo-relay.strip-scope-end-content",
        SANITIZER_PRIORITY,
        content_stripper(),
    )
    .map_err(|error| error.to_string())?;
    register_subscriber(
        SUBSCRIBER_NAME,
        Arc::new(|event| {
            let kind = event.kind();
            let name = event.name();
            if is_allowlisted(kind, name) && std::env::var_os("NEMO_RELAY_STDOUT").is_some() {
                println!("nemo-relay kind={kind} name={name}");
            }
        }),
    )
    .map_err(|error| error.to_string())
}

pub fn initialize() -> bool {
    INSTALLATION.get_or_init(install).is_ok()
}

pub struct SessionScope {
    session_id: String,
    handle: ScopeHandle,
}

impl SessionScope {
    pub fn start(session_id: impl Into<String>, parent_session_id: Option<&str>) -> Option<Self> {
        if !initialize() {
            return None;
        }
        let session_id = session_id.into();
        let parent = parent_session_id.and_then(|id| sessions().lock().ok()?.get(id).cloned());
        if parent_session_id.is_some() && parent.is_none() {
            return None;
        }
        let name = if parent.is_some() {
            SUBAGENT_NAME
        } else {
            SESSION_NAME
        };
        let handle = push_scope(
            PushScopeParams::builder()
                .name(name)
                .scope_type(ScopeType::Agent)
                .parent_opt(parent.as_ref())
                .attributes(ScopeAttributes::empty())
                .build(),
        )
        .ok()?;
        sessions()
            .lock()
            .ok()?
            .insert(session_id.clone(), handle.clone());
        Some(Self { session_id, handle })
    }
}

impl Drop for SessionScope {
    fn drop(&mut self) {
        if let Ok(mut sessions) = sessions().lock()
            && sessions
                .get(&self.session_id)
                .is_some_and(|handle| handle.uuid == self.handle.uuid)
        {
            sessions.remove(&self.session_id);
        }
        let _ = pop_scope(
            PopScopeParams::builder()
                .handle_uuid(&self.handle.uuid)
                .build(),
        );
    }
}

pub struct TurnScope {
    session_id: String,
    handle: ScopeHandle,
}

impl TurnScope {
    pub fn start(session_id: impl Into<String>) -> Option<Self> {
        if !initialize() {
            return None;
        }
        let session_id = session_id.into();
        let parent = sessions().lock().ok()?.get(&session_id).cloned()?;
        let handle = push_scope(
            PushScopeParams::builder()
                .name(TURN_NAME)
                .scope_type(ScopeType::Function)
                .parent(&parent)
                .attributes(ScopeAttributes::empty())
                .build(),
        )
        .ok()?;
        turns()
            .lock()
            .ok()?
            .insert(session_id.clone(), handle.clone());
        Some(Self { session_id, handle })
    }
}

impl Drop for TurnScope {
    fn drop(&mut self) {
        if let Ok(mut turns) = turns().lock()
            && turns
                .get(&self.session_id)
                .is_some_and(|handle| handle.uuid == self.handle.uuid)
        {
            turns.remove(&self.session_id);
        }
        let _ = pop_scope(
            PopScopeParams::builder()
                .handle_uuid(&self.handle.uuid)
                .build(),
        );
    }
}

fn active_turn(session_id: &str) -> Option<ScopeHandle> {
    turns().lock().ok()?.get(session_id).cloned()
}

pub async fn managed_tool_call<T, E, F>(session_id: &str, future: F) -> Result<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    F: Future<Output = Result<T, E>> + Send + 'static,
{
    let Some(parent) = active_turn(session_id) else {
        return future.await;
    };
    let pending = Arc::new(Mutex::new(Some(Box::pin(future))));
    let output = Arc::new(Mutex::new(None));
    let callback_pending = Arc::clone(&pending);
    let callback_output = Arc::clone(&output);
    let _ = tool_call_execute(
        ToolCallExecuteParams::builder()
            .name(TOOL_NAME)
            .args(json!({}))
            .parent(parent)
            .func(Arc::new(move |_args| {
                let future = callback_pending
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take());
                let output = Arc::clone(&callback_output);
                Box::pin(async move {
                    let Some(future) = future else {
                        return Err(FlowError::Internal(
                            "grok tool callback invoked more than once".to_string(),
                        ));
                    };
                    let result = future.await;
                    let succeeded = result.is_ok();
                    if let Ok(mut slot) = output.lock() {
                        *slot = Some(result);
                    }
                    if succeeded {
                        Ok(ToolExecutionResult::new(json!({})))
                    } else {
                        Err(FlowError::Internal("grok tool callback failed".to_string()))
                    }
                })
            }))
            .build(),
    )
    .await;
    if let Some(result) = output.lock().ok().and_then(|mut slot| slot.take()) {
        return result;
    }
    let fallback = pending.lock().ok().and_then(|mut slot| slot.take());
    match fallback {
        Some(future) => future.await,
        None => panic!("NeMo Relay tool callback completed without returning its shell result"),
    }
}

pub async fn managed_llm_call<T, F, C>(session_id: &str, future: F, succeeded: C) -> T
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
    C: Fn(&T) -> bool + Send + Sync + 'static,
{
    let Some(parent) = active_turn(session_id) else {
        return future.await;
    };
    let pending = Arc::new(Mutex::new(Some(Box::pin(future))));
    let output = Arc::new(Mutex::new(None));
    let callback_pending = Arc::clone(&pending);
    let callback_output = Arc::clone(&output);
    let succeeded = Arc::new(succeeded);
    let _ = llm_call_execute(
        LlmCallExecuteParams::builder()
            .name(LLM_NAME)
            .request(LlmRequest {
                headers: Default::default(),
                content: json!({}),
            })
            .parent(parent)
            .func(Arc::new(move |_request| {
                let future = callback_pending
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take());
                let output = Arc::clone(&callback_output);
                let succeeded = Arc::clone(&succeeded);
                Box::pin(async move {
                    let Some(future) = future else {
                        return Err(FlowError::Internal(
                            "grok LLM callback invoked more than once".to_string(),
                        ));
                    };
                    let result = future.await;
                    let call_succeeded = succeeded(&result);
                    if let Ok(mut slot) = output.lock() {
                        *slot = Some(result);
                    }
                    if call_succeeded {
                        Ok(json!({}))
                    } else {
                        Err(FlowError::Internal("grok LLM callback failed".to_string()))
                    }
                })
            }))
            .build(),
    )
    .await;
    if let Some(result) = output.lock().ok().and_then(|mut slot| slot.take()) {
        return result;
    }
    let fallback = pending.lock().ok().and_then(|mut slot| slot.take());
    match fallback {
        Some(future) => future.await,
        None => panic!("NeMo Relay LLM callback completed without returning its shell result"),
    }
}

pub fn flush() {
    let _ = flush_subscribers();
}
