"""Dedicated bridge reuse: anonymous idle slots contain no endpoint state."""
import concurrent.futures as cf,json,os,socket,struct,subprocess
from netlink import NL,U,attr,info
GROUP=0x4e670001

def create(i):
    n=NL()
    try:
        n.create(f'pool_{i}','bridge',b'',attr(1,b'\x02'+os.urandom(5)))
        n.request(19,info(n.get(f'pool_{i}'))+attr(27,U(GROUP)))
    finally:n.s.close()

def lease(n,i,name):
    index=n.get(f'pool_{i}')
    n.request(19,info(index)+attr(3,name.encode()+b'\0'))
    return index

def release(indices,command):
    def down(pair):
        i,index=pair;n=NL()
        try:n.request(19,info(index,0,1)+attr(3,f'pool_{i}'.encode()+b'\0')+attr(27,U(GROUP)))
        finally:n.s.close()
    with cf.ThreadPoolExecutor(32) as ex:list(ex.map(down,indices.items()))
    links=json.loads(command(['ip','-j','addr','show']));owned=set(indices.values());n=NL()
    try:
        for link in links:
            if link['ifindex'] not in owned:continue
            for a in link.get('addr_info',[]):
                family=2 if a['family']=='inet' else 10;raw=socket.inet_pton(family,a['local'])
                n.request(21,struct.pack('BBBBI',family,a['prefixlen'],0,0,link['ifindex'])+attr(1,raw)+attr(2,raw))
        byname={f'pool_{i}':index for i,index in indices.items()}
        for row in json.loads(command(['ip','-j','neigh','show'])):
            if row.get('dev') not in byname:continue
            family=10 if ':' in row['dst'] else 2
            n.request(29,struct.pack('B3xiHBB',family,byname[row['dev']],0,0,0)+attr(1,socket.inet_pton(family,row['dst'])))
    finally:n.s.close()

def verify(command):
    links=json.loads(command(['ip','-j','addr','show']))
    pools=[l for l in links if l['ifname'].startswith('pool_')]
    assert all(not l.get('addr_info') and 'UP' not in l['flags'] for l in pools)
    assert not any(r.get('dev','').startswith('pool_') for r in json.loads(command(['ip','-j','neigh','show'])))
    fdb=json.loads(command(['bridge','-j','fdb','show']))
    assert all('permanent' in r.get('state',[]) for r in fdb if r.get('dev','').startswith('pool_'))
    names={l['ifname'] for l in pools}
    assert all(not r.get('mdb') for r in json.loads(command(['bridge','-j','mdb','show'])) if r.get('dev') in names)
    return {'idle_bridges':len(pools),'addresses':0,'neighbors':0,'dynamic_fdb':0,'mdb':0}
