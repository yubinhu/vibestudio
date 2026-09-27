//! Desktop-only control surface. Long-running work lives in skill-core.
use super::{json_reply, query_param, Reply};
use serde::Deserialize;
use serde_json::json;
use skill_core::comparison::{ComparisonConfig, ComparisonManager, ComparisonUpdate};
use tiny_http::Method;

pub(super) fn unavailable() -> Reply {
    Reply {
        status: 404,
        body: serde_json::to_vec(&json!({"error": "Live UI comparison requires the VibeStudio desktop switchboard on this machine."})).unwrap(),
        content_type: "application/json".into(),
        extra: vec![],
    }
}

#[derive(Deserialize)]
struct UpdateRequest {
    id: String,
    #[serde(flatten)]
    update: ComparisonUpdate,
}

#[derive(Deserialize)]
struct SessionRequest {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRequest {
    id: String,
    #[serde(default)]
    config: Option<ComparisonConfig>,
}

pub(super) fn handle(method: &Method, url: &str, body: &str, manager: Option<&ComparisonManager>) -> Reply {
    let Some(manager) = manager else { return unavailable() };
    let path = url.split('?').next().unwrap_or(url);
    match (method, path) {
        (Method::Get, "/api/comparison/capabilities") => json_reply(Ok(json!({
            "available": true, "protocol": 1, "nativeWebviews": true, "sessionArtifacts": true
        }))),
        (Method::Get, "/api/comparison/devices") => json_reply(Ok(skill_core::comparison_devices::catalog())),
        (Method::Post, "/api/comparison/devices/refresh") => {
            let mut reply = json_reply(Ok(skill_core::comparison_devices::refresh_catalog()));
            reply.status = 202;
            reply
        },
        (Method::Get, "/api/comparison/list") => json_reply(Ok(manager.list())),
        (Method::Get, "/api/comparison/status") => {
            let id = query_param(url, "id").unwrap_or_default();
            json_reply(manager.get(&id).ok_or_else(|| "Comparison session not found.".into()))
        }
        (Method::Post, "/api/comparison/start") => {
            let result = serde_json::from_str::<ComparisonConfig>(body)
                .map_err(|e| format!("Invalid comparison configuration: {e}"))
                .and_then(|config| manager.start(config));
            let mut reply = json_reply(result);
            if reply.status == 200 { reply.status = 202; }
            reply
        }
        (Method::Post, "/api/comparison/update") => json_reply(
            serde_json::from_str::<UpdateRequest>(body)
                .map_err(|e| format!("Invalid comparison update: {e}"))
                .and_then(|request| manager.update(&request.id, request.update)),
        ),
        (Method::Post, "/api/comparison/open") => {
            let mut reply = json_reply(serde_json::from_str::<OpenRequest>(body)
                .map_err(|e| format!("Invalid comparison open request: {e}"))
                .and_then(|request| manager.open(&request.id, request.config)));
            if reply.status == 200 { reply.status = 202; }
            reply
        }
        (Method::Post, "/api/comparison/close") => json_reply(
            serde_json::from_str::<SessionRequest>(body)
                .map_err(|e| format!("Invalid comparison session: {e}"))
                .and_then(|request| manager.close(&request.id)),
        ),
        (Method::Post, "/api/comparison/stop") => {
            let mut reply = json_reply(serde_json::from_str::<SessionRequest>(body)
                .map_err(|e| format!("Invalid comparison session: {e}"))
                .and_then(|request| manager.stop(&request.id)));
            if reply.status == 200 { reply.status = 202; }
            reply
        }
        (Method::Options, _) => Reply {
            status: 204, body: vec![], content_type: "text/plain".into(), extra: vec![],
        },
        _ => Reply {
            status: 404, body: b"{\"error\":\"Unknown comparison operation.\"}".to_vec(),
            content_type: "application/json".into(), extra: vec![],
        },
    }
}

/// Narrow capability endpoint for injected native preview observers. It never
/// enters workspace proxying or the generic loopback CORS response helper.
pub(super) fn handle_scroll(
    mut request: tiny_http::Request,
    token: &str,
    relay: Option<&skill_core::comparison::ComparisonScrollRelay>,
    unlocked: bool,
) {
    use super::{from_this_machine, header_value};
    use skill_core::comparison::MAX_COMPARISON_SCROLL_BYTES;
    use std::io::Read;
    let origin = header_value(&request, "Origin");
    let local = from_this_machine(&request)
        && header_value(&request, "Forwarded").is_none()
        && request.remote_addr().is_some_and(|address| address.ip().is_loopback());
    let respond = |request: tiny_http::Request, status: u16, origin: Option<&str>| {
        let mut response = tiny_http::Response::empty(tiny_http::StatusCode(status));
        response.add_header(tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap());
        if let Some(origin) = origin {
            for (name, value) in [
                ("Access-Control-Allow-Origin", origin),
                ("Access-Control-Allow-Methods", "POST, OPTIONS"),
                ("Access-Control-Allow-Headers", "Content-Type"),
                ("Vary", "Origin"),
            ] {
                if let Ok(header) = tiny_http::Header::from_bytes(name, value) {
                    response.add_header(header);
                }
            }
        }
        // No generic log helper: the path contains a short-lived capability.
        let _ = request.respond(response);
    };
    if !local || relay.is_none() {
        respond(request, 404, None);
        return;
    }
    if !unlocked {
        respond(request, 423, None);
        return;
    }
    let relay = relay.unwrap();
    let Some(origin) = origin.filter(|origin| relay.authorized(token, origin)) else {
        respond(request, 403, None);
        return;
    };
    if *request.method() == Method::Options {
        let method = header_value(&request, "Access-Control-Request-Method");
        let headers = header_value(&request, "Access-Control-Request-Headers");
        let permitted = method.as_deref() == Some("POST")
            && headers.as_deref().is_none_or(|headers| headers.split(',').all(|header| header.trim().eq_ignore_ascii_case("content-type")));
        respond(request, if permitted { 204 } else { 403 }, permitted.then_some(origin.as_str()));
        return;
    }
    if *request.method() != Method::Post {
        respond(request, 405, Some(&origin));
        return;
    }
    if !header_value(&request, "Content-Type").is_some_and(|kind| kind.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/plain"))) {
        respond(request, 415, Some(&origin));
        return;
    }
    if request.body_length().is_some_and(|length| length > MAX_COMPARISON_SCROLL_BYTES) {
        respond(request, 413, Some(&origin));
        return;
    }
    let mut body = String::new();
    let read = request.as_reader().take((MAX_COMPARISON_SCROLL_BYTES + 1) as u64).read_to_string(&mut body);
    let status = if body.len() > MAX_COMPARISON_SCROLL_BYTES {
        413
    } else if read.is_err() || relay.dispatch(token, &origin, &body).is_err() {
        400
    } else {
        204
    };
    respond(request, status, Some(&origin));
}
