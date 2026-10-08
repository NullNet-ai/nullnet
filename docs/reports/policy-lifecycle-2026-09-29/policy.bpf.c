#include <linux/bpf.h>
#include <linux/pkt_cls.h>
#include <linux/if_ether.h>
#include <linux/ip.h>
#include <linux/udp.h>
#define SEC(name) __attribute__((section(name), used))
#define __uint(name, val) int (*name)[val]
#define __type(name, val) val *name
#define INLINE static __attribute__((always_inline)) inline
static void *(*lookup)(void *, const void *) = (void *)BPF_FUNC_map_lookup_elem;
static long (*redirect)(__u32, __u64) = (void *)BPF_FUNC_redirect;
static __u64 (*now)(void) = (void *)BPF_FUNC_ktime_get_ns;
static long (*getkey)(struct __sk_buff *, struct bpf_tunnel_key *, __u32, __u64) = (void *)BPF_FUNC_skb_get_tunnel_key;
static long (*setkey)(struct __sk_buff *, struct bpf_tunnel_key *, __u32, __u64) = (void *)BPF_FUNC_skb_set_tunnel_key;
static long (*store)(struct __sk_buff *, __u32, const void *, __u32, __u64) = (void *)BPF_FUNC_skb_store_bytes;
struct tuple { __u32 src, dst; __u16 sport, dport; __u32 proto; };
struct permit { __u32 generation, output; __u64 deadline; __u32 transmit, pad; };
struct endpoint { __u32 index; __u8 mac[6]; __u8 pad[2]; };
struct settings { __u32 local, remote, transport, mark; };
struct { __uint(type, BPF_MAP_TYPE_HASH); __uint(max_entries, 4096); __type(key, struct tuple); __type(value, struct permit); } permits SEC(".maps");
struct { __uint(type, BPF_MAP_TYPE_HASH); __uint(max_entries, 256); __type(key, __u32); __type(value, __u32); } identities SEC(".maps");
struct { __uint(type, BPF_MAP_TYPE_HASH); __uint(max_entries, 256); __type(key, __u32); __type(value, struct endpoint); } endpoints SEC(".maps");
struct { __uint(type, BPF_MAP_TYPE_ARRAY); __uint(max_entries, 1); __type(key, __u32); __type(value, struct settings); } config SEC(".maps");
struct { __uint(type, BPF_MAP_TYPE_ARRAY); __uint(max_entries, 16); __type(key, __u32); __type(value, __u64); } counters SEC(".maps");
INLINE int count(__u32 reason, int action) { __u64 *v = lookup(&counters, &reason); if (v) __sync_fetch_and_add(v, 1); return action; }
INLINE int parse(struct __sk_buff *skb, struct tuple *key) {
 void *data = (void *)(long)skb->data, *end = (void *)(long)skb->data_end;
 struct ethhdr *eth = data; struct iphdr *ip = data + sizeof(*eth);
 if ((void *)(ip + 1) > end || eth->h_proto != __builtin_bswap16(ETH_P_IP)) return -1;
 if (ip->ihl != 5 || ip->version != 4 || (ip->frag_off & __builtin_bswap16(0x3fff))) return -1;
 if (ip->protocol != 17 && ip->protocol != 6) return -1;
 __u16 *ports = (void *)(ip + 1); if ((void *)(ports + 2) > end) return -1;
 key->src = ip->saddr; key->dst = ip->daddr; key->sport = ports[0]; key->dport = ports[1]; key->proto = ip->protocol; return 0;
}
SEC("tc") int application(struct __sk_buff *skb) {
 struct tuple key = {}; __u32 idx = skb->ifindex, zero = 0;
 if (parse(skb, &key)) return count(0, TC_ACT_SHOT);
 __u32 *identity = lookup(&identities, &idx);
 if (!identity || *identity != key.src) return count(1, TC_ACT_SHOT);
 struct permit *p = lookup(&permits, &key);
 if (!p || !p->transmit || p->deadline <= now()) return count(2, TC_ACT_SHOT);
 struct settings *cfg = lookup(&config, &zero); if (!cfg) return TC_ACT_SHOT;
 struct bpf_tunnel_key tk = {}; tk.tunnel_id = p->generation; tk.remote_ipv4 = cfg->remote; tk.local_ipv4 = cfg->local; tk.tunnel_ttl = 64;
 if (setkey(skb, &tk, sizeof(tk), BPF_F_ZERO_CSUM_TX)) return count(3, TC_ACT_SHOT);
 skb->mark = cfg->mark; count(4, 0); return redirect(cfg->transport, 0);
}
SEC("tc") int transport(struct __sk_buff *skb) {
 struct tuple key = {}; struct bpf_tunnel_key tk = {}; __u32 zero = 0;
 struct settings *cfg = lookup(&config, &zero);
 if (!cfg || skb->mark != cfg->mark || getkey(skb, &tk, sizeof(tk), 0) || tk.remote_ipv4 != cfg->remote) return count(5, TC_ACT_SHOT);
 if (parse(skb, &key)) return count(6, TC_ACT_SHOT);
 struct permit *p = lookup(&permits, &key);
 if (!p || p->deadline <= now()) return count(7, TC_ACT_SHOT);
 if (p->generation != tk.tunnel_id) return count(8, TC_ACT_SHOT);
 struct endpoint *ep = lookup(&endpoints, &key.dst);
 if (!ep || ep->index != p->output) return count(9, TC_ACT_SHOT);
 __u32 output = ep->index; __u8 mac[6]; __builtin_memcpy(mac, ep->mac, 6);
 if (store(skb, 0, mac, 6, 0)) return TC_ACT_SHOT;
 skb->mark = 0; count(10, 0); return redirect(output, 0);
}
char LICENSE[] SEC("license") = "GPL";
