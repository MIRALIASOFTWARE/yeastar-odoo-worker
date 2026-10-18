use std::sync::Arc;

use axum::{body::Bytes, extract::State};
use chrono::{Duration, Utc};
use serde_json::{json, Value};

use crate::util::{
    esc_html, get_str, id_to_string, parse_body, resp, respond, solo_digitos, Resp,
};
use crate::AppState;

const DEDUP_MINUTOS: i64 = 10; // ventana para no duplicar tickets

fn prioridad_codigo(s: &str) -> &'static str {
    match s.to_lowercase().as_str() {
        "baja" => "0",
        "normal" => "1",
        "alta" => "2",
        "urgente" => "3",
        _ => "1",
    }
}

fn prioridad_nombre(c: &str) -> &'static str {
    match c {
        "0" => "baja",
        "1" => "normal",
        "2" => "alta",
        "3" => "urgente",
        _ => "normal",
    }
}

/// Traducción de etapas de Odoo para leerlas por voz (ajusta a tus etapas).
fn estado_es(e: &str) -> String {
    let traducido = match e.to_lowercase().as_str() {
        "new" => "Nuevo",
        "in progress" => "En curso",
        "solved" => "Resuelto",
        "cancelled" => "Cancelado",
        _ => e,
    };
    traducido.to_string()
}

fn quitar_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut dentro = false;
    for c in s.chars() {
        match c {
            '<' => {
                dentro = true;
                out.push(' ');
            }
            '>' => dentro = false,
            _ if !dentro => out.push(c),
            _ => {}
        }
    }
    out
}

/// Busca un contacto por nombre; si no existe lo crea.
/// Si algo falla devuelve None y el ticket se crea igualmente sin cliente.
async fn buscar_o_crear_cliente(st: &AppState, nombre: &str, telefono: &str) -> Option<i64> {
    let limpio = nombre.replace(['%', '_'], "").trim().to_string();
    match st
        .odoo
        .call(
            "res.partner",
            "search",
            json!([[["name", "=ilike", limpio]]]),
            json!({ "limit": 1 }),
        )
        .await
    {
        Ok(v) => {
            if let Some(id) = v.as_array().and_then(|a| a.first()).and_then(Value::as_i64) {
                return Some(id);
            }
        }
        Err(e) => {
            tracing::error!("No se pudo asociar el cliente: {e}");
            return None;
        }
    }
    match st
        .odoo
        .call(
            "res.partner",
            "create",
            json!([{ "name": nombre, "phone": telefono }]),
            json!({}),
        )
        .await
    {
        Ok(v) => v
            .as_i64()
            .or_else(|| v.as_array().and_then(|a| a.first()).and_then(Value::as_i64)),
        Err(e) => {
            tracing::error!("No se pudo crear el cliente: {e}");
            None
        }
    }
}

// ---------- POST /ticket (crear) ----------

pub async fn crear(State(st): State<Arc<AppState>>, body: Bytes) -> Resp {
    respond(handle_crear(&st, &body).await)
}

async fn handle_crear(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let raw = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };

    let nombre = get_str(&raw, "nombre_cliente");
    let telefono = get_str(&raw, "telefono");
    let asunto = get_str(&raw, "asunto");
    let detalle = get_str(&raw, "descripcion_problema");
    let resumen = get_str(&raw, "resumen");
    let categoria = get_str(&raw, "categoria");
    let prioridad = prioridad_codigo(&get_str(&raw, "prioridad"));

    if nombre.is_empty() || asunto.is_empty() || detalle.is_empty() {
        return Ok(resp(
            400,
            json!({ "ok": false, "error": "Faltan datos: nombre_cliente, asunto o descripcion_problema" }),
        ));
    }
    if solo_digitos(&telefono).len() < 6 {
        return Ok(resp(
            400,
            json!({
                "ok": false,
                "error": "telefono_invalido",
                "mensaje": "Se necesita un teléfono de contacto válido."
            }),
        ));
    }

    let asunto_corto: String = asunto.chars().take(200).collect();

    // Evitar duplicados: mismo cliente y asunto en los últimos minutos
    let desde = (Utc::now() - Duration::minutes(DEDUP_MINUTOS))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let existentes = st
        .odoo
        .call(
            "helpdesk.ticket",
            "search",
            json!([[
                ["name", "=", asunto_corto],
                ["partner_name", "=", nombre],
                ["create_date", ">", desde]
            ]]),
            json!({ "limit": 1 }),
        )
        .await?;
    if let Some(id) = existentes.as_array().and_then(|a| a.first()) {
        let id_s = id_to_string(id);
        return Ok(resp(
            200,
            json!({
                "ok": true,
                "numero_ticket": id_s,
                "message": format!("Este ticket ya estaba registrado con el número {id_s}")
            }),
        ));
    }

    let partner_id = buscar_o_crear_cliente(st, &nombre, &telefono).await;

    let mut descripcion = format!(
        "<p><b>Cliente:</b> {}<br/><b>Teléfono:</b> {}<br/>",
        esc_html(&nombre),
        esc_html(&telefono)
    );
    if !categoria.is_empty() {
        descripcion.push_str(&format!("<b>Categoría:</b> {}<br/>", esc_html(&categoria)));
    }
    descripcion.push_str(&format!("<b>Problema:</b> {}<br/>", esc_html(&detalle)));
    if !resumen.is_empty() {
        descripcion.push_str(&format!(
            "<b>Resumen de la llamada:</b> {}<br/>",
            esc_html(&resumen)
        ));
    }
    descripcion.push_str("<i>Registrado por Agente Yeastar.</i></p>");

    let mut vals = json!({
        "name": asunto_corto,
        "team_id": st.cfg.helpdesk_team,
        "stage_id": st.cfg.helpdesk_stage,
        "user_id": st.cfg.helpdesk_user,
        "partner_name": nombre,
        "partner_phone": telefono,
        "priority": prioridad,
        "description": descripcion
    });
    if let Some(pid) = partner_id {
        vals["partner_id"] = json!(pid);
    }

    let id = st
        .odoo
        .call("helpdesk.ticket", "create", json!([vals]), json!({}))
        .await?;
    let id_s = id_to_string(&id);

    Ok(resp(
        200,
        json!({
            "ok": true,
            "numero_ticket": id_s,
            "message": format!("Ticket {id_s} creado correctamente para {nombre}")
        }),
    ))
}

// ---------- POST /ticket/estado ----------

pub async fn estado(State(st): State<Arc<AppState>>, body: Bytes) -> Resp {
    respond(handle_estado(&st, &body).await)
}

/// Mismo mensaje si no existe o si el teléfono no coincide (no revelar cuál).
fn no_encontrado(motivo: &str) -> Resp {
    tracing::error!("estado: no_encontrado -> {motivo}");
    resp(
        404,
        json!({
            "ok": false,
            "error": "no_encontrado",
            "mensaje": "No se encontró un ticket con esos datos."
        }),
    )
}

async fn handle_estado(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let raw = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };

    let num = match solo_digitos(&get_str(&raw, "numero_ticket"))
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
    {
        Some(n) => n,
        None => {
            return Ok(resp(
                400,
                json!({ "ok": false, "error": "numero_invalido", "mensaje": "Número de ticket no válido." }),
            ))
        }
    };
    let tel = solo_digitos(&get_str(&raw, "telefono"));
    if tel.len() < 6 {
        return Ok(resp(
            400,
            json!({
                "ok": false,
                "error": "telefono_invalido",
                "mensaje": "Se necesita el teléfono con el que se creó el ticket."
            }),
        ));
    }

    let res = st
        .odoo
        .call(
            "helpdesk.ticket",
            "search_read",
            json!([[["id", "=", num]]]),
            json!({
                "fields": ["name", "stage_id", "priority", "create_date", "description", "partner_phone"],
                "limit": 1
            }),
        )
        .await?;

    let t = match res.as_array().and_then(|a| a.first()) {
        Some(t) => t,
        None => return Ok(no_encontrado(&format!("no existe el ticket {num}"))),
    };

    let ultimos = &tel[tel.len().saturating_sub(8)..]; // dígitos ASCII: corte seguro
    let tel_guardado = solo_digitos(t["partner_phone"].as_str().unwrap_or(""));
    let texto_ticket = solo_digitos(&quitar_tags(t["description"].as_str().unwrap_or("")));
    let coincide = (!tel_guardado.is_empty() && tel_guardado.contains(ultimos))
        || texto_ticket.contains(ultimos);
    if !coincide {
        return Ok(no_encontrado("el teléfono no coincide"));
    }

    let estado_orig = t["stage_id"]
        .as_array()
        .and_then(|a| a.get(1))
        .and_then(Value::as_str)
        .unwrap_or("Desconocido");
    let creado: String = t["create_date"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(10)
        .collect();

    Ok(resp(
        200,
        json!({
            "ok": true,
            "numero_ticket": num.to_string(),
            "asunto": t["name"].as_str().unwrap_or(""),
            "estado": estado_es(estado_orig),
            "prioridad": prioridad_nombre(t["priority"].as_str().unwrap_or("1")),
            "creado": creado
        }),
    ))
}
