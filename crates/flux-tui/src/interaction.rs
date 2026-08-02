//! Typed user-interaction channel and its surface-owned presentation state.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use flux_runtime::{
    InteractionCapabilities, InteractionInputMode, InteractionResponse, UserInteraction,
    UserInteractionRequest,
};

pub(crate) type PendingInteraction = (UserInteractionRequest, oneshot::Sender<InteractionResponse>);

#[derive(Debug)]
struct QueuedInteraction {
    id: u64,
    request: UserInteractionRequest,
    reply: oneshot::Sender<InteractionResponse>,
}

/// A bounded bridge from a running `user.ask` operation into the TUI event loop.
#[derive(Debug)]
pub struct InteractionQueue {
    pending: Arc<Mutex<VecDeque<QueuedInteraction>>>,
    next_id: AtomicU64,
}

impl InteractionQueue {
    /// Create the responder before the engine is assembled, so registration and delivery use the
    /// same capability handle.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: Arc::new(Mutex::new(VecDeque::new())),
            next_id: AtomicU64::new(1),
        })
    }

    pub(crate) fn pop(&self) -> Option<PendingInteraction> {
        self.pending
            .lock()
            .unwrap()
            .pop_front()
            .map(|pending| (pending.request, pending.reply))
    }
}

#[async_trait]
impl UserInteraction for InteractionQueue {
    fn capabilities(&self) -> InteractionCapabilities {
        InteractionCapabilities::text()
    }

    async fn request(
        &self,
        request: UserInteractionRequest,
    ) -> flux_core::Result<InteractionResponse> {
        let (reply, response) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut pending = self.pending.lock().unwrap();
            if pending.len() >= 8 {
                return Err(flux_core::Error::Other(
                    "the TUI already has too many queued user interactions".into(),
                ));
            }
            pending.push_back(QueuedInteraction { id, request, reply });
        }
        let _remove_if_cancelled = RemoveQueuedOnDrop {
            pending: self.pending.clone(),
            id,
        };
        response.await.map_err(|_| {
            flux_core::Error::Other(
                "the TUI interaction surface closed before the question was answered".into(),
            )
        })
    }
}

struct RemoveQueuedOnDrop {
    pending: Arc<Mutex<VecDeque<QueuedInteraction>>>,
    id: u64,
}

impl Drop for RemoveQueuedOnDrop {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap()
            .retain(|pending| pending.id != self.id);
    }
}

#[derive(Debug, Clone)]
pub(crate) enum InteractionControl {
    Boolean,
    Single(Vec<serde_json::Value>),
    Multi(Vec<serde_json::Value>),
    Form(Vec<FormField>),
    Json,
}

#[derive(Debug, Clone)]
pub(crate) struct FormField {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) description: Option<String>,
    pub(crate) required: bool,
    pub(crate) control: FormFieldControl,
    pub(crate) default: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub(crate) enum FormFieldControl {
    Boolean,
    Single(Vec<serde_json::Value>),
    Multi(Vec<serde_json::Value>),
    String,
    Integer,
    Number,
}

/// Draw/edit state for one interaction. The reply sender remains event-loop-owned.
#[derive(Debug, Clone)]
pub(crate) struct InteractionView {
    pub(crate) request: UserInteractionRequest,
    pub(crate) control: InteractionControl,
    pub(crate) selected: usize,
    pub(crate) checked: Vec<bool>,
    pub(crate) input: String,
    pub(crate) form_values: Vec<Option<serde_json::Value>>,
    pub(crate) form_cursors: Vec<usize>,
    pub(crate) form_checked: Vec<Vec<bool>>,
    pub(crate) form_inputs: Vec<String>,
    pub(crate) error: Option<String>,
}

impl InteractionView {
    pub(crate) fn new(request: UserInteractionRequest) -> Self {
        let control = classify(&request.schema);
        let checked = match &control {
            InteractionControl::Multi(options) => vec![false; options.len()],
            _ => Vec::new(),
        };
        let (form_values, form_cursors, form_checked, form_inputs) = match &control {
            InteractionControl::Form(fields) => (
                fields.iter().map(|field| field.default.clone()).collect(),
                fields
                    .iter()
                    .map(|field| match (&field.control, &field.default) {
                        (FormFieldControl::Single(options), Some(default)) => options
                            .iter()
                            .position(|option| option == default)
                            .unwrap_or(0),
                        _ => 0,
                    })
                    .collect(),
                fields
                    .iter()
                    .map(|field| match &field.control {
                        FormFieldControl::Multi(options) => {
                            let defaults =
                                field.default.as_ref().and_then(serde_json::Value::as_array);
                            options
                                .iter()
                                .map(|option| defaults.is_some_and(|set| set.contains(option)))
                                .collect()
                        }
                        _ => Vec::new(),
                    })
                    .collect(),
                fields
                    .iter()
                    .map(|field| {
                        field
                            .default
                            .as_ref()
                            .map(|value| match value {
                                serde_json::Value::String(text) => text.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_default()
                    })
                    .collect(),
            ),
            _ => (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        Self {
            request,
            control,
            selected: 0,
            checked,
            input: String::new(),
            form_values,
            form_cursors,
            form_checked,
            form_inputs,
            error: None,
        }
    }

    pub(crate) fn response(value: serde_json::Value) -> InteractionResponse {
        InteractionResponse::Submitted {
            value,
            input_mode: InteractionInputMode::Controls,
        }
    }

    pub(crate) fn form_value(&self) -> Result<serde_json::Value, String> {
        let InteractionControl::Form(fields) = &self.control else {
            return Err("interaction is not a form".into());
        };
        let mut object = serde_json::Map::new();
        for (index, field) in fields.iter().enumerate() {
            let value = match field.control {
                FormFieldControl::String => (!self.form_inputs[index].is_empty())
                    .then(|| serde_json::Value::String(self.form_inputs[index].clone()))
                    .or_else(|| self.form_values[index].clone()),
                FormFieldControl::Integer => {
                    if self.form_inputs[index].is_empty() {
                        self.form_values[index].clone()
                    } else {
                        Some(
                            self.form_inputs[index]
                                .parse::<i64>()
                                .map(serde_json::Value::from)
                                .map_err(|_| format!("{} must be an integer", field.label))?,
                        )
                    }
                }
                FormFieldControl::Number => {
                    if self.form_inputs[index].is_empty() {
                        self.form_values[index].clone()
                    } else {
                        let number = self.form_inputs[index]
                            .parse::<f64>()
                            .ok()
                            .and_then(serde_json::Number::from_f64)
                            .ok_or_else(|| format!("{} must be a finite number", field.label))?;
                        Some(number.into())
                    }
                }
                _ => self.form_values[index].clone(),
            };
            if let Some(value) = value {
                object.insert(field.name.clone(), value);
            }
        }
        Ok(serde_json::Value::Object(object))
    }
}

pub(crate) fn classify(schema: &serde_json::Value) -> InteractionControl {
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("boolean") {
        return InteractionControl::Boolean;
    }
    if let Some(options) = enum_values(schema) {
        return InteractionControl::Single(options);
    }
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("array")
        && schema
            .get("uniqueItems")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        if let Some(options) = schema.get("items").and_then(enum_values) {
            return InteractionControl::Multi(options);
        }
    }
    if let Some(fields) = form_fields(schema) {
        return InteractionControl::Form(fields);
    }
    InteractionControl::Json
}

fn form_fields(schema: &serde_json::Value) -> Option<Vec<FormField>> {
    if schema.get("type")?.as_str()? != "object" {
        return None;
    }
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let properties = schema.get("properties")?.as_object()?;
    let fields = properties
        .iter()
        .map(|(name, schema)| {
            let control =
                if schema.get("type").and_then(serde_json::Value::as_str) == Some("boolean") {
                    FormFieldControl::Boolean
                } else if let Some(options) = enum_values(schema) {
                    FormFieldControl::Single(options)
                } else if schema.get("type").and_then(serde_json::Value::as_str) == Some("array")
                    && schema
                        .get("uniqueItems")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                {
                    FormFieldControl::Multi(enum_values(schema.get("items")?)?)
                } else {
                    match schema.get("type").and_then(serde_json::Value::as_str)? {
                        "string" => FormFieldControl::String,
                        "integer" => FormFieldControl::Integer,
                        "number" => FormFieldControl::Number,
                        _ => return None,
                    }
                };
            Some(FormField {
                name: name.clone(),
                label: schema
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(name)
                    .to_string(),
                description: schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                required: required.contains(name.as_str()),
                control,
                default: schema.get("default").cloned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!fields.is_empty()).then_some(fields)
}

fn enum_values(schema: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    schema
        .get("enum")
        .and_then(serde_json::Value::as_array)
        .filter(|values| !values.is_empty())
        .cloned()
        .or_else(|| {
            schema
                .get("oneOf")?
                .as_array()?
                .iter()
                .map(|branch| branch.get("const").cloned())
                .collect::<Option<Vec<_>>>()
                .filter(|values| !values.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn common_schemas_select_native_controls_and_unknown_shapes_fall_back() {
        assert!(matches!(
            classify(&json!({"type":"boolean"})),
            InteractionControl::Boolean
        ));
        assert!(matches!(
            classify(&json!({"enum":["a","b"]})),
            InteractionControl::Single(_)
        ));
        assert!(matches!(
            classify(&json!({
                "type":"array", "uniqueItems":true, "items":{"enum":["a","b"]}
            })),
            InteractionControl::Multi(_)
        ));
        assert!(matches!(
            classify(&json!({"type":"object","properties":{"x":{"type":"string"}}})),
            InteractionControl::Form(_)
        ));
        assert!(matches!(
            classify(&json!({
                "type":"object",
                "properties":{"nested":{"type":"object","properties":{}}}
            })),
            InteractionControl::Json
        ));
    }

    #[tokio::test]
    async fn queue_waits_for_an_explicit_surface_response() {
        let queue = InteractionQueue::new();
        let worker = {
            let queue = queue.clone();
            tokio::spawn(async move {
                queue
                    .request(UserInteractionRequest {
                        origin: flux_runtime::InteractionOrigin::Agent,
                        prompt: flux_runtime::UserPrompt::text("continue?"),
                        schema: json!({"type":"boolean"}),
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        let (_, reply) = queue.pop().expect("queued request");
        reply
            .send(InteractionResponse::Submitted {
                value: json!(true),
                input_mode: InteractionInputMode::Controls,
            })
            .unwrap();
        assert_eq!(
            worker.await.unwrap().unwrap(),
            InteractionResponse::Submitted {
                value: json!(true),
                input_mode: InteractionInputMode::Controls,
            }
        );
    }

    #[tokio::test]
    async fn cancelling_the_request_future_removes_a_queued_question() {
        let queue = InteractionQueue::new();
        let worker = {
            let queue = queue.clone();
            tokio::spawn(async move {
                queue
                    .request(UserInteractionRequest {
                        origin: flux_runtime::InteractionOrigin::Agent,
                        prompt: flux_runtime::UserPrompt::text("continue?"),
                        schema: json!({"type":"boolean"}),
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        worker.abort();
        let _ = worker.await;
        assert!(queue.pop().is_none());
    }
}
