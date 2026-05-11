#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Source .env from project root if present
if [[ -f "$SCRIPT_DIR/../.env" ]]; then
  # shellcheck source=/dev/null
  source "$SCRIPT_DIR/../.env"
fi

DEPLOY_USER=${DEPLOY_USER:-azu}
DEPLOY_HOST=${DEPLOY_HOST:-144.31.17.0}
NGINX_SITE=${NGINX_SITE:-goblin.geno.su}
NGINX_EXISTING_LOG=${NGINX_EXISTING_LOG:-goblin_slop_access.log}
METRICS_LOG=${METRICS_LOG:-goblin_metrics.log}
METRICS_AUTH_USER=${METRICS_AUTH_USER:-bebra}
METRICS_AUTH_PASS=${METRICS_AUTH_PASS:-bebra}

REMOTE="${DEPLOY_USER}@${DEPLOY_HOST}"
REMOTE_BIN=/opt/goblin-metrics
REMOTE_DB_DIR=/var/lib/goblin-metrics

PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "==> Building release binaries…"
cd "$PROJECT_ROOT"
cargo build --release 2>&1

echo "==> Uploading binaries…"
ssh "$REMOTE" "mkdir -p /tmp/goblin-bins"
scp target/release/log-ingestor target/release/sys-metrics target/release/web-ui \
  "$REMOTE:/tmp/goblin-bins/"
ssh "$REMOTE" "sudo mkdir -p $REMOTE_BIN && \
  sudo systemctl stop goblin-log-ingestor goblin-sys-metrics goblin-web-ui 2>/dev/null || true && \
  for bin in log-ingestor sys-metrics web-ui; do \
    sudo install -m 750 -o root -g goblin-metrics /tmp/goblin-bins/\$bin $REMOTE_BIN/\$bin; \
  done"

echo "==> Uploading migrations and deploy files…"
ssh "$REMOTE" "mkdir -p /tmp/goblin-migrations /tmp/goblin-deploy"
scp migrations/001_init.sql migrations/002_add_host.sql migrations/003_add_latency.sql \
    migrations/004_host_ts_index.sql "$REMOTE:/tmp/goblin-migrations/"
scp deploy/goblin-log-ingestor.service deploy/goblin-sys-metrics.service \
    deploy/goblin-web-ui.service deploy/nginx-logging.conf \
    deploy/nginx-metrics-location.conf \
  "$REMOTE:/tmp/goblin-deploy/"

echo "==> Generating htpasswd…"
HTPASSWD_HASH=$(openssl passwd -apr1 "$METRICS_AUTH_PASS")
printf '%s:%s\n' "$METRICS_AUTH_USER" "$HTPASSWD_HASH" | \
  ssh "$REMOTE" "cat > /tmp/goblin-htpasswd"

echo "==> Pushing deploy config to remote…"
ssh "$REMOTE" "cat > /tmp/goblin-deploy-env.sh" << EOF
NGINX_SITE='${NGINX_SITE}'
NGINX_EXISTING_LOG='${NGINX_EXISTING_LOG}'
METRICS_LOG='${METRICS_LOG}'
EOF

echo "==> Server setup…"
ssh "$REMOTE" bash -s << 'REMOTE_SCRIPT'
source /tmp/goblin-deploy-env.sh
set -euo pipefail

# ── Dedicated service user ────────────────────────────────────────────────────
if ! id goblin-metrics &>/dev/null; then
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
    --comment "GoblinMetrics service account" goblin-metrics
  echo "  created goblin-metrics user"
fi

# Log access: add goblin-metrics to adm group (owns /var/log/nginx/)
if ! id goblin-metrics | grep -q adm; then
  sudo usermod -aG adm goblin-metrics
  echo "  goblin-metrics added to adm group"
fi

# Ensure nginx log files are group-readable by adm
sudo chmod g+r /var/log/nginx/*.log 2>/dev/null || true

# ── Data directory ────────────────────────────────────────────────────────────
sudo mkdir -p /var/lib/goblin-metrics
sudo chown goblin-metrics:goblin-metrics /var/lib/goblin-metrics
sudo chmod 750 /var/lib/goblin-metrics

# ── SQLite migrations ─────────────────────────────────────────────────────────
if ! command -v sqlite3 &>/dev/null; then
  sudo apt-get install -y sqlite3
fi
sudo -u goblin-metrics sqlite3 /var/lib/goblin-metrics/metrics.db \
  < /tmp/goblin-migrations/001_init.sql
sudo -u goblin-metrics sqlite3 /var/lib/goblin-metrics/metrics.db \
  < /tmp/goblin-migrations/002_add_host.sql 2>/dev/null || true
sudo -u goblin-metrics sqlite3 /var/lib/goblin-metrics/metrics.db \
  < /tmp/goblin-migrations/003_add_latency.sql 2>/dev/null || true
sudo -u goblin-metrics sqlite3 /var/lib/goblin-metrics/metrics.db \
  < /tmp/goblin-migrations/004_host_ts_index.sql 2>/dev/null || true
echo "  migrations done"

# ── Nginx basic-auth htpasswd ─────────────────────────────────────────────────
sudo install -m 640 -o root -g www-data /tmp/goblin-htpasswd \
  /etc/nginx/.goblin-metrics.htpasswd
echo "  htpasswd installed"

# ── Nginx extended logging ────────────────────────────────────────────────────
sudo cp /tmp/goblin-deploy/nginx-logging.conf \
  /etc/nginx/conf.d/goblin-metrics-logging.conf

SITE=/etc/nginx/sites-enabled/$NGINX_SITE
if ! grep -q "$METRICS_LOG" "$SITE"; then
  sudo sed -i \
    "/access_log.*$NGINX_EXISTING_LOG/a\\    access_log /var/log/nginx/$METRICS_LOG goblin_json;" \
    "$SITE"
  echo "  nginx site updated"
fi

# Mount dashboard at site/goblin-metrics/ (proxy to 127.0.0.1:4444).
sudo mkdir -p /etc/nginx/snippets
sudo cp /tmp/goblin-deploy/nginx-metrics-location.conf \
  /etc/nginx/snippets/goblin-metrics.conf

if ! sudo grep -q 'snippets/goblin-metrics.conf' "$SITE"; then
  sudo awk '
    /^server[[:space:]]*\{/ { server_count++ }
    server_count == 2 && !done && /^    location \/ \{/ {
      print "    include /etc/nginx/snippets/goblin-metrics.conf;"
      print ""
      done = 1
    }
    { print }
  ' "$SITE" > /tmp/goblin-site.new && \
    sudo install -m 644 /tmp/goblin-site.new "$SITE" && \
    rm /tmp/goblin-site.new
  echo "  goblin-metrics include injected into $NGINX_SITE"
fi

# Clean up old subdomain config if it exists from a prior deploy
sudo rm -f /etc/nginx/sites-enabled/metrics.$NGINX_SITE

sudo nginx -t
sudo systemctl reload nginx
echo "  nginx reloaded"

# ── Systemd units ─────────────────────────────────────────────────────────────
sudo cp /tmp/goblin-deploy/goblin-log-ingestor.service /etc/systemd/system/
sudo cp /tmp/goblin-deploy/goblin-sys-metrics.service  /etc/systemd/system/
sudo cp /tmp/goblin-deploy/goblin-web-ui.service       /etc/systemd/system/

# Patch log path in ingestor service to match configured log filename
sudo sed -i "s|LOG_FILE=.*|LOG_FILE=/var/log/nginx/$METRICS_LOG|" \
  /etc/systemd/system/goblin-log-ingestor.service

sudo systemctl daemon-reload

for svc in goblin-log-ingestor goblin-sys-metrics goblin-web-ui; do
  sudo systemctl enable  "$svc"
  sudo systemctl restart "$svc"
  sleep 1
  echo "  $svc: $(systemctl is-active $svc)"
done
REMOTE_SCRIPT

echo ""
echo "==> Service status:"
ssh "$REMOTE" "systemctl status goblin-log-ingestor goblin-sys-metrics goblin-web-ui --no-pager -l || true"

echo ""
echo "==> Deploy complete. Dashboard: https://${NGINX_SITE}/goblin-metrics/"
echo "    (web-ui binds 127.0.0.1:4444 — not publicly exposed; access via nginx or SSH tunnel)"
