# nullnet

**Routing in the dark 🥷**

Nullnet is a control plane for default-deny networking: no host can reach another until a
real request arrives, and the path that carries it is built for that request and torn down
when it goes idle.

> _Networks that don't exist until they're needed &bull; No standing connections &bull; No attack surface_

## The idea

Most networks connect every machine to every other machine, then bolt a firewall on top to
take reachability away.<br>
The attack surface starts as the whole network and shrinks only as far as the rules manage to carve it down.

Nullnet inverts that.<br>
By default **nothing can talk to anything**.<br>
When a service is actually requested — from the outside through the proxy, or by another service —
the server walks the dependency chain that request will travel and opens every link along it at once,
so the full path is ready atomically.<br>
Each link is a private Linux VXLAN tunnel between exactly two machines, encrypted,
and alive only while traffic is flowing through it.

There is no standing internal network for an intruder to move around in, because there is no
standing internal network at all — only the narrow, temporary paths that current legitimate
traffic has asked for.

## How it works

Three binaries coordinate over a gRPC control plane:

| Component          | What it does                                                                                                                                                                                                                         |
|--------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **nullnet-proxy**  | It's the front door. Requests arrive looking for a named service; the proxy asks the server to build the path that delivers them. Terminates TLS, does host/path routing, and forwards raw tcp/udp on demand.                        |
| **nullnet-server** | It's the brain, and the only piece that sees the whole picture. Holds the topology of which services may talk to which, decides when to build each link, and tears it down once it's idle.                                           |
| **nullnet-client** | It runs on each machine using eBPF. Announces the services running locally, and watches for them reaching out. It intercepts the first packet of a new connection, asks the server for a path, and releases it once the path exists. |

<p align="center"><img src="docs/architecture.png" alt="nullnet architecture"></p>

## What you get

- **On-demand tunnels** — point-to-point VXLAN links, built atomically across a whole
  dependency chain, encrypted by default (MACsec / XFRM), and reclaimed once the connections
  they carry are gone.
- **Default-deny host firewall** — an eBPF classifier on every node's uplink NIC, with one
  global allowlist decided server-side so there's a single point of configuration.
- **Ingress that knows what it's serving** — Let's Encrypt certificates with automatic
  renewal, `Host`+path routing, redirects, and raw tcp/udp listeners opened live from config.
- **Geo policy in both directions** — restrict which countries a service may reach, and
  which may reach it, enforced at the initiator and at the proxy chokepoint.
- **An admin UI that shows the live topology** — services, sessions, per-edge state, and a
  persisted event log of everything the control plane did and why.

## Getting started

Setup, configuration, and the full env-var / service-config reference live in
**[SETUP.md](SETUP.md)**.

```bash
git clone https://github.com/NullNet-ai/nullnet.git /root/nullnet
cd /root/nullnet
# then fill in each component's members/nullnet-*/.env — see SETUP.md
./setup-server.sh     # on the control-plane host
./setup-proxy.sh      # on the ingress host
./setup-client.sh     # on every machine running services
```
