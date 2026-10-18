use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};

/// Cliente mínimo de JSON-RPC (`execute_kw`) de Odoo.
pub struct Odoo {
    client: Client,
    url: String,
    db: String,
    uid: i64,
    api_key: String,
}

impl Odoo {
    pub fn new(url: String, db: String, uid: i64, api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("no se pudo crear el cliente HTTP");
        Self {
            client,
            url: url.trim_end_matches('/').to_string(),
            db,
            uid,
            api_key,
        }
    }

    /// Llama a `execute_kw`. `args` y `kwargs` son los de la API de Odoo.
    pub async fn call(
        &self,
        model: &str,
        method: &str,
        args: Value,
        kwargs: Value,
    ) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "call",
            "id": 1,
            "params": {
                "service": "object",
                "method": "execute_kw",
                "args": [self.db, self.uid, self.api_key, model, method, args, kwargs]
            }
        });

        let resp = self
            .client
            .post(format!("{}/jsonrpc", self.url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("No se pudo conectar con Odoo: {e}"))?;

        let j: Value = resp
            .json()
            .await
            .map_err(|e| format!("Respuesta de Odoo no válida: {e}"))?;

        if let Some(err) = j.get("error") {
            let msg = err
                .pointer("/data/message")
                .and_then(Value::as_str)
                .or_else(|| err.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| err.to_string());
            return Err(msg);
        }
        Ok(j.get("result").cloned().unwrap_or(Value::Null))
    }
}
