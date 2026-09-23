"""Named network namespaces on dedicated threads; /run/netns is preinitialized."""
import ctypes,os
libc=ctypes.CDLL(None,use_errno=True)
libc.mount.argtypes=[ctypes.c_char_p,ctypes.c_char_p,ctypes.c_char_p,ctypes.c_ulong,ctypes.c_void_p]
libc.umount2.argtypes=[ctypes.c_char_p,ctypes.c_int]
def checked(result):
 if result:raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
def create(path):
 original=os.open('/proc/thread-self/ns/net',os.O_RDONLY);created=False;mounted=False
 try:
  fd=os.open(path,os.O_RDONLY|os.O_CREAT|os.O_EXCL,0);os.close(fd);created=True
  os.unshare(os.CLONE_NEWNET)
  checked(libc.mount(b'/proc/thread-self/ns/net',os.fsencode(path),b'none',4096,None));mounted=True
 except BaseException:
  if created and not mounted:os.unlink(path)
  raise
 finally:os.setns(original,os.CLONE_NEWNET);os.close(original)
def remove(path):
 checked(libc.umount2(os.fsencode(path),2));os.unlink(path)
