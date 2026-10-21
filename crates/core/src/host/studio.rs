//! Studio calls clone a provider out of the actor; inference never blocks it.

use super::{
    App, Duration, GatewayRoute, GatewayUsage, HostRequest, HostState, Request, Result, TcpStream,
    ask_actor, io, log_gateway_usage, openrouter, public_model_id, respond_error, respond_json,
};
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
    let Ok(req) = serde_json::from_slice::<crate::deck::feedback::Request>(&request.body) else {
        return respond_error(stream, 400, "invalid feedback request").await;
    };
    if let Err(errors) = req.check() {
        return respond_json(
            stream,
            400,
            &serde_json::json!({"error":"invalid feedback request","errors":errors}),
        )
        .await;
    }
    match tokio::time::timeout(Duration::from_secs(120), run_feedback(req, state)).await {
        Ok(Ok(response)) => respond_json(stream, 200, &response).await,
        Ok(Err(error)) => respond_error(stream, 502, error).await,
        Err(_) => respond_error(stream, 504, "feedback timed out").await,
    }
}

async fn run_feedback(
    req: crate::deck::feedback::Request,
    state: &HostState,
) -> Result<crate::deck::feedback::Response, &'static str> {
    use crate::deck::feedback;
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
        ChatMessage::text("user", feedback::prompt_context(&req)),
    ];
    for round in 0..2 {
        let completion = call
            .provider
            .complete_with_usage(&call.route.model, messages.clone())
            .await
            .map_err(|_| "feedback provider failed")?;
        log_gateway_usage(
            state,
            call.route.clone(),
            GatewayUsage {
                usage: completion.usage,
                ..GatewayUsage::default()
            },
        )
        .await;
        let (mut response, errors) = feedback::prepare(&req, &completion.text);
        if errors.is_empty() || round == 1 {
            response.model = call.name;
            if !errors.is_empty() {
                response.notes.push(format!(
                    "Dropped invalid feedback: {}",
                    errors
                        .iter()
                        .map(|e| format!("{}: {}", e.path, e.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
            }
            return Ok(response);
        }
        messages.push(ChatMessage::text("assistant", completion.text));
        messages.push(ChatMessage::text(
            "user",
            format!(
                "Correct these errors against the original deck. Return corrected JSON only.\n{}",
                errors
                    .iter()
                    .map(|e| format!("{}: {}", e.path, e.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ));
    }
    unreachable!("two rounds always return")
}

#[cfg(test)]
#[path = "studio_feedback_tests.rs"]
mod tests;
