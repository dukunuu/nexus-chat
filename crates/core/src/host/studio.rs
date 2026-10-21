//! Studio calls clone a provider out of the actor; inference never blocks it.

use super::{
    App, Duration, GatewayRoute, GatewayUsage, HostRequest, HostState, Request, Result, TcpStream,
    ask_actor, io, log_gateway_usage, openrouter, public_model_id, respond_error, respond_json,
};
use crate::deck::suggest::{self, SuggestRequest, SuggestResponse};
use crate::provider::ChatMessage;

pub(super) struct ModelCall {
    provider: openrouter::OpenRouter,
    route: GatewayRoute,
    name: String,
}

pub(super) fn select_model(app: &App, requested: Option<&str>) -> Result<ModelCall, String> {
    let name = requested
        .or(app.current_model.as_deref())
        .map(str::to_owned)
        .or_else(|| {
            app.models
                .iter()
                .find(|m| app.backends.configured(m.backend))
                .map(crate::app::composite_id)
        })
        .ok_or_else(|| "no chat model selected".to_string())?;
    let resolved = app
        .models
        .iter()
        .find(|m| m.id == name || crate::app::composite_id(m) == name || public_model_id(m) == name)
        .map_or_else(|| name.clone(), crate::app::composite_id);
    let (provider, model) = app
        .resolve_model_backend(&resolved)
        .ok_or_else(|| "model unavailable".to_string())?;
    let route = GatewayRoute {
        tag: provider.backend_tag(),
        model,
        key: String::new(),
        account_id: None,
    };
    Ok(ModelCall {
        provider,
        route,
        name: resolved,
    })
}

pub(super) async fn handle(
    stream: &mut TcpStream,
    request: &Request,
    state: &HostState,
) -> io::Result<()> {
    let Ok(req) = serde_json::from_slice::<SuggestRequest>(&request.body) else {
        return respond_error(stream, 400, "invalid suggestion request").await;
    };
    if let Err(errors) = req.check() {
        return respond_json(
            stream,
            400,
            &serde_json::json!({"error":"invalid suggestion request", "errors":errors}),
        )
        .await;
    }
    match tokio::time::timeout(Duration::from_secs(120), run(req, state)).await {
        Ok(Ok(response)) if response.suggestions.is_empty() => {
            respond_json(stream, 422, &response).await
        }
        Ok(Ok(response)) => respond_json(stream, 200, &response).await,
        Ok(Err(error)) => respond_error(stream, 502, error).await,
        Err(_) => respond_error(stream, 504, "suggestion timed out").await,
    }
}

async fn run(req: SuggestRequest, state: &HostState) -> Result<SuggestResponse, &'static str> {
    let call = ask_actor(&state.requests, |reply| HostRequest::StudioModel {
        model: req.model.clone(),
        reply,
    })
    .await
    .map_err(|_| "model unavailable")?;
    let mut messages = vec![
        ChatMessage::text(
            "system",
            include_str!("../../assets/prompts/deck-suggest.md"),
        ),
        ChatMessage::text(
            "user",
            format!(
                "{}\nInstruction: {}",
                suggest::prompt_context(&req),
                req.instruction
            ),
        ),
    ];
    let mut notes = Vec::new();
    for round in 0..2 {
        let completion = call
            .provider
            .complete_with_usage(&call.route.model, messages.clone())
            .await
            .map_err(|_| "suggestion provider failed")?;
        log_gateway_usage(
            state,
            call.route.clone(),
            GatewayUsage {
                usage: completion.usage,
                ..GatewayUsage::default()
            },
        )
        .await;
        let (suggestions, errors, count) = match suggest::parse_reply(&completion.text) {
            Ok(raw) => {
                let count = raw.len();
                let (suggestions, errors) = suggest::prepare(&req.deck, raw);
                (suggestions, errors, count)
            }
            Err(error) => (
                Vec::new(),
                vec![crate::deck::CheckError {
                    path: "reply".into(),
                    message: error,
                }],
                1,
            ),
        };
        if errors.is_empty() || round == 1 {
            let dropped = count.saturating_sub(suggestions.len());
            if dropped > 0 {
                notes.push(format!(
                    "dropped {dropped} suggestion{} that didn't apply",
                    if dropped == 1 { "" } else { "s" }
                ));
            }
            if suggestions.is_empty() && notes.is_empty() {
                notes.push("no valid suggestions".into());
            }
            return Ok(SuggestResponse {
                suggestions,
                model: call.name,
                revision: req.revision,
                notes,
            });
        }
        messages.push(ChatMessage::text("assistant", completion.text));
        let located = errors
            .iter()
            .map(|e| format!("{}: {}", e.path, e.message))
            .collect::<Vec<_>>()
            .join("\n");
        messages.push(ChatMessage::text("user", format!("Correct these errors against the original deck. Return corrected JSON only.\n{located}")));
    }
    unreachable!("two-round loop always returns")
}

#[cfg(test)]
#[path = "studio_tests.rs"]
mod tests;
