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

# new — strip the matched prefix before forwarding (NGINX
# `proxy_pass http://backend/;` trailing-slash equivalent): "api" sees
# "/users", not "/api/users"
[[route]]
host = "ops.example.com"
path = "/api"
service = "api"
strip_prefix = true

# new — carry the matched suffix and the original query string into a
# redirect (NGINX `rewrite ^/old(.*) /new$1 permanent;` equivalent):
# /old/x/y?foo=bar -> /new/x/y?foo=bar
[[route]]
host = "old.example.com"
path = "/old"
redirect_to = "/new"
preserve_path = true
preserve_query = true
```

`service` and `redirect_to` are mutually exclusive (exactly one required),
mirroring how `*_blocked_countries`/`*_allowed_countries` are already
validated as mutually exclusive in `input.rs`. `redirect_status` is
restricted to `301 | 302 | 307 | 308`. `strip_prefix` only applies to
`service` routes; `preserve_path`/`preserve_query` only apply to
`redirect_to` routes — each is rejected on the wrong target kind. All three
default to `false`, so every existing config is unaffected.

### Example: the issue's own scenario

One host fronting three apps by path (`/api`, `/grafana`, `/gitlab`, as named
in the issue), plus a legacy-domain redirect:

```toml
# services/ops.toml

# --- Backends (unchanged from today — these are just services) ---

[[services]]
name = "api"
docker_container = "api-backend"
port = 8080
timeout = 30

[[services]]
name = "grafana"
docker_container = "grafana"
port = 3000
timeout = 30

[[services]]
name = "gitlab"
docker_container = "gitlab"
port = 80
timeout = 60

# --- Routes: NGINX location-block equivalent for ops.example.com ---

[[route]]
host = "ops.example.com"
path = "/api"
service = "api"

[[route]]
host = "ops.example.com"
path = "/grafana"
service = "grafana"

[[route]]
host = "ops.example.com"
path = "/gitlab"
service = "gitlab"

# Catch-all for this host — anything not matching a more specific prefix
# above falls here. Without this, unmatched paths on ops.example.com 404.
[[route]]
host = "ops.example.com"
path = "/"
service = "grafana"

# --- A literal redirect: legacy domain -> the new one, no backend needed ---

[[route]]
host = "old-ops.example.com"
path = "/"
redirect_to = "https://ops.example.com/"
redirect_status = 301
```

Resulting behavior:

| Request | Result |
|---|---|
| `https://ops.example.com/api/users` | proxied to `api` (port 8080) — longest matching prefix is `/api` |
| `https://ops.example.com/grafana/d/xyz` | proxied to `grafana` (port 3000) |
| `https://ops.example.com/gitlab/` | proxied to `gitlab` (port 80) |
| `https://ops.example.com/anything-else` | proxied to `grafana` — the catch-all `path = "/"` |
| `https://old-ops.example.com/whatever` | `301 https://ops.example.com/` |

A single-service host with **no** `[[route]]` block at all keeps working
exactly as it does today (the implicit `{host = name, path = "/"} →
service(name)` fallback from the backward-compatibility section above) — you
only add `[[route]]` entries for hosts that actually need path-based fan-out
or a redirect.

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
  // Only meaningful when target = service_name: strip path_prefix from the
  // path forwarded to the backend (NGINX `proxy_pass http://backend/;`
  // trailing-slash equivalent). Default false forwards the path unchanged.
  bool strip_prefix = 5;
}

message HttpRedirect {
  string to = 1;              // absolute URL, or a path template — see
                               // "Redirect target" below
  uint32 status_code = 2;     // 301/302/307/308
  bool preserve_path = 3;     // append the request path's matched suffix to `to`
  bool preserve_query = 4;    // append the original request's query string
}

message HttpRouteBundle {
  repeated HttpRoute routes = 1;
}

// Long-lived stream, mirrors WatchCertificates/WatchPortMappings: full table
// on subscribe, one push per config change.
rpc WatchHttpRoutes(Empty) returns (stream HttpRouteBundle);
```

No change to `ProxyRequest`/`Upstream`/`Proxy` RPC — once a route resolves to
a `service_name`, everything downstream (container discovery, VXLAN edge
setup, `get_or_add_upstream`) is untouched.

### Redirect target

`to` is either an absolute URL (used verbatim) or starts with `/` (path-only
— same scheme/host as the incoming request, target path substituted).
`preserve_path` appends the request path's suffix beyond the matched
`path_prefix` (NGINX `rewrite ^/old(.*) /new$1 permanent;` equivalent);
`preserve_query` appends the original request's `?query` string, merged with
`&` if `to` already carries its own. Both default to `false`, reproducing
`to` used verbatim — the only behavior before these two fields existed.

**Correction to an earlier version of this doc**: it previously claimed
query-string passthrough was "already implicit since redirects don't touch
it." That was wrong — the original implementation never touched the request
path/query at all in either direction, so nothing was preserved by default.
`preserve_query`/`preserve_path` are the fix, not a refinement of existing
passthrough.

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
      #[serde(default)]
      strip_prefix: bool,
      redirect_to: Option<String>,
      redirect_status: Option<u16>,
      #[serde(default)]
      preserve_path: bool,
      #[serde(default)]
      preserve_query: bool,
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
  - `strip_prefix` is rejected on a `redirect_to` route; `preserve_path`/
    `preserve_query` are rejected on a `service` route — each only makes
    sense for its own target kind;
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

### Path rewrite additions (`strip_prefix` / `preserve_path` / `preserve_query`)

Added on top of the v1 design above, closing three gaps identified after the
initial implementation landed:

- `routes.rs`'s `RouteTable::resolve` computes the request path's suffix
  beyond the matched `path_prefix` (via `str::strip_prefix`) and returns it
  as part of the match — `RouteMatch::Backend { service_name, forward_path
  }` (the path to actually forward — rewritten when `strip_prefix` is set,
  via a `normalize_forward_path` helper that guarantees the result is a
  valid absolute path even when stripping would otherwise leave `""`) or
  `RouteMatch::Redirect { .., matched_suffix }` (the raw suffix; `main.rs`
  combines it with the request's query string and Host header, which
  `routes.rs` has no access to).
- `main.rs`'s `ProxyCtx` gains `forward_path: Option<String>`, set in
  `request_filter` alongside `service_name`. `upstream_request_filter`
  applies it via a new `rewrite_uri_path` helper (`RequestHeader::set_uri`
  with the new path, existing query string preserved) — the client-facing
  session and every log line still show the *original* path; only the
  request actually sent upstream changes.
- `resolve_redirect_target` gained `matched_suffix`/`preserve_path`/
  `preserve_query` parameters: splits any query already in the configured
  `to`, optionally appends the matched suffix to the path portion, then
  optionally appends the request's own query (merged with `&`, not
  overwritten), before applying the existing absolute-URL-vs-relative-path
  logic unchanged.

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
- **Resolved**: matched-suffix and query-string preservation in redirects
  (`preserve_path`/`preserve_query`), backend path rewriting
  (`strip_prefix`) — see "Path rewrite additions" above — and admin UI form
  support for all three (checkboxes on the Add/Edit modal, `RouteTargetJson`
  updated in `types.ts`).
- **Ingress country policy is keyed by service name today** — a redirect
  route has no backend service, so there's no ingress policy to check before
  issuing it. This is consistent with NGINX (a `return` in a `location`
  doesn't consult upstream ACLs) but worth calling out explicitly.
- **Route table size**: sent in full on every push, like certs/port-mappings.
  Fine at expected config scale (this mirrors two existing streams that
  already do this); revisit only if it becomes a real bottleneck.

## Implementation status

Implemented on `feature/proxy-redirects-137`:

1. **Contract** (`nullnet-grpc-lib`): `HttpRoute`, `HttpRedirect`,
   `HttpRouteBundle`, `WatchHttpRoutes` RPC. ✅ builds + tested.
2. **Server**: `RouteToml` parsing + validation (mutual exclusion, status
   code, same-stack service reference, `protocol = http` + proxy-reachable
   check), `detect_route_conflicts`, `build_http_route_bundle` (incl. implicit
   fallback routes), `watch_http_routes` RPC handler + its own `Notify`,
   `route_conflict` server event. `[[services]]` is now `#[serde(default)]`
   so a stack can be routes-only (a bare redirect needs no backend at all).
   Unit tests mirroring the existing `detect_port_conflicts`/
   `parses_explicit_and_implicit_services` coverage in `services/input.rs`,
   plus `build_http_route_bundle` coverage in `nullnet_grpc_impl.rs`. ✅ 154
   tests pass.
3. **Proxy**: `routes.rs` (`RouteTable`, longest-prefix lookup,
   `watch_and_serve`), `main.rs` dispatch changes (`request_filter`
   redirect/404/backend-selection via a new `ProxyCtx`, `upstream_peer`
   reading the resolved service name). Unit tests for prefix matching,
   backward-compat fallback (no explicit routes → today's behavior), and
   redirect resolution. ✅ 22 tests pass.
4. **Admin API**: `GET`/`POST /api/routes/{stack}` (`http_server/routes.rs`),
   `toml_edit`-based structural merge into the stack file (new
   `toml_edit` workspace dependency).
5. **UI**: `pages/Routes.tsx` (table + add/edit `Modal`, following the
   `Users.tsx` CRUD pattern), nav entry, `RouteJson`/`RoutesResponseJson`
   types. Add/Edit modal has checkboxes for `strip_prefix`/`preserve_path`/
   `preserve_query`, each with a one-line explainer; the routes table's
   target label shows which are set (e.g. `→ api (strip prefix)`). ✅
   `tsc -b` + `vite build` + `eslint` clean.
6. **Docs**: not yet updated — `docs/architecture.md` doesn't currently
   enumerate individual watch streams, so no change was needed there.
7. **Path rewrite additions** (`strip_prefix`, `preserve_path`,
   `preserve_query`): proto fields, server validation (each rejected on the
   wrong target kind), `RouteTable::resolve`'s `forward_path`/
   `matched_suffix` computation, `main.rs`'s `rewrite_uri_path` +
   `resolve_redirect_target` rewrite, and the UI form fields above. ✅ 161
   server tests, 36 proxy tests pass (found and fixed a genuine bug in the
   process — the original `resolve_redirect_target` never touched the
   request's path/query at all, contrary to what an earlier version of this
   doc claimed).

Manually verified end-to-end by the repo owner against a real deployment
(`nullnet-server` + `nullnet-proxy` + `nullnet-client`, real backend
containers) — confirmed working, including hot-reload of route changes. A
"not working" symptom hit during that verification turned out to be a stale
`nullnet-proxy` binary (right commit checked out, binary not rebuilt from
it), not a code bug.

`cargo fmt`/`cargo clippy -D warnings` clean for `nullnet-grpc-lib`,
`nullnet-server`, `nullnet-proxy` (the three crates CI lints/tests).

## Follow-ups (out of scope for this issue)

- Regex/wildcard path matching.
- Header-based routing (beyond Host), if ever needed.
