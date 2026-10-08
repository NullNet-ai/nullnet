"""Detach only this session's filters from originally unfiltered, unchanged sockets."""
import ctypes,json,os,socket,struct
from compiled_filters import pinned,update
import socket_filter as sf
libc=ctypes.CDLL(None,use_errno=True);libc.syscall.restype=ctypes.c_long
rows=json.load(open('/tmp/nn-layer1-before-sockets.json'))['sockets'];restored=[]
lease=pinned('EVENT_LEASE');update(lease,struct.pack('I',0),struct.pack('Q',0));mapping=pinned('EVENT_SOCKETS')
for row in rows:
 if row['original_filter_instructions']!=0:continue
 pidfd=libc.syscall(434,row['pid'],0)
 if pidfd<0:continue
 fd=libc.syscall(438,pidfd,row['fd'],0);os.close(pidfd)
 if fd<0:continue
 try:
  if os.fstat(fd).st_ino!=row['inode']:continue
  s=socket.socket(fileno=os.dup(fd));cookie=s.getsockopt(socket.SOL_SOCKET,57,8);s.close()
  key=ctypes.create_string_buffer(cookie);value=ctypes.create_string_buffer(4);attribute=ctypes.create_string_buffer(144)
  struct.pack_into('I',attribute,0,mapping);struct.pack_into('QQ',attribute,8,ctypes.addressof(key),ctypes.addressof(value))
  try:sf.bpf(1,attribute)
  except OSError:continue
  if struct.unpack('I',value.raw)[0]!=row['portid']:continue
  length=ctypes.c_uint(0);result=libc.getsockopt(fd,1,26,None,ctypes.byref(length))
  if result==0:assert length.value==0;continue
  assert ctypes.get_errno()==13
  zero=ctypes.c_int(0);assert libc.setsockopt(fd,1,27,ctypes.byref(zero),4)==0
  length=ctypes.c_uint(0);assert libc.getsockopt(fd,1,26,None,ctypes.byref(length))==0 and length.value==0
  restored.append({'pid':row['pid'],'fd':row['fd'],'inode':row['inode']})
 finally:os.close(fd)
assert len(restored)==7,restored
for name in ['nullnet_event_route','nullnet_event_uevent','EVENT_LEASE','EVENT_SOCKETS','EVENT_OWNED','EVENT_COUNTS']:
 os.unlink('/sys/fs/bpf/nullnet-device-events-v1/'+name)
os.rmdir('/sys/fs/bpf/nullnet-device-events-v1')
result={'result':'passed','originally_empty_filters_restored':restored,'prototype_pins_removed':True}
open('/tmp/nn-layer1-filter-restoration.json','w').write(json.dumps(result,indent=2)+'\n');print(json.dumps({'filters_restored':len(restored),'prototype_pins_removed':True}))
