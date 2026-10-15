# RustFRP deployment templates

These templates keep the admin API behind a reverse proxy and terminate public
HTTPS there. Configure and expose the Edge data port and user tunnel range
separately in the host firewall.

## systemd and Caddy

1. Install `rustfrp-server` at `/usr/local/bin/rustfrp-server` and the built
   shared UI at `/opt/rustfrp/panel`.
2. Create a `rustfrp` system user and `/etc/rustfrp/rustfrp-server.env` with
   `RUSTFRP_HTTP=127.0.0.1:8080`, `RUSTFRP_EDGE=0.0.0.0:7000`,
   `RUSTFRP_PANEL_DIR=/opt/rustfrp/panel`,
   `RUSTFRP_PUBLIC_URL=https://frp.example.com`, and your port policy.
3. Install `rustfrp-server.service` and enable it with systemd.
4. Set `RUSTFRP_DOMAIN` and `ACME_EMAIL`, then load `Caddyfile`. Caddy manages
   the HTTPS certificate and WebSocket proxying.
5. Open TCP 80/443, the Edge port, and only the configured tunnel ports.
   Do not publish the admin listener directly.

## nginx

Replace the sample hostname and certificate paths in `nginx.conf`, obtain a
certificate with the host ACME client, then reload nginx. Upgrade headers are
configured for live panel events.

## Container

`compose.yaml` keeps the database in a named volume and places Caddy in front
of the Controller. Build the panel with `make web-build`, then mount its output
at `deploy/panel`. Pin release images by digest in production. With containers,
bind `RUSTFRP_HTTP` to the private service network and keep port 8080 unpublished.
