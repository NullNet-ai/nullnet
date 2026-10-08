"""Inspect subscribed host netlink sockets without restarting their owners."""
import ctypes
import json
import os
from pathlib import Path
import socket

libc=ctypes.CDLL(None,use_errno=True)
libc.syscall.restype=ctypes.c_long
host_ns=os.readlink('/proc/1/ns/net')
subscribers={}
for line in Path('/proc/net/netlink').read_text().splitlines()[1:]:
    fields=line.split()
    protocol=int(fields[1])
    groups=int(fields[3],16)
    if protocol in (0,15) and groups:
        subscribers[int(fields[-1])]={'protocol':protocol,'groups':groups,'portid':int(fields[2])}
rows=[]
errors=[]
seen=set()
for process in Path('/proc').glob('[0-9]*'):
    try:
        if os.readlink(process/'ns/net')!=host_ns:
            continue
        name=(process/'comm').read_text().strip()
        for fd in (process/'fd').iterdir():
            try:
                target=os.readlink(fd)
                if not target.startswith('socket:['):
                    continue
                inode=int(target[8:-1])
                if inode not in subscribers or inode in seen:
                    continue
                seen.add(inode)
                pidfd=libc.syscall(434,int(process.name),0)
                if pidfd<0:
                    raise OSError(ctypes.get_errno(),'pidfd_open')
                duplicate=libc.syscall(438,pidfd,int(fd.name),0)
                os.close(pidfd)
                if duplicate<0:
                    raise OSError(ctypes.get_errno(),'pidfd_getfd')
                s=socket.socket(fileno=duplicate)
                length=ctypes.c_uint(0)
                result=libc.getsockopt(s.fileno(),socket.SOL_SOCKET,26,None,ctypes.byref(length))
                if result<0:
                    raise OSError(ctypes.get_errno(),'SO_GET_FILTER')
                count=length.value
                buffer=ctypes.create_string_buffer(max(1,count*8))
                if count:
                    result=libc.getsockopt(s.fileno(),socket.SOL_SOCKET,26,buffer,ctypes.byref(length))
                    if result<0:
                        raise OSError(ctypes.get_errno(),'SO_GET_FILTER data')
                row={**subscribers[inode],'pid':int(process.name),'comm':name,'fd':int(fd.name),'inode':inode,
                     'original_filter_instructions':count,'original_filter_hex':buffer.raw[:count*8].hex()}
                rows.append(row)
                s.close()
            except (OSError,ValueError) as error:
                errors.append({'pid':process.name,'fd':fd.name,'error':str(error)})
    except (FileNotFoundError,ProcessLookupError,PermissionError):
        pass
result={'sockets':rows,'errors':errors,'host_netns':host_ns}
Path('/tmp/nn-event-sockets.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
