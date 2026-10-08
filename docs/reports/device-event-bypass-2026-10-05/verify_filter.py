"""Exercise the actual kernel filter without touching listener sockets."""
import ctypes
import json
from pathlib import Path
import struct
import socket_filter as sf


def execute(fd,data):
    # TEST_RUN pulls an Ethernet header; actual netlink sockets have none.
    data=b'\0'*12+b'\x08\x00'+data
    input_buffer=ctypes.create_string_buffer(data)
    output_buffer=ctypes.create_string_buffer(8192)
    attribute=ctypes.create_string_buffer(144)
    struct.pack_into('IIIIQQI',attribute,0,fd,0,len(data),len(output_buffer),
                     ctypes.addressof(input_buffer),ctypes.addressof(output_buffer),1)
    sf.bpf(10,attribute)
    return struct.unpack_from('I',attribute,4)[0]


def attr(kind,value):
    result=struct.pack('HH',4+len(value),kind)+value
    return result+b'\0'*((-len(result))%4)


def message(kind,body,seq=0,flags=0):
    return struct.pack('IHHII',16+len(body),kind,flags,seq,0)+body


def link(name,index=0x4e123400,seq=0,flags=0,kind=16):
    return message(kind,struct.pack('BBHiII',0,0,0,index,0,0)+attr(3,name.encode()+b'\0'),seq,flags)


owned=sf.map_create(9,4,4,65536,'nn_evt_test')
counters=sf.map_create(6,4,8,5,'nn_evt_test_ct')
uevent=sf.uevent_program(counters).load_kernel('nn_evt_test_u')
route=sf.route_program(owned,counters).load_kernel('nn_evt_test_r')
results=[]


def check(label,fd,data,drop):
    returned=execute(fd,data)
    assert (returned==0)==drop,(label,returned,drop)
    results.append({'case':label,'dropped':returned==0})


for name in ['br_1960000_s','ns_1960000_s-o','nnv_1960000_s','nnb_3','nnp_app','veth-1960000-s','macsec-1960000s']:
    for action in ['add','remove','change','move']:
        data=(action+'@/devices/virtual/net/'+name+'\0ACTION='+action+'\0SUBSYSTEM=net\0INTERFACE='+name+'\0').encode()
        check('uevent-'+action+'-'+name,uevent,data,True)
    check('link-'+name,route,link(name),True)
for name in ['ens18','docker0','veth123abc','veth-42p','veth-42','br_other','ns_other','macsec-42wrong']:
    data=('add@/devices/virtual/net/'+name+'\0ACTION=add\0SUBSYSTEM=net\0INTERFACE='+name+'\0').encode()
    check('uevent-preserve-'+name,uevent,data,False)
    check('link-preserve-'+name,route,link(name),False)
for name,drop in [('veth-1960000-s',True),('macsec-1960000s',True),('nnv_1960000_s',True),
                  ('veth123abc',False),('veth-42p',False),('macsec-42wrong',False)]:
    for action in ['add','remove']:
        data=(action+'@/devices/virtual/net/'+name+'/queues/tx-0\0ACTION='+action+'\0SUBSYSTEM=queues\0').encode()
        check('queue-'+action+'-'+name,uevent,data,drop)
check('unicast-request-reply',route,link('nnv_1960000_s',seq=123),False)
check('multipart-dump',route,link('nnv_1960000_s',flags=2),False)
echo=bytearray(link('nnv_1960000_s',seq=123))
struct.pack_into('I',echo,12,0x4e123499)
check('own-unicast-reply',route,bytes(echo),False)
struct.pack_into('I',echo,12,0x9e123456)
check('other-socket-multicast-echo',route,bytes(echo),True)
check('mixed-datagram',route,link('nnv_1960000_s')+link('ens18'),False)
index=0x4e123401
check('track-owned-index',route,link('nnv_1960000_s',index),True)
address=struct.pack('BBBBI',2,29,0,0,index)+attr(1,b'\x0a\x00\x00\x01')
check('address-owned',route,message(20,address),True)
route_body=struct.pack('BBBBBBBBI',2,29,0,0,254,2,253,1,0)+attr(1,b'\x0a\x00\x00\x00')+attr(4,struct.pack('I',index))
check('route-owned',route,message(24,route_body),True)
neighbor=struct.pack('BBHiHBB',2,0,0,index,0,0,0)+attr(1,b'\x0a\x00\x00\x02')
check('neighbor-owned',route,message(28,neighbor),True)
check('reuse-index-foreign',route,link('veth123abc',index),False)
check('address-after-index-reuse',route,message(20,address),False)
check('route-after-index-reuse',route,message(24,route_body),False)
check('neighbor-after-index-reuse',route,message(28,neighbor),False)
Path('/tmp/nn-event-filter-tests.json').write_text(json.dumps(results,indent=2)+'\n')
print(json.dumps({'kernel_filter_cases':len(results),'result':'passed'}))
