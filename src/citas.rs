use std::sync::Arc;

use axum::{body::Bytes, extract::State};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::util::{get_str, parse_body, parse_duration, resp, respond, Resp};
use crate::AppState;

const WORK_START: i64 = 9; // 9:00
const WORK_END: i64 = 17; // 17:00
const STEP_MIN: i64 = 30; // intervalo entre horarios ofrecidos
const DEFAULT_DUR: i64 = 60; // duración por defecto (min)
const MAX_SLOTS: usize = 8; // máximo de horarios devueltos

// ---------- utilidades de fecha ----------

fn fmt_utc(d: DateTime<Utc>) -> String {
    d.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn parse_odoo_utc(s: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|n| n.and_utc())
}

/// Hora local de la zona del negocio -> UTC. Si la hora no existe (salto de
/// horario de verano) usa una hora más tarde.
fn local_to_utc(tz: Tz, n: NaiveDateTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&n)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(n + Duration::hours(1))).earliest())
        .map(|d| d.with_timezone(&Utc))
}

fn parse_local_datetime(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim().replace('T', " ");
    NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M"))
        .ok()
}

fn es_hhmm(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && [0, 1, 3, 4].iter().all(|&i| b[i].is_ascii_digit())
}

fn rango_dia(tz: Tz, d: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let ini = local_to_utc(tz, d.and_hms_opt(0, 0, 0)?)?;
    let fin = local_to_utc(tz, d.succ_opt()?.and_hms_opt(0, 0, 0)?)?;
    Some((ini, fin))
}

fn rango_mes(tz: Tz, primero: NaiveDate) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (y, m) = (primero.year(), primero.month());
    let siguiente = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)?
    };
    let ini = local_to_utc(tz, primero.and_hms_opt(0, 0, 0)?)?;
    let fin = local_to_utc(tz, siguiente.and_hms_opt(0, 0, 0)?)?;
    Some((ini, fin))
}

// ---------- consultas a Odoo ----------

/// Eventos del propietario que se solapan con [start, stop].
async fn eventos_en_rango(
    st: &AppState,
    start: DateTime<Utc>,
    stop: DateTime<Utc>,
) -> Result<Vec<Value>, String> {
    let dominio = json!([[
        ["start", "<", fmt_utc(stop)],
        ["stop", ">", fmt_utc(start)],
        ["partner_ids", "in", [st.cfg.owner_partner]]
    ]]);
    let r = st
        .odoo
        .call(
            "calendar.event",
            "search_read",
            dominio,
            json!({ "fields": ["name", "start", "stop"] }),
        )
        .await?;
    Ok(r.as_array().cloned().unwrap_or_default())
}

fn solapa(eventos: &[Value], s: DateTime<Utc>, e: DateTime<Utc>) -> bool {
    eventos.iter().any(|ev| {
        let a = ev["start"].as_str().and_then(parse_odoo_utc);
        let b = ev["stop"].as_str().and_then(parse_odoo_utc);
        match (a, b) {
            (Some(a), Some(b)) => a < e && b > s,
            _ => false,
        }
    })
}

// ---------- POST /disponibilidad ----------

pub async fn disponibilidad(State(st): State<Arc<AppState>>, body: Bytes) -> Resp {
    respond(handle_disponibilidad(&st, &body).await)
}

async fn handle_disponibilidad(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let raw = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let tz = st.cfg.tz;
    let fecha_s = get_str(&raw, "fecha");
    let fecha = match NaiveDate::parse_from_str(&fecha_s, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => {
            return Ok(resp(
                400,
                json!({ "ok": false, "error": "Fecha inválida. Formato esperado: YYYY-MM-DD" }),
            ))
        }
    };
    let dur = parse_duration(&raw, DEFAULT_DUR);

    if matches!(fecha.weekday(), Weekday::Sat | Weekday::Sun) {
        return Ok(resp(
            200,
            json!({
                "ok": true,
                "fecha": fecha_s,
                "disponibles": "",
                "hora_pedida_libre": "no",
                "mensaje": "No se agenda en fines de semana. Solo de lunes a viernes."
            }),
        ));
    }

    let dia_ini = fecha
        .and_hms_opt(WORK_START as u32, 0, 0)
        .and_then(|n| local_to_utc(tz, n))
        .ok_or("Fecha inválida")?;
    let dia_fin = fecha
        .and_hms_opt(WORK_END as u32, 0, 0)
        .and_then(|n| local_to_utc(tz, n))
        .ok_or("Fecha inválida")?;
    let eventos = eventos_en_rango(st, dia_ini, dia_fin).await?;
    let ahora = Utc::now();

    let mut libres: Vec<String> = Vec::new();
    let mut min = WORK_START * 60;
    while min + dur <= WORK_END * 60 {
        let (h, m) = (min / 60, min % 60);
        if let Some(s) = fecha
            .and_hms_opt(h as u32, m as u32, 0)
            .and_then(|n| local_to_utc(tz, n))
        {
            let e = s + Duration::minutes(dur);
            if s > ahora && !solapa(&eventos, s, e) {
                libres.push(format!("{h:02}:{m:02}"));
            }
        }
        min += STEP_MIN;
    }

    let hora_pedida = get_str(&raw, "hora");
    let hora_libre = if es_hhmm(&hora_pedida) {
        if libres.contains(&hora_pedida) { "si" } else { "no" }
    } else {
        ""
    };

    let lista: Vec<&str> = libres.iter().take(MAX_SLOTS).map(|s| s.as_str()).collect();
    let mensaje = if libres.is_empty() {
        "No hay horarios libres ese día.".to_string()
    } else {
        format!("Horarios libres: {}.", lista.join(", "))
    };

    Ok(resp(
        200,
        json!({
            "ok": true,
            "fecha": fecha_s,
            "duracion_minutos": dur,
            "disponibles": lista.join(", "),
            "hora_pedida_libre": hora_libre,
            "mensaje": mensaje
        }),
    ))
}

// ---------- POST / (agendar) ----------

pub async fn agendar(State(st): State<Arc<AppState>>, body: Bytes) -> Resp {
    respond(handle_agendar(&st, &body).await)
}

async fn handle_agendar(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let raw = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let tz = st.cfg.tz;

    let nombre = get_str(&raw, "nombre_cliente");
    let telefono = Some(get_str(&raw, "telefono"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "N/D".to_string());
    let motivo = Some(get_str(&raw, "motivo"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Consulta telefónica".to_string());
    let dur = parse_duration(&raw, DEFAULT_DUR);
    let fecha_hora = get_str(&raw, "fecha_hora_inicio");

    if nombre.is_empty() || fecha_hora.is_empty() {
        return Ok(resp(
            400,
            json!({ "ok": false, "error": "Faltan parámetros requeridos: nombre_cliente y fecha_hora_inicio" }),
        ));
    }

    let start = match parse_local_datetime(&fecha_hora).and_then(|n| local_to_utc(tz, n)) {
        Some(d) => d,
        None => {
            return Ok(resp(
                400,
                json!({
                    "ok": false,
                    "error": format!("Fecha inválida: {fecha_hora}. Formato esperado: YYYY-MM-DD HH:MM")
                }),
            ))
        }
    };
    let stop = start + Duration::minutes(dur);

    if start <= Utc::now() {
        return Ok(resp(
            400,
            json!({ "ok": false, "error": "fecha_pasada", "mensaje": "Esa fecha y hora ya pasó." }),
        ));
    }

    // Evitar citas dobles
    let eventos = eventos_en_rango(st, start, stop).await?;
    if solapa(&eventos, start, stop) {
        return Ok(resp(
            409,
            json!({
                "ok": false,
                "error": "horario_ocupado",
                "mensaje": "Ese horario ya está ocupado. Ofrezca otro."
            }),
        ));
    }

    let vals = json!({
        "name": format!("{motivo} - {nombre}"),
        "start": fmt_utc(start),
        "stop": fmt_utc(stop),
        "description": format!(
            "Cliente: {nombre}\nTeléfono: {telefono}\nMotivo: {motivo}\nRegistrado por Agente Yeastar."
        ),
        "user_id": st.cfg.cal_user,
        "partner_ids": [[6, 0, st.cfg.cal_partners]]
    });
    let id = st
        .odoo
        .call("calendar.event", "create", json!([vals]), json!({}))
        .await?;

    Ok(resp(
        200,
        json!({
            "ok": true,
            "status": "success",
            "event_id": id,
            "message": format!("Cita agendada con éxito para {nombre}")
        }),
    ))
}

// ---------- POST /citas (listar agenda, con PIN) ----------

pub async fn listar(State(st): State<Arc<AppState>>, body: Bytes) -> Resp {
    respond(handle_listar(&st, &body).await)
}

async fn handle_listar(st: &AppState, body: &[u8]) -> Result<Resp, String> {
    let raw = match parse_body(body) {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };

    let pin = get_str(&raw, "pin");
    let pin_ok = match st.cfg.owner_pin.as_deref() {
        Some(p) if !p.is_empty() => bool::from(pin.as_bytes().ct_eq(p.as_bytes())),
        _ => false,
    };
    if !pin_ok {
        return Ok(resp(
            403,
            json!({ "ok": false, "error": "pin_incorrecto", "mensaje": "El PIN no es correcto." }),
        ));
    }

    let tz = st.cfg.tz;
    let fecha = get_str(&raw, "fecha");
    let mes = get_str(&raw, "mes");

    let (ini, fin, etiqueta, max_items) =
        if let Ok(d) = NaiveDate::parse_from_str(&fecha, "%Y-%m-%d") {
            let (a, b) = rango_dia(tz, d).ok_or("Fecha inválida")?;
            (a, b, fecha.clone(), 15usize)
        } else if let Ok(p) = NaiveDate::parse_from_str(&format!("{mes}-01"), "%Y-%m-%d") {
            let (a, b) = rango_mes(tz, p).ok_or("Mes inválido")?;
            (a, b, mes.clone(), 10usize)
        } else {
            return Ok(resp(
                400,
                json!({ "ok": false, "error": "Indique fecha (YYYY-MM-DD) o mes (YYYY-MM)" }),
            ));
        };

    let r = st
        .odoo
        .call(
            "calendar.event",
            "search_read",
            json!([[
                ["start", "<", fmt_utc(fin)],
                ["stop", ">", fmt_utc(ini)],
                ["partner_ids", "in", [st.cfg.owner_partner]]
            ]]),
            json!({ "fields": ["name", "start", "stop"], "order": "start asc", "limit": 100 }),
        )
        .await?;

    let citas: Vec<String> = r
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|ev| {
            let s = ev["start"].as_str().and_then(parse_odoo_utc)?;
            let local = s.with_timezone(&tz);
            Some(format!(
                "{} {}",
                local.format("%Y-%m-%d %H:%M"),
                ev["name"].as_str().unwrap_or("")
            ))
        })
        .collect();

    let mensaje = if citas.is_empty() {
        "No hay citas en ese periodo.".to_string()
    } else if citas.len() > max_items {
        format!("Hay {} citas; se muestran las primeras {}.", citas.len(), max_items)
    } else {
        format!("Hay {} citas.", citas.len())
    };

    Ok(resp(
        200,
        json!({
            "ok": true,
            "periodo": etiqueta,
            "total": citas.len(),
            "citas": citas.iter().take(max_items).cloned().collect::<Vec<_>>().join("; "),
            "mensaje": mensaje
        }),
    ))
}
