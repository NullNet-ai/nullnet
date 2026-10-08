"""Authenticated lab API helper; cookies remain in memory."""
import http.cookiejar,json,ssl,urllib.request
BASE='https://192.168.1.104:8080';STACK='nn-phase1'
jar=http.cookiejar.CookieJar()
op=urllib.request.build_opener(urllib.request.HTTPSHandler(context=ssl._create_unverified_context()),urllib.request.HTTPCookieProcessor(jar))
def api(path,data=None):
 request=urllib.request.Request(BASE+path,json.dumps(data).encode() if data is not None else None,{'Content-Type':'application/json'})
 with op.open(request,timeout=45) as r:
  body=r.read();return json.loads(body) if body else None

def login():api('/api/auth/login',{'username':'admin','password':'admin'})
if __name__=='__main__':
 login()
 services=[{'name':f'p1-{i:02d}.test','docker_container':f'nn-phase1-{i:02d}','host_ip':'192.168.1.103' if i<=6 else '192.168.1.104','port':9000+i,'timeout':1,'pausable':False} for i in range(1,13)]
 print(json.dumps(api('/api/service-config/'+STACK,{'services':services})))
 state=api('/api/services/'+STACK);print(json.dumps(state)[:2400])
