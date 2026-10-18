//! Pasarela entre el agente de IA de Yeastar y Odoo (citas y tickets de soporte).
//! Sustituye a los workers de Cloudflare `yeastar-ai-odoo` y `zatick-cloud`.

mod citas;
mod odoo;
mod tickets;
mod util;

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono_tz::Tz;
use serde_json::json;
use subtle::ConstantTimeEq;
use tracing_subscriber::EnvFilter;

use odoo::Odoo;
use util::resp;

pub struct Config {
    pub pbx_secret: String,
    pub owner_pin: Option<String>,
    pub tz: Tz,
    pub owner_partner: i64,
    pub cal_user: i64,
    pub cal_partners: Vec<i64>,
    pub helpdesk_team: i64,
    pub helpdesk_user: i64,
    pub helpdesk_stage: i64,
}

pub struct AppState {
    pub cfg: Config,
    pub odoo: Odoo,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_i64(name: &str, default: i64) -> i64 {
    env_or(name, &default.to_string()).trim().parse().unwrap_or(default)
}

fn env_req(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => panic!("Falta la variable de entorno obligatoria {name}"),
    }
}

/// Exige la cabecera X-PBX-Secret en todas las rutas de negocio.
async fn auth(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get("x-pbx-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    tracing::info!(
        "Petición: {} {} | secreto recibido: {}",
        req.method(),
        req.uri().path(),
        if provided.is_empty() { "NO" } else { "sí" }
    );

    let esperado = st.cfg.pbx_secret.as_bytes();
    let ok = !esperado.is_empty() && bool::from(provided.as_bytes().ct_eq(esperado));
    if !ok {
        tracing::error!("401: secreto ausente o no coincide");
        return resp(401, json!({ "ok": false, "error": "Unauthorized" })).into_response();
    }
    next.run(req).await
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("no se pudo escuchar SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term.recv() => {},
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let tz = env_or("BUSINESS_TZ", "America/Havana")
        .parse::<Tz>()
        .map_err(|_| "BUSINESS_TZ no es una zona horaria válida (ej. America/Havana)")?;

    let cfg = Config {
        pbx_secret: env_req("PBX_WEBHOOK_SECRET"),
        owner_pin: std::env::var("OWNER_PIN").ok().filter(|v| !v.trim().is_empty()),
        tz,
        owner_partner: env_i64("OWNER_PARTNER_ID", 3),
        cal_user: env_i64("CAL_USER_ID", 2),
        cal_partners: env_or("CAL_PARTNER_IDS", "3,13")
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect(),
        helpdesk_team: env_i64("HELPDESK_TEAM_ID", 1),
        helpdesk_user: env_i64("HELPDESK_USER_ID", 2),
        helpdesk_stage: env_i64("HELPDESK_STAGE_ID", 1),
    };

    let odoo = Odoo::new(
        env_req("ODOO_URL"),
        env_req("ODOO_DB"),
        env_i64("ODOO_UID", 6),
        env_req("ODOO_API_KEY"),
    );
    let state = Arc::new(AppState { cfg, odoo });

    let protegido = Router::new()
        .route("/", post(citas::agendar))
        .route("/disponibilidad", post(citas::disponibilidad))
        .route("/citas", post(citas::listar))
        .route("/ticket", post(tickets::crear))
        .route("/ticket/estado", post(tickets::estado))
        .layer(middleware::from_fn_with_state(state.clone(), auth));

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(protegido)
        .with_state(state);

    let addr = env_or("BIND_ADDR", "127.0.0.1:8080");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Escuchando en {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
