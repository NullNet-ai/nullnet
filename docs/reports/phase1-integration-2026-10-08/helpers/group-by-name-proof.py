import subprocess,socket,struct,json
subprocess.run(['ip','link','add','nn_name_proof','type','dummy'],check=True)
def attr(kind,value):
 length=4+len(value);return struct.pack('HH',length,kind)+value+b'\0'*((-length)%4)
body=bytes(16)+attr(3,b'nn_name_proof\0')+attr(27,struct.pack('I',0x4e400000));s=socket.socket(socket.AF_NETLINK,socket.SOCK_RAW,0);s.bind((0,0));s.sendto(struct.pack('IHHII',16+len(body),19,5,1,0)+body,(0,0));reply=s.recv(65536);assert struct.unpack_from('i',reply,16)[0]==0,reply.hex()
row=json.loads(subprocess.check_output(['ip','-j','link','show','nn_name_proof']))[0];assert row['ifname']=='nn_name_proof' and int(row['group'])==0x4e400000,row;print(json.dumps({'name_lookup_setlink_ack':True,'ifname':row['ifname'],'group':row['group']}))
