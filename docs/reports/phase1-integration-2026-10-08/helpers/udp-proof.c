/* LAN-only UDP fixture with exact datagram checks. */
#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
int main(int argc,char **argv) {
  if(argc!=4)return 2;
  int fd=socket(AF_INET,SOCK_DGRAM,0);if(fd<0)return 3;
  int server=!strcmp(argv[1],"server");
  struct sockaddr_in peer={.sin_family=AF_INET,.sin_port=htons(atoi(argv[server?2:3]))};
  if(server) {
    peer.sin_addr.s_addr=INADDR_ANY;if(bind(fd,(void*)&peer,sizeof peer))return 4;
    FILE *pid=fopen(argv[3],"w");if(!pid)return 5;fprintf(pid,"%d\n",getpid());fclose(pid);
    for(;;) {char data[4096];socklen_t size=sizeof peer;ssize_t n=recvfrom(fd,data,sizeof data,0,(void*)&peer,&size);if(n<0)return 6;if(sendto(fd,data,n,0,(void*)&peer,size)!=n)return 7;}
  }
  if(inet_pton(AF_INET,argv[2],&peer.sin_addr)!=1)return 8;
  if(connect(fd,(void*)&peer,sizeof peer))return 9;
  struct timeval timeout={.tv_sec=20};setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof timeout);
  for(int i=0;i<8;i++) {
    unsigned char sent[900],received[4096];for(int n=0;n<900;n++)sent[n]=(n+i)%256;
    if(send(fd,sent,sizeof sent,0)!=sizeof sent)return 10;
    ssize_t count=recv(fd,received,sizeof received,0);if(count!=sizeof sent||memcmp(sent,received,sizeof sent))return 11;
  }
  puts("{\"messages\":8,\"bytes_per_message\":900,\"errors\":0}");close(fd);return 0;
}
