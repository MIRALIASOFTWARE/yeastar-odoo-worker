use axum::{http::StatusCode, Json};
use serde_json::{json, Value};

/// Respuesta HTTP estándar de los handlers: (código, JSON).
pub type Resp = (StatusCode, Json<Value>);

pub fn resp(status: u16, v: Value) -> Resp {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(v),
    )
}

/// Convierte el resultado interno en respuesta; los errores inesperados salen como 502.
pub fn respond(r: Result<Resp, String>) -> Resp {
    match r {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("Error: {e}");
            resp(502, json!({ "ok": false, "error": e }))
        }
    }
}

pub fn parse_body(b: &[u8]) -> Result<Value, Resp> {
    serde_json::from_slice(b)
        .map_err(|_| resp(400, json!({ "ok": false, "error": "JSON inválido" })))
}

/// Lee un campo como texto recortado. Tolera null, ausente y números.
pub fn get_str(raw: &Value, key: &str) -> String {
    match raw.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

pub fn solo_digitos(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

pub fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Duración en minutos: acepta número o texto; si no es válida usa `default`. Máximo 480.
pub fn parse_duration(raw: &Value, default: i64) -> i64 {
    let n = match raw.get("duracion_minutos") {
        Some(Value::Number(n)) => n.as_f64().map(|f| f as i64),
        Some(Value::String(s)) => {
            let d: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            d.parse::<i64>().ok()
        }
        _ => None,
    };
    match n {
        Some(v) if v > 0 => v.min(480),
        _ => default,
    }
}

/// Odoo devuelve el id como número (o como lista con un id según la versión).
pub fn id_to_string(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a.first().map(id_to_string).unwrap_or_default(),
        other => other.to_string(),
    }
}
