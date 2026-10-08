/* Cached-image fixture: static HTTP, payload, and declared/backend dependency trees. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>
static int listener;
static char node[16];
static int send_all(int fd, const char *p, size_t n) {
  while (n) { ssize_t sent = send(fd,p,n,MSG_NOSIGNAL); if(sent<=0)return -1; p+=sent;n-=sent; }
  return 0;
}
static int child(const char *host, unsigned port, char *out, size_t capacity) {
  char line[1024], ip[64], names[900], resolved[64]="";
  FILE *hosts=fopen("/etc/hosts","r"); if(!hosts)return -1;
  while(fgets(line,sizeof line,hosts)) {
    if(sscanf(line,"%63s %899[^\n]",ip,names)!=2)continue;
    for(char *word=strtok(names," \t");word;word=strtok(NULL," \t")) {
      if(word[0]=='#')break; if(!strcmp(word,host))snprintf(resolved,sizeof resolved,"%s",ip);
    }
  }
  fclose(hosts); struct sockaddr_in address={.sin_family=AF_INET,.sin_port=htons(port)};
  if(inet_pton(AF_INET,resolved,&address.sin_addr)!=1)return -1;
  int fd=socket(AF_INET,SOCK_STREAM,0);if(fd<0)return -1;
  struct timeval timeout={.tv_sec=30};setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof timeout);setsockopt(fd,SOL_SOCKET,SO_SNDTIMEO,&timeout,sizeof timeout);
  if(connect(fd,(struct sockaddr*)&address,sizeof address)) {close(fd);return -1;}
  char request[300];int length=snprintf(request,sizeof request,"GET /tree HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",host);
  if(send_all(fd,request,length)) {close(fd);return -1;}
  char response[65536];size_t total=0;ssize_t received;
  while(total<sizeof response-1 && (received=recv(fd,response+total,sizeof response-1-total,0))>0)total+=received;
  close(fd);response[total]=0;
  if(strncmp(response,"HTTP/1.1 200",12))return -1;
  char *body=strstr(response,"\r\n\r\n");if(!body)return -1;body+=4;
  size_t bytes=total-(body-response);if(bytes>=capacity)return -1;memcpy(out,body,bytes);return bytes;
}
static void serve(int fd) {
  struct timeval timeout={.tv_sec=30};setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof timeout);
  char request[4096];ssize_t bytes=recv(fd,request,sizeof request-1,0);if(bytes<=0)return;request[bytes]=0;
  char path[256];if(sscanf(request,"GET %255s",path)!=1)return;
  char body[65536];size_t size=snprintf(body,sizeof body,"node=%s\n",node);int status=200;
  if(!strcmp(path,"/tree") || !strcmp(path,"/cgi-bin/tree")) {
    char filename[128];snprintf(filename,sizeof filename,"/srv/children/%s",node);FILE *children=fopen(filename,"r");
    if(children) {char host[128];unsigned port;while(fscanf(children,"%127[^:]:%u\n",host,&port)==2) {
      int received=child(host,port,body+size,sizeof body-size);if(received<0){status=500;break;}size+=received;
    }fclose(children);}
  }
  if(!strcmp(path,"/payload.bin")) {
    char header[200];int length=snprintf(header,sizeof header,"HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n");
    if(send_all(fd,header,length))return;char payload[4096];for(int i=0;i<4096;i++)payload[i]=i%256;
    for(int i=0;i<256;i++)if(send_all(fd,payload,sizeof payload))break;return;
  }
  char header[200];int length=snprintf(header,sizeof header,"HTTP/1.1 %d Result\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",status,size);
  if(!send_all(fd,header,length))send_all(fd,body,size);
}
static void *worker(void *unused) {
  (void)unused;for(;;){int fd=accept(listener,NULL,NULL);if(fd<0){if(errno==EINTR)continue;return NULL;}serve(fd);close(fd);}
}
int main(int argc,char **argv) {
  if(argc!=2)return 2;snprintf(node,sizeof node,"%s",getenv("NN_NODE")?getenv("NN_NODE"):"?");signal(SIGPIPE,SIG_IGN);
  listener=socket(AF_INET,SOCK_STREAM,0);int one=1;setsockopt(listener,SOL_SOCKET,SO_REUSEADDR,&one,sizeof one);
  struct sockaddr_in address={.sin_family=AF_INET,.sin_port=htons(atoi(argv[1])),.sin_addr.s_addr=INADDR_ANY};
  if(bind(listener,(struct sockaddr*)&address,sizeof address)||listen(listener,256))return 3;
  pthread_t threads[8];for(int i=0;i<8;i++)pthread_create(&threads[i],NULL,worker,NULL);for(int i=0;i<8;i++)pthread_join(threads[i],NULL);return 0;
}
