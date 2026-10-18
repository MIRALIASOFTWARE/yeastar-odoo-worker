# yeastar-odoo-gateway

Servicio en Rust que sustituye a los workers de Cloudflare `yeastar-ai-odoo` (citas) y
`zatick-cloud` (tickets). Un solo binario con estas rutas, todas `POST` con la cabecera
`X-PBX-Secret`:

| Ruta | Función |
|---|---|
| `/` | Agendar una cita |
| `/disponibilidad` | Horarios libres de un día |
| `/citas` | Listar la agenda (requiere `pin`) |
| `/ticket` | Crear un ticket |
| `/ticket/estado` | Consultar el estado de un ticket |
| `GET /health` | Comprobación de vida (sin secreto) |

## Compilar

```bash
cargo build --release
# binario: target/release/yeastar-odoo-gateway
```

## Instalar en el VPS (Debian/Ubuntu)

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin yeastar-gw
sudo mkdir -p /opt/yeastar-odoo-gateway
sudo cp target/release/yeastar-odoo-gateway /opt/yeastar-odoo-gateway/

sudo cp .env.example /etc/yeastar-odoo-gateway.env   # edítalo con los valores reales
sudo chown root:root /etc/yeastar-odoo-gateway.env && sudo chmod 600 /etc/yeastar-odoo-gateway.env

sudo cp deploy/yeastar-odoo-gateway.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now yeastar-odoo-gateway
journalctl -u yeastar-odoo-gateway -f        # logs
```

## HTTPS con Caddy

Instala Caddy, copia `deploy/Caddyfile` a `/etc/caddy/Caddyfile` con tu dominio y ejecuta
`sudo systemctl reload caddy`. Yeastar exige URLs `https://`.

## Probar

```bash
export URL="https://api.tudominio.com"; export SECRET="tu-secreto"
curl -s $URL/health
curl -s -w "\nHTTP %{http_code}\n" -X POST $URL/disponibilidad \
  -H "Content-Type: application/json" -H "X-PBX-Secret: $SECRET" \
  -d '{"fecha":"2026-10-19"}'
```

## Migrar los tools de Yeastar

Cambia solo el host de la URL de cada tool. Usa rutas exactas, sin `/` final
(salvo la raíz de citas):

- `agendar_citas_odoo` -> `https://api.tudominio.com/`
- `consultar_disponibilidad_odoo` -> `https://api.tudominio.com/disponibilidad`
- `crear_ticket_soporte` -> `https://api.tudominio.com/ticket`
- `consultar_estado_ticket` -> `https://api.tudominio.com/ticket/estado`

Importante: el worker de tickets aceptaba también la raíz `/` para crear tickets. Aquí `/`
agenda citas, así que el tool `crear_ticket_soporte` debe apuntar a `/ticket`.

## Alternativa: Docker (Ubuntu 24.04)

Archivos: `Dockerfile`, `docker-compose.yml` y `deploy/Caddyfile.docker`.

```bash
cp .env.example gateway.env        # edítalo con los valores reales
chmod 600 gateway.env
nano deploy/Caddyfile.docker       # pon tu dominio
docker compose up -d --build
docker compose logs -f gateway
```

Actualizar tras cambiar el código: `docker compose up -d --build`.
El archivo de entorno no admite comentarios en la misma línea que un valor.
