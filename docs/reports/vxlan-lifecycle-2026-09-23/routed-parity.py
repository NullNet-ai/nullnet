"""Source routing, per-gateway forwarding rules and two-stage SNAT proof."""
import json

def prove(w):
    pids={s:json.loads(w.remote(s,['cat','/tmp/nn23-wire-pids.json']).stdout) for s in [0,1]}
    def ep(side,args,check=True):return w.remote(side,['nsenter','-t',str(pids[side][1]),'-n',*args],check)
    for side in [0,1]:
        w.net(side,['ip','link','add','routed','type','veth','peer','name','route-in','netns',str(pids[side][1])])
        w.net(side,['ip','addr','add',f'198.19.{side}.1/24','dev','routed']);w.net(side,['ip','link','set','routed','up'])
        ep(side,['ip','addr','add',f'198.19.{side}.2/24','dev','route-in']);ep(side,['ip','link','set','route-in','up'])
        w.net(side,['sysctl','-w','net.ipv4.ip_forward=1']);w.net(side,['iptables','-P','FORWARD','DROP'])
    ep(0,['ip','route','add','198.19.1.2/32','via','198.19.0.1','dev','route-in'])
    w.net(0,['ip','rule','add','from','198.19.0.2','lookup','321','priority','1000'])
    w.net(0,['ip','route','add','default','via','10.223.0.4','dev','gw0','table','321'])
    w.net(0,['iptables','-t','nat','-A','POSTROUTING','-s','198.19.0.2','-o','gw0','-j','SNAT','--to-source','10.223.0.3'])
    w.net(1,['iptables','-t','nat','-A','POSTROUTING','-s','10.223.0.0/24','-o','routed','-j','MASQUERADE'])
    for side,a,b in [(0,'routed','gw0'),(1,'gw0','routed')]:
        w.net(side,['iptables','-A','FORWARD','-i',a,'-o',b,'-j','ACCEPT'])
        w.net(side,['iptables','-A','FORWARD','-i',b,'-o',a,'-m','conntrack','--ctstate','ESTABLISHED,RELATED','-j','ACCEPT'])
        w.net(side,['iptables','-t','mangle','-A','FORWARD','-p','tcp','--tcp-flags','SYN,RST','SYN','-j','TCPMSS','--set-mss','1040'])
    result={'ping':ep(0,['ping','-n','-c','3','-W','1','198.19.1.2'],False).returncode}
    return result|{'rules':[w.net(s,['iptables','-nvL','FORWARD']).stdout for s in [0,1]],'nat':[w.net(s,['iptables','-t','nat','-nvL','POSTROUTING']).stdout for s in [0,1]]}
