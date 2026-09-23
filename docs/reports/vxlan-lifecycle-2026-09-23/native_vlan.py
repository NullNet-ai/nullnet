"""Prototype: trusted service tags preserve the complete customer frame."""
import socket,struct
from netlink import U,attr
from native_xfrm import tcmsg

def vlan_action(order,vid=None,code=0):
    options=attr(2,struct.pack('IIiiii',0,0,code,0,0,1 if vid is None else 2))
    if vid is not None:options+=attr(3,struct.pack('H',vid))+attr(4,struct.pack('!H',0x88a8))
    return attr(order|32768,attr(1,b'vlan\0')+attr(2|32768,options))

def install(nl,index,vid,mark=None):
    if mark is None:nl.request(36,tcmsg(index,0xffff0000,0xfffffff1)+attr(1,b'clsact\0'),5|512|1024)
    if mark is not None:
        nl.request(45,tcmsg(index,mark,0xfffffff2,1)+attr(1,b'fw\0'))
        nl.request(45,tcmsg(index,0,0xfffffff3,1)+attr(1,b'matchall\0'))
    flags=5|1024|512
    ingress=vlan_action(1,vid)
    if mark is None:
        nl.request(44,tcmsg(index,0,0xfffffff2,1)+attr(1,b'matchall\0')+attr(2|32768,attr(2|32768,ingress)),flags)
    else:
        nl.request(44,tcmsg(index,mark,0xfffffff2,1)+attr(1,b'fw\0')+attr(2|32768,attr(4|32768,ingress)),flags)
    egress=vlan_action(1,code=3 if mark is not None else 0)
    if mark is not None:
        options=attr(2,struct.pack('IIiii',0,0,3,0,0))+attr(5,U(mark))+attr(8,U(0xffffffff))
        egress+=attr(2|32768,attr(1,b'skbedit\0')+attr(2|32768,options))
    nl.request(44,tcmsg(index,0,0xfffffff3,1)+attr(1,b'matchall\0')+attr(2|32768,attr(2|32768,egress)),flags)

def install_conditional(nl,index,vid,mark=None):
    if mark is None:nl.request(36,tcmsg(index,0xffff0000,0xfffffff1)+attr(1,b'clsact\0'),5|512|1024)
    else:
        nl.request(45,tcmsg(index,mark,0xfffffff2,1)+attr(1,b'fw\0'))
        nl.request(45,tcmsg(index,0,0xfffffff2,2)+attr(1,b'matchall\0'))
        for pref,handle,kind,field,code in [(3,mark,b'fw\0',4,0),(4,0,b'matchall\0',2,2)]:
            action=attr(1|32768,attr(1,b'gact\0')+attr(2|32768,attr(2,struct.pack('IIiii',0,0,code,0,0))))
            nl.request(44,tcmsg(index,handle,0xfffffff2,pref)+attr(1,kind)+attr(2|32768,attr(field|32768,action)),5|512|1024)
    # AD before Q prevents a newly pushed AD tag matching a second filter.
    for pref,proto in [(1,0x88a8),(2,0x8100)]:
        msg=struct.pack('B3xiIII',0,index,0,0xfffffff2,(pref<<16)|socket.htons(proto))
        nl.request(44,msg+attr(1,b'matchall\0')+attr(2|32768,attr(2|32768,vlan_action(1,vid,-1 if mark is not None else 0))),5|512|1024)
