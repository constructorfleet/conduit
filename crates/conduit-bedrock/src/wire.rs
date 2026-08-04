//! Mapping Conduit's vocabulary onto the Converse API's.
//!
//! Kept separate from the provider so the translation is in one place and
//! readable on its own, as it is for every other vendor here. What makes this
//! one more than a rename is the shape of a conversation: Converse requires the
//! turns to alternate between user and assistant, starting with a user turn, and
//! Conduit's history does not — a memory recall, a tool result, and a spoken
//! utterance can arrive as three consecutive user-side messages. Sent as they
//! are, that is a `ValidationException` rather than a conversation.

use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message as WireMessage, StopReason,
    SystemContentBlock, Tool, ToolConfiguration, ToolInputSchema, ToolSpecification,
};
use aws_smithy_types::error::operation::BuildError;
use aws_smithy_types::Document;
use conduit_core::event::FinishReason;
use conduit_provider::llm::{CompletionRequest, Message, Role, ToolSpec};

use crate::document;

/// A streaming Converse request, in the pieces the fluent builder takes.
///
/// Assembled here rather than against the client, so the mapping can be tested
/// without an endpoint to send it to.
#[derive(Debug, Clone)]
pub struct Request {
    /// Model, inference profile, or ARN, passed through untouched.
    pub model: String,
    /// Conversation history, alternating, oldest first.
    pub messages: Vec<WireMessage>,
    /// Instructions framing the conversation, as the API's own top-level field.
    pub system: Vec<SystemContentBlock>,
    /// Tools the model may call, absent when there are none.
    ///
    /// A `ToolConfiguration` naming an empty tool list is refused by its own
    /// builder, so "no tools" has to be the absence of the field rather than an
    /// empty one.
    pub tools: Option<ToolConfiguration>,
    /// The controls every model shares: token cap and sampling.
    pub inference: InferenceConfiguration,
    /// Provider-specific settings, as the API's escape hatch for them.
    ///
    /// Converse has no room for arbitrary top-level fields the way the Messages
    /// API does, and `additionalModelRequestFields` is where a model-specific
    /// control — `top_k`, `thinking`, `anthropic_beta` — is meant to go.
    pub additional: Option<Document>,
}

impl Request {
    /// Translates a Conduit request into the Converse API's shape.
    ///
    /// `defaults` are the Configured Provider's stored settings; the request's
    /// own settings override them, so a pipeline can still overrule a default.
    ///
    /// `system` is the provider's configured prompt, used when the history
    /// carries no system message of its own.
    ///
    /// # Errors
    ///
    /// Returns [`BuildError`] if a message or a tool specification is one the
    /// SDK will not assemble. Nothing is sent in that case, which is what the
    /// caller reports.
    pub fn from_completion(
        request: CompletionRequest,
        defaults: &serde_json::Map<String, serde_json::Value>,
        system: Option<&str>,
    ) -> std::result::Result<Self, BuildError> {
        let (history, from_messages) = split_system(request.messages);
        // A system message in the history is the runtime's per-turn framing —
        // the graph's prompt and anything recalled from memory — and it is more
        // specific than the provider's standing prompt.
        let system = from_messages.or_else(|| system.map(str::to_owned));

        let mut tools = Vec::new();
        for spec in request.tools {
            tools.push(tool(spec)?);
        }

        let settings = crate::layered_settings(defaults, request.settings.as_map());

        Ok(Self {
            model: request.model,
            messages: conversation(history)?,
            system: system.map(SystemContentBlock::Text).into_iter().collect(),
            tools: match tools.is_empty() {
                true => None,
                false => Some(ToolConfiguration::builder().set_tools(Some(tools)).build()?),
            },
            inference: inference(request.max_tokens, request.temperature),
            // An empty object would be a field sent for no reason, and some
            // models reject one.
            additional: match settings.is_empty() {
                true => None,
                false => Some(document::from_json(&serde_json::Value::Object(settings))),
            },
        })
    }
}

/// The token cap and sampling controls, as far as the request names them.
///
/// `maxTokens` is optional here, unlike the Messages API where it is required
/// and a default has to be invented: a request that names none gets whatever the
/// model's own limit is, which is a better answer than a number this crate chose.
fn inference(max_tokens: Option<u32>, temperature: Option<f32>) -> InferenceConfiguration {
    let mut builder = InferenceConfiguration::builder();
    if let Some(max_tokens) = max_tokens {
        // Saturating rather than failing: a cap larger than the API's own type
        // can hold is a cap the model would refuse anyway, and the largest
        // expressible one is what the caller meant by it.
        builder = builder.max_tokens(i32::try_from(max_tokens).unwrap_or(i32::MAX));
    }
    if let Some(temperature) = temperature {
        builder = builder.temperature(temperature);
    }
    builder.build()
}

/// One tool, as the API describes one.
fn tool(spec: ToolSpec) -> std::result::Result<Tool, BuildError> {
    Ok(Tool::ToolSpec(
        ToolSpecification::builder()
            .name(spec.name)
            .description(spec.description)
            // Named `inputSchema` here, where the chat completions API calls it
            // `parameters`, and carried as the SDK's own document tree rather
            // than as JSON.
            .input_schema(ToolInputSchema::Json(document::from_json(&spec.parameters)))
            .build()?,
    ))
}

/// Separates system framing from the conversation.
///
/// Every system message is lifted out and joined: the API takes one system field
/// and the runtime may contribute several, since the graph's prompt and a memory
/// recall arrive as separate messages.
fn split_system(messages: Vec<Message>) -> (Vec<Message>, Option<String>) {
    let mut system = Vec::new();
    let mut history = Vec::new();
    for message in messages {
        match message.role {
            Role::System => system.push(message.content),
            _ => history.push(message),
        }
    }

    (history, (!system.is_empty()).then(|| system.join("\n\n")))
}

/// Turns Conduit's history into an alternating conversation.
///
/// Three things happen here, and each is a `ValidationException` avoided:
/// consecutive turns on the same side are joined, because the API requires the
/// roles to alternate; a leading assistant turn is dropped, because a
/// conversation starts with a user; and a trailing one is dropped too, because
/// the API reads it as a prefill of the reply, which is not what the runtime
/// meant by recording what the assistant said before a tool ran.
fn conversation(messages: Vec<Message>) -> std::result::Result<Vec<WireMessage>, BuildError> {
    let mut turns: Vec<(ConversationRole, String)> = Vec::new();
    for message in messages {
        let role = match message.role {
            Role::Assistant => ConversationRole::Assistant,
            // A tool result belongs to the user turn in this API, and it would
            // ideally be a `toolResult` block naming the `toolUse` it answers.
            // It cannot be: Conduit's history keeps the result and the id but
            // not the block that requested it, and a `toolResult` without its
            // matching `toolUse` is rejected. Sent as the text it is, the model
            // reads the answer without the pairing.
            Role::User | Role::Tool | Role::System => ConversationRole::User,
        };

        match turns.last_mut() {
            Some((last, content)) if *last == role => {
                content.push_str("\n\n");
                content.push_str(&message.content);
            }
            _ => turns.push((role, message.content)),
        }
    }

    while turns.last().is_some_and(|(role, _)| *role == ConversationRole::Assistant) {
        tracing::debug!("dropping a trailing assistant turn, which the API reads as a prefill");
        turns.pop();
    }
    if turns.first().is_some_and(|(role, _)| *role == ConversationRole::Assistant) {
        tracing::debug!("dropping a leading assistant turn: a conversation starts with a user");
        turns.remove(0);
    }

    turns
        .into_iter()
        .map(|(role, content)| {
            WireMessage::builder().role(role).content(ContentBlock::Text(content)).build()
        })
        .collect()
}

/// Maps a Converse stop reason onto Conduit's.
///
/// Matched on the string rather than the variant: the enum is
/// `#[non_exhaustive]` and matching its `Unknown` arm is deprecated, so the
/// name the API sent is the stable thing to read. An unfamiliar reason becomes
/// [`FinishReason::Stop`], because the response did end and inventing a more
/// specific meaning would be a guess.
#[must_use]
pub fn finish_reason(reason: &StopReason) -> FinishReason {
    match reason.as_str() {
        "tool_use" => FinishReason::ToolUse,
        // Two ways to run out of room: the cap the request named, and the
        // model's own window.
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        // The response was stopped by something other than the model, which is
        // the same thing a refusal is.
        "content_filtered" | "guardrail_intervened" => FinishReason::Cancelled,
        _ => FinishReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::id::ToolCallId;

    fn request(messages: Vec<Message>) -> Request {
        Request::from_completion(
            CompletionRequest::new("us.anthropic.claude-opus-4-5-20251101-v1:0", messages),
            &serde_json::Map::new(),
            None,
        )
        .expect("builds")
    }

    /// The text of each turn, in order, tagged with its role.
    fn turns(request: &Request) -> Vec<(&'static str, String)> {
        request
            .messages
            .iter()
            .map(|message| {
                let role = match message.role() {
                    ConversationRole::Assistant => "assistant",
                    _ => "user",
                };
                let text = message
                    .content()
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text(text) => text.clone(),
                        _ => panic!("this crate sends text blocks"),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                (role, text)
            })
            .collect()
    }

    #[test]
    fn a_system_message_becomes_the_top_level_field() {
        let body = request(vec![Message::system("Be terse."), Message::user("hi")]);

        assert_eq!(body.system.len(), 1);
        assert!(
            matches!(&body.system[0], SystemContentBlock::Text(text) if text == "Be terse."),
            "{:?}",
            body.system
        );
        assert_eq!(turns(&body), [("user", "hi".to_owned())], "only the user turn is history");
    }

    #[test]
    fn several_system_messages_are_joined_rather_than_dropped() {
        // The runtime contributes the graph's prompt and a memory recall as
        // separate system messages, and one field has to hold both.
        let body = request(vec![
            Message::system("Be terse."),
            Message::system("You remember: the kettle is in the kitchen."),
            Message::user("where is it"),
        ]);

        let SystemContentBlock::Text(system) = &body.system[0] else {
            panic!("a text block");
        };
        assert!(system.contains("Be terse."), "{system}");
        assert!(system.contains("kettle"), "{system}");
    }

    #[test]
    fn a_configured_prompt_applies_only_when_the_history_frames_nothing() {
        let with_history = Request::from_completion(
            CompletionRequest::new("model", vec![Message::system("Per turn.")]),
            &serde_json::Map::new(),
            Some("Configured."),
        )
        .expect("builds");
        assert!(
            matches!(&with_history.system[0], SystemContentBlock::Text(text) if text == "Per turn."),
            "the turn's own framing is more specific than the provider's standing one"
        );

        let without = Request::from_completion(
            CompletionRequest::new("model", vec![Message::user("hi")]),
            &serde_json::Map::new(),
            Some("Configured."),
        )
        .expect("builds");
        assert!(
            matches!(&without.system[0], SystemContentBlock::Text(text) if text == "Configured.")
        );
    }

    #[test]
    fn consecutive_turns_on_the_same_side_are_joined_because_roles_must_alternate() {
        // The failure this exists for: the runtime hands over a recalled fact,
        // a tool result, and what the person said, and the API rejects the lot
        // as three user turns in a row.
        let body = request(vec![
            Message::user("what did I ask for"),
            Message::tool_result(ToolCallId::new("tool_1"), "a kettle"),
            Message::user("and where is it"),
        ]);

        let turns = turns(&body);
        assert_eq!(turns.len(), 1, "one user turn, not three: {turns:?}");
        let (role, text) = &turns[0];
        assert_eq!(role, &"user");
        assert!(text.contains("what did I ask for"), "{text}");
        assert!(text.contains("a kettle"), "{text}");
        assert!(text.contains("and where is it"), "{text}");
    }

    #[test]
    fn an_alternating_conversation_is_left_alone() {
        let body = request(vec![
            Message::user("hi"),
            Message::assistant("hello"),
            Message::user("thanks"),
        ]);

        assert_eq!(
            turns(&body),
            [
                ("user", "hi".to_owned()),
                ("assistant", "hello".to_owned()),
                ("user", "thanks".to_owned()),
            ]
        );
    }

    #[test]
    fn a_trailing_assistant_turn_is_dropped_because_the_api_reads_it_as_a_prefill() {
        // The runtime appends the assistant's own words before tool results, so
        // a history ending on one is reachable — and continuing that text is not
        // what recording it meant.
        let body =
            request(vec![Message::user("hi"), Message::assistant("let me look that up")]);

        assert_eq!(turns(&body), [("user", "hi".to_owned())]);
    }

    #[test]
    fn a_leading_assistant_turn_is_dropped_because_a_conversation_starts_with_a_user() {
        let body = request(vec![
            Message::assistant("anything else?"),
            Message::user("yes"),
            Message::assistant("go on"),
            Message::user("that is all"),
        ]);

        assert_eq!(
            turns(&body),
            [
                ("user", "yes".to_owned()),
                ("assistant", "go on".to_owned()),
                ("user", "that is all".to_owned()),
            ]
        );
    }

    #[test]
    fn no_tools_is_no_tool_configuration_rather_than_an_empty_one() {
        // `ToolConfiguration` refuses to build without tools, so the absence has
        // to be the absent field.
        assert!(request(vec![Message::user("hi")]).tools.is_none());
    }

    #[test]
    fn a_tool_spec_carries_its_schema_as_the_apis_own_document_tree() {
        let body = Request::from_completion(
            CompletionRequest {
                tools: vec![ToolSpec {
                    name: "lights.turn_on".to_owned(),
                    description: "Turns lights on.".to_owned(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": { "room": { "type": "string" } },
                        "required": ["room"],
                    }),
                }],
                ..CompletionRequest::new("model", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        )
        .expect("builds");

        let configuration = body.tools.expect("a tool was offered");
        let [Tool::ToolSpec(spec)] = configuration.tools() else {
            panic!("one tool specification");
        };
        assert_eq!(spec.name(), "lights.turn_on");
        let Some(ToolInputSchema::Json(schema)) = spec.input_schema() else {
            panic!("a JSON schema");
        };
        assert_eq!(
            document::to_json(schema),
            serde_json::json!({
                "type": "object",
                "properties": { "room": { "type": "string" } },
                "required": ["room"],
            }),
            "an argument schema that lost a `required` list would silently widen the tool"
        );
    }

    #[test]
    fn a_token_cap_is_sent_only_when_the_request_names_one() {
        // Unlike the Messages API, `maxTokens` is optional here, so there is no
        // default to invent: the model's own limit is a better answer than a
        // number this crate chose.
        assert_eq!(request(vec![Message::user("hi")]).inference.max_tokens(), None);

        let asked = Request::from_completion(
            CompletionRequest {
                max_tokens: Some(256),
                ..CompletionRequest::new("model", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        )
        .expect("builds");
        assert_eq!(asked.inference.max_tokens(), Some(256));
    }

    #[test]
    fn temperature_reaches_the_inference_configuration() {
        // Unlike the Messages API, which rejects it outright, Converse takes
        // sampling controls in a field of their own.
        let body = Request::from_completion(
            CompletionRequest {
                temperature: Some(0.7),
                ..CompletionRequest::new("model", vec![Message::user("hi")])
            },
            &serde_json::Map::new(),
            None,
        )
        .expect("builds");

        assert_eq!(body.inference.temperature(), Some(0.7));
    }

    #[test]
    fn declared_settings_travel_as_additional_model_request_fields() {
        // Converse has no room for arbitrary top-level fields, so a
        // model-specific control goes in the field the API set aside for it.
        let mut defaults = serde_json::Map::new();
        defaults.insert("top_k".to_owned(), serde_json::json!(10));

        let mut completion = CompletionRequest::new("model", vec![Message::user("hi")]);
        completion.settings =
            serde_json::from_value(serde_json::json!({ "top_k": 40 })).expect("settings");

        let body = Request::from_completion(completion, &defaults, None).expect("builds");

        let additional = body.additional.expect("the setting travels");
        assert_eq!(
            document::to_json(&additional),
            serde_json::json!({ "top_k": 40 }),
            "the request's setting overrides the configured default"
        );
    }

    #[test]
    fn naming_no_settings_sends_no_additional_fields_at_all() {
        // An empty object is a field sent for no reason, and some models refuse
        // one.
        assert!(request(vec![Message::user("hi")]).additional.is_none());
    }

    #[test]
    fn stop_reasons_map_onto_the_vocabulary() {
        assert_eq!(finish_reason(&StopReason::ToolUse), FinishReason::ToolUse);
        assert_eq!(finish_reason(&StopReason::MaxTokens), FinishReason::Length);
        assert_eq!(finish_reason(&StopReason::EndTurn), FinishReason::Stop);
        assert_eq!(finish_reason(&StopReason::StopSequence), FinishReason::Stop);
        assert_eq!(finish_reason(&StopReason::ContentFiltered), FinishReason::Cancelled);
        assert_eq!(finish_reason(&StopReason::GuardrailIntervened), FinishReason::Cancelled);
        assert_eq!(
            finish_reason(&StopReason::ModelContextWindowExceeded),
            FinishReason::Length,
            "running out of window is running out of room"
        );
    }

    #[test]
    fn a_stop_reason_this_build_predates_still_means_the_response_ended() {
        // The enum is non-exhaustive and grows. A reason with no mapping must
        // not be the thing that fails a turn.
        assert_eq!(finish_reason(&StopReason::from("invented_later")), FinishReason::Stop);
    }
}
