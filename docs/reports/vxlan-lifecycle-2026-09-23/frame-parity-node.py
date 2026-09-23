"""Exact Ethernet-frame comparison, including hardware-stripped VLAN headers."""
import json,os,select,socket,struct,sys,time

CASES=[[],[(0x8100,1)],[(0x8100,2)],[(0x8100,4094)],[(0x8100,0xa001)],[(0x88a8,2)],[(0x8100,1),(0x8100,2)],[(0x88a8,2),(0x8100,4094)]]
def frame(i,tags):
    return b'\xff'*6+bytes.fromhex('0223aabbccdd')+b''.join(struct.pack('!HH',*t) for t in tags)+b'\x88\xb5'+b'NN23_PARITY_'+str(i).encode()+b'_'*64
def sock(pid,iface):
    home=os.open('/proc/self/ns/net',os.O_RDONLY);target=os.open(f'/proc/{pid}/ns/net',os.O_RDONLY)
    try:
        os.setns(target,os.CLONE_NEWNET);s=socket.socket(socket.AF_PACKET,socket.SOCK_RAW,socket.htons(3));s.bind((iface,0));s.setsockopt(263,8,1)
    finally:os.setns(home,os.CLONE_NEWNET);os.close(home);os.close(target)
    return s

if __name__=='__main__':
    pids=json.load(open('/tmp/nn23-wire-pids.json'));mode=sys.argv[1]
    if mode=='send':
        s=sock(pids[0],'in0')
        for i,tags in enumerate(CASES):
            for _ in range(3):s.send(frame(i,tags));time.sleep(.01)
    else:
        ss={sock(pids[i%len(pids)],f'in{i}'):i for i in range(8)};rows=[];deadline=time.monotonic()+4
        print('READY',flush=True)
        while time.monotonic()<deadline:
            ready,_,_=select.select(list(ss),[],[],.1)
            for s in ready:
                data,anc,_,address=s.recvmsg(65536,1024)
                if b'NN23_PARITY_' not in data or address[2]==4:continue
                for level,kind,value in anc:
                    if level==263 and kind==8:
                        status,_,_,_,_,tci,tpid=struct.unpack('IIIHHHH',value[:20])
                        if status&16:data=data[:12]+struct.pack('!HH',tpid if status&64 else 0x8100,tci)+data[12:]
                rows.append({'endpoint':ss[s],'frame':data.hex()})
        print(json.dumps(rows),flush=True)
