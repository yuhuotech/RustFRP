# Managed local plugins

The optional `plugin` member of `ManagedProxy` follows the snake_case JSON
shape below. It is sent only to the client Agent that owns the tunnel; external
Edge device snapshots remove this member. The Controller stores the options,
but REST tunnel responses expose only the plugin kind and boolean “secret set”
flags.

| Field | Purpose |
|---|---|
| `kind` | `socks5`, `http_proxy`, `static_file`, `unix_domain_socket`, `http2https`, `https2http`, `https2https`, or `tls2raw` |
| `local_addr`, `unix_path`, `local_path`, `strip_prefix` | Client-local target or static-file path |
| `http_user`, `http_password`, `username`, `password` | Optional plugin authentication; credentials are never returned by REST |
| `crt_path`, `key_path` | Certificate paths read on the client Agent; private key bytes never go to the Controller |
| `enable_http2` | Preserved for export; an explicit `true` is reported because plugin forwarding currently supports HTTP/1.1 only |
| `host_header_rewrite`, `request_headers` | HTTP/1.1 request transformation |

Plugin options are supported on managed TCP, HTTP, and HTTPS tunnels. `crt_path` and
`key_path` must both be empty or both be set. Per-field and header limits are
checked before storage; the client also checks file size and plugin-specific
requirements before applying the snapshot. Empty or absent `plugin` preserves
older wire snapshots.

The local tunnel summary and diagnostic APIs never contain plugin credentials,
certificate contents, or private keys. FRP config import activates supported
plugins whose certificate paths are local to the client; config export includes
the stored plugin fields.
