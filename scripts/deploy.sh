#!/usr/bin/env bash
set -euo pipefail

DEPLOY_USER=${DEPLOY_USER:-azu}
DEPLOY_HOST=${DEPLOY_HOST:-144.31.17.0}
REMOTE="${DEPLOY_USER}@${DEPLOY_HOST}"
REMOTE_BIN=/opt/goblin-metrics
REMOTE_DB_DIR=/var/lib/goblin-metrics
REMOTE_CONF=/etc/nginx/conf.d/goblin-metrics-logging.conf
NGINX_SITE=/etc/nginx/sites-enabled/goblin.geno.su

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
scp migrations/001_init.sql migrations/002_add_host.sql "$REMOTE:/tmp/goblin-migrations/"
scp deploy/goblin-log-ingestor.service deploy/goblin-sys-metrics.service \
    deploy/goblin-web-ui.service deploy/nginx-logging.conf \
  "$REMOTE:/tmp/goblin-deploy/"

echo "==> Server setup…"
ssh "$REMOTE" bash -s << 'REMOTE_SCRIPT'
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
echo "  migrations done"

# ── Nginx extended logging ────────────────────────────────────────────────────
sudo cp /tmp/goblin-deploy/nginx-logging.conf \
  /etc/nginx/conf.d/goblin-metrics-logging.conf

SITE=/etc/nginx/sites-enabled/goblin.geno.su
if ! grep -q goblin_metrics.log "$SITE"; then
  sudo sed -i \
    '/access_log \/var\/log\/nginx\/goblin_slop_access.log combined;/a\    access_log /var/log/nginx/goblin_metrics.log goblin_json;' \
    "$SITE"
  echo "  nginx site updated"
fi

sudo nginx -t
sudo systemctl reload nginx
echo "  nginx reloaded"

# ── Systemd units ─────────────────────────────────────────────────────────────
sudo cp /tmp/goblin-deploy/goblin-log-ingestor.service /etc/systemd/system/
sudo cp /tmp/goblin-deploy/goblin-sys-metrics.service  /etc/systemd/system/
sudo cp /tmp/goblin-deploy/goblin-web-ui.service       /etc/systemd/system/
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
echo "==> Deploy complete. Dashboard: http://${DEPLOY_HOST}:4444"
