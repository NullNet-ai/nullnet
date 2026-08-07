# HTTP path-based routing + redirects

> Design for [#137](https://github.com/NullNet-ai/nullnet/issues/137): let
> `nullnet-proxy` route a single Host across multiple backend services by URL
> path (NGINX `location` block parity), and configure literal redirects —
> without a separate NGINX instance in front of it.

## Goal

Today one HTTP service name **is** the Host — `upstream_peer` resolves the
`Host`/`:authority` header straight to a `service_name` and asks the control
service for an upstream. There is no way to send `example.com/api` to one
backend and `example.com/grafana` to another, and no way to configure an
arbitrary redirect (the only redirect that exists is the hardcoded
HTTP→HTTPS 301). Kevin's ask: something like NGINX's `location` block —
route by path prefix to different backends, redirects included — so multiple
apps can share one Host without a separate NGINX in front of `nullnet-proxy`.

Scope: **HTTP(S) only.** TCP/UDP port mappings are unaffected — they have no
notion of a request path.

## Current model (recap)

- A `[[services]]` TOML entry's `name` *is* the routing key for HTTP: the
  proxy takes the Host header verbatim as `service_name` and calls the
  `Proxy` RPC, which resolves/builds the backend (container discovery, VXLAN
  edge, etc.) — see `members/nullnet-proxy/src/main.rs` `upstream_peer` and
  `members/nullnet-server/src/nullnet_grpc_impl.rs` `handle_proxy_request`.
- `tcp`/`udp` services instead get a `listen_port` and flow through
  `PortMappingBundle`/`WatchPortMappings`, which the proxy turns into raw
  listeners (`port_mappings.rs`).
- Path is never read anywhere in the request path; `ServiceProtocol::Http`
  literally means "Host-header routed, no path awareness."

## Proposed model

Introduce a **route table**, decoupled from service definitions, using the
same "server owns the config, proxy subscribes to a watch stream" pattern
already used for `CertBundle`/`PortMappingBundle`.

A route is `(host, path_prefix) → target`, where `target` is either an
existing service name (proxy_pass equivalent) or a literal redirect
(host/path/status/URL). Backends keep meaning exactly what they mean today —
this only adds a dispatch layer in front of them.

### Backward compatibility (no migration required)

Every `[[services]]` entry with `protocol = "http"` (default) and a
`timeout` (proxy-reachable) is *implicitly* also a route:
`{ host = name, path_prefix = "/", target = Service(name) }`. An install
with zero `[[route]]` entries behaves exactly as it does today — this is the
same trick `local_protocol()` already plays for backward-compat elsewhere in
this codebase.

An explicit `[[route]]` for a host takes over that host's dispatch entirely:
once any route names a host, the implicit `{name} → "/"` fallback for that
host is dropped and every path must be explicitly covered (falling outside
all prefixes → 404), same as an NGINX `server{}` block with no catch-all
`location /`.

### Config (TOML)

```toml
# unchanged — still the backend a route can point at
[[services]]
name = "grafana"
docker_container = "grafana"
port = 3000
timeout = 30

[[services]]
name = "gitlab"
docker_container = "gitlab"
port = 80
timeout = 30

# new — explicit location blocks for one Host
[[route]]
host = "ops.example.com"
path = "/grafana"
service = "grafana"

[[route]]
host = "ops.example.com"
path = "/gitlab"
service = "gitlab"

[[route]]
host = "ops.example.com"
path = "/"
service = "grafana"          # catch-all, same as an NGINX `location /`

# new — a redirect needs no backend at all
[[route]]
host = "old.example.com"
path = "/"
redirect_to = "https://ops.example.com/"
redirect_status = 301        # optional, defaults to 301
```

`service` and `redirect_to` are mutually exclusive (exactly one required),
mirroring how `*_blocked_countries`/`*_allowed_countries` are already
validated as mutually exclusive in `input.rs`. `redirect_status` is
restricted to `301 | 302 | 307 | 308`.

### Proto

```proto
// One location-block-style dispatch rule for HTTP(S). Longest path_prefix
// match wins within a host; "/" is the catch-all. Mutually exclusive target.
message HttpRoute {
  string host = 1;
  string path_prefix = 2;
  oneof target {
    string service_name = 3;     // proxy_pass equivalent — resolved the same
                                  // way a Host-routed service is today
    HttpRedirect redirect = 4;
  }
}

message HttpRedirect {
  string to = 1;           // absolute URL, or a path template — see "Redirect
                            // target" below
  uint32 status_code = 2;  // 301/302/307/308
}

message HttpRouteBundle {
  repeated HttpRoute routes = 1;
}

// Long-lived stream, mirrors WatchCertificates/WatchPortMappings: full table
// on subscribe, one push per services.toml change.
rpc WatchHttpRoutes(Empty) returns (stream HttpRouteBundle);
```

No change to `ProxyRequest`/`Upstream`/`Proxy` RPC — once a route resolves to
a `service_name`, everything downstream (container discovery, VXLAN edge
setup, `get_or_add_upstream`) is untouched.

### Redirect target

Keep it simple for v1: `to` is either an absolute URL (used verbatim) or
starts with `/` (path-only — same scheme/host as the incoming request,
target path substituted). No variable interpolation (no `$request_uri`
equivalent) in v1 — see Open wrinkles.

### Server changes (`nullnet-server`)

- `ServiceToml` gains no fields. New `RouteToml` struct alongside it in
  `services/input.rs`:
  ```rust
  #[derive(Deserialize)]
  struct RouteToml {
      host: String,
      #[serde(default = "default_path")]
      path: String,
      service: Option<String>,
      redirect_to: Option<String>,
      #[serde(default)]
      redirect_status: Option<u16>,
  }
  ```
- `ServicesToml` gains `#[serde(default)] routes: Vec<RouteToml>`.
- Validation (in `parse_stack_content`, so both the file loader and the UI's
  save-time check share it, same as today):
  - exactly one of `service`/`redirect_to` per route;
  - `redirect_status` ∈ {301,302,307,308} when present;
  - `service` must name a declared service in the *same* stack (routes don't
    reach across stacks, same scoping as `proxy_dependencies`);
  - the referenced service must be `protocol = "http"`;
  - no two routes in the *same stack* share `(host, path)` exactly — this
    plus the equivalent cross-stack check (below) is `detect_route_conflicts`,
    the HTTP-route sibling of the existing `detect_port_conflicts`.
- `detect_route_conflicts(&StackMap) -> Vec<RouteConflict>`, cross-stack, same
  shape/spirit as `detect_port_conflicts` — `(host, path)` is a global proxy
  resource like `(protocol, listen_port)` is today. Wired into
  `load_validated()` and the reload path identically (drop offending stacks
  rather than brick the control plane, emit the same kind of event).
- `nullnet_grpc_impl.rs`: `build_http_route_bundle(&StackMap) -> HttpRouteBundle`
  next to `build_port_mapping_bundle`, folding in each stack's implicit
  fallback routes for hosts with no explicit `[[route]]`. New
  `http_routes: watch::Receiver<HttpRouteBundle>` field + `http_routes_changed:
  Arc<Notify>` (its own `Notify`, matching the existing comment on why
  `port_mappings_changed` isn't shared) + `watch_http_routes` RPC handler,
  copy-pasted from `watch_port_mappings`.

### Proxy changes (`nullnet-proxy`)

- New `routes.rs`: `RouteTable` (host → `Vec<HttpRoute>` sorted by
  `path_prefix` length descending, so first match = longest-prefix match),
  held in an `Arc<ArcSwap<RouteTable>>` exactly like `CertStore`.
  `watch_and_serve` mirrors `watch_certificates` in `main.rs` (subscribe,
  atomic-swap on each push, exit-for-restart if the stream drops).
- `main.rs`:
  - `ProxyHttp::CTX` changes from `()` to a small struct carrying the
    resolved dispatch decision (`enum Dispatch { Backend(String), NotFound
    }`, default `Backend(host)` when no route table entry matches — this is
    the backward-compat fallback), computed once in `request_filter`.
  - `request_filter` looks up `(host, session.req_header().uri.path())` in
    the route table *before* the existing TLS-redirect logic:
    - `Redirect` match → write the 301/302/307/308 response directly (same
      shape as the existing HTTP→HTTPS 301 writer), return `Ok(true)`.
    - `Backend(service_name)` match → stash it in ctx, fall through to the
      existing ingress-country-check / TLS-redirect logic unchanged (ingress
      policy is evaluated against the *resolved backend* service, since
      that's the service actually being reached).
    - no match at all (host has explicit routes but none cover this path) →
      write a 404, return `Ok(true)`.
  - `upstream_peer` uses `ctx`'s resolved service name instead of
    recomputing it from the Host header — the only production-code change to
    this function is where `service_name` comes from.

### What is reused vs new

| Piece | Reused | New |
|---|---|---|
| Backend resolution (container discovery, VXLAN edge, `get_or_add_upstream`) | 100% — a route's `service_name` target goes through exactly today's path | — |
| Distribution to the proxy | `ArcSwap` hot-swap pattern (`CertStore`), `watch::Receiver` + `Notify` pattern (`PortMappingBundle`) | one more `Watch*` RPC + bundle type |
| Config validation | `parse_stack_content` single-source-of-truth, cross-stack conflict detection shape (`detect_port_conflicts`) | `detect_route_conflicts`, mutual-exclusion check on route target |
| Redirect response writing | the existing 301 writer in `request_filter` (host+path→Location) | parameterized status code, arbitrary target |
| Request-path awareness | — | first read of `session.req_header().uri.path()` for routing (previously unused for routing) |

## Admin UI

The UI (`members/nullnet-server/ui`, React 19 + TS + Vite, embedded into the
server binary via `rust-embed`) has no structured form anywhere today —
`Config.tsx` is a raw `<textarea>` bound 1:1 to the stack's TOML text
(GET/POST/DELETE `/api/config/{stack}`), and the services list
(`Services.tsx` / `services.rs` `ServiceJson`) doesn't even surface
`protocol`/`listen_port`. Routes get a real form, following the one existing
CRUD+modal precedent in this codebase (`pages/Users.tsx`: table + "+ Add"
button opening a shared `Modal`, per-field `useState`, busy/error state,
disabled-until-valid submit) rather than raw TOML editing or a new pattern.

### New endpoints (`nullnet-server`, alongside `config.rs`/`services.rs`)

```
GET  /api/routes/{stack}   -> { routes: RouteJson[], http_services: string[] }
POST /api/routes/{stack}   -> body: RouteJson[]  (whole-list replace)
                            -> { ok: bool, error?: string }   // same SaveResult shape as config.rs
```

`http_services` is every declared `protocol = "http"` service name in that
stack — kept as a routes-endpoint-only field rather than adding `protocol` to
`ServiceJson`, so `services.rs`/`Services.tsx` stay untouched.

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RouteTargetJson {
    Service { service: String },
    Redirect { to: String, status: u16 },
}

#[derive(Serialize, Deserialize)]
struct RouteJson { host: String, path: String, target: RouteTargetJson }
```

`POST` re-runs the same validation as the route parsing described above
(mutual exclusion, status ∈ {301,302,307,308}, referenced service exists and
is `http`, `(host, path)` uniqueness / cross-stack conflict check), then
merges the new route list into the stack's existing TOML **without
disturbing the rest of the file** (services, comments, formatting) — the
same guarantee raw-text editing already gives for everything else. Plain
`toml`/serde round-tripping loses comments/ordering on a full-file
rewrite, so this needs a structural editor (`toml_edit`, already a
lightweight transitive dependency of the `toml` ecosystem) to splice the
`[[route]]` array in place instead of reserializing the whole document.

### New page: `pages/RoutesPage.tsx`

- Table: Host | Path | Target (`→ grafana` for a service target, `redirect
  301 → https://…` for a redirect), Edit/Delete per row.
- "+ Add route" opens the shared `Modal` with: Host (text), Path (text,
  default `/`), a Service/Redirect toggle, then either a `<select>` populated
  from `http_services` or a URL text input + status `<select>`
  (301/302/307/308, default 301).
- Save posts the full edited array to `POST /api/routes/{stack}`; a
  structural/business-rule error surfaces via the same `.modal-err` pattern
  `Users.tsx` already uses.
- Scoped to the active stack via the existing `useStack()` context
  (`StackContext.tsx`), same as `Config.tsx`/`Services.tsx`.
- New nav entry in the `Ops` group (`components/Layout.tsx`, next to
  `services`/`config`) and a route in `App.tsx` (named `RoutesPage` to avoid
  colliding with `react-router-dom`'s own `Routes`), both wrapped in
  `RequireAuth` like every other page.

## Open wrinkles

- **Path matching is prefix-only**, like NGINX's plain (non-regex)
  `location` — no wildcards/regex in v1. If a follow-up needs `location ~`
  parity, that's a separate `path_regex` field on `HttpRoute`, additive.
- **No variable interpolation in redirect targets** (`$request_uri`-style).
  A path-only `to` re-appends the literal target path, not the *matched
  suffix* — `path="/old" → to="/new"` on request `/old/x` redirects to
  `/new`, not `/new/x`. If suffix-preserving redirects are needed, that's a
  `preserve_suffix: bool` flag as a follow-up, not a v1 requirement (the
  issue only asks for redirects "of course that's included," not detailed
  semantics).
- **Ingress country policy is keyed by service name today** — a redirect
  route has no backend service, so there's no ingress policy to check before
  issuing it. This is consistent with NGINX (a `return` in a `location`
  doesn't consult upstream ACLs) but worth calling out explicitly.
- **Route table size**: sent in full on every push, like certs/port-mappings.
  Fine at expected config scale (this mirrors two existing streams that
  already do this); revisit only if it becomes a real bottleneck.

## Follow-ups (out of scope for this issue)

- Regex/wildcard path matching.
- Redirect target variable interpolation (preserve matched suffix, query
  string passthrough is already implicit since redirects don't touch it).
- Header-based routing (beyond Host), if ever needed.

## Implementation status

Design only — nothing below is implemented yet. Planned as a sequence of
commits so each stage builds/tests independently:

1. **Contract** (`nullnet-grpc-lib`): `HttpRoute`, `HttpRedirect`,
   `HttpRouteBundle`, `WatchHttpRoutes` RPC.
2. **Server**: `RouteToml` parsing + validation (mutual exclusion, status
   code, same-stack service reference, `protocol = http` check),
   `detect_route_conflicts`, `build_http_route_bundle` (incl. implicit
   fallback routes), `watch_http_routes` RPC handler + its own `Notify`.
   Unit tests mirroring the existing `detect_port_conflicts`/
   `parses_explicit_and_implicit_services` coverage in `services/input.rs`.
3. **Proxy**: `routes.rs` (`RouteTable`, longest-prefix lookup,
   `watch_and_serve`), `main.rs` dispatch changes (`request_filter`
   redirect/404/backend-selection, `upstream_peer` reading resolved ctx).
   Unit tests for prefix matching, backward-compat fallback (no explicit
   routes → today's behavior), and the redirect response writer.
4. **Admin API**: `GET`/`POST /api/routes/{stack}`, `toml_edit`-based
   structural merge into the stack file.
5. **UI**: `RoutesPage.tsx`, nav entry, `Modal`-based add/edit form.
6. **Docs**: update `docs/architecture.md` if the route-table stream is
   worth calling out at the overview level once shipped.
