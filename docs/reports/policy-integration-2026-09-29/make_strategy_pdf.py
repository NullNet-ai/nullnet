"""Generate a concise vector explanation of the policy lifecycle strategy."""
from pathlib import Path
from reportlab.pdfgen import canvas
from reportlab.lib.colors import HexColor, white
from reportlab.lib.pagesizes import A4

OUT=Path(__file__).with_name('nullnet-policy-strategy.pdf')
W,H=A4
INK=HexColor('#142D3B');MUTED=HexColor('#526674');TEAL=HexColor('#007D78');BLUE=HexColor('#2766A0');RED=HexColor('#B63D42');PALE=HexColor('#EFF5F6');LINE=HexColor('#CBDADD')
c=canvas.Canvas(str(OUT),pagesize=A4)
c.setTitle('Nullnet | On-demand connections, persistent transport')
c.setAuthor('Nullnet')

def text(x,y,s,size=11,color=INK,bold=False):
 c.setFillColor(color);c.setFont('Helvetica-Bold' if bold else 'Helvetica',size);c.drawString(x,y,s)
def lines(x,y,rows,size=11,color=INK,gap=16):
 for row in rows:text(x,y,row,size,color);y-=gap
 return y
def box(x,y,w,h,title,sub=(),color=TEAL):
 c.setFillColor(PALE);c.setStrokeColor(LINE);c.roundRect(x,y,w,h,9,fill=1,stroke=1)
 text(x+12,y+h-22,title,11,color,True)
 lines(x+12,y+h-41,sub,9,MUTED,13)
def arrow(x1,y,x2,color=TEAL,dash=False):
 c.setStrokeColor(color);c.setFillColor(color);c.setLineWidth(2)
 c.setDash(4,3) if dash else c.setDash()
 c.line(x1,y,x2-6,y);c.setDash();p=c.beginPath();p.moveTo(x2,y);p.lineTo(x2-7,y+4);p.lineTo(x2-7,y-4);p.close();c.drawPath(p,fill=1,stroke=0)
def header(page,kicker,title,subtitle):
 text(40,H-39,'NULLNET / ARCHITECTURE',10,TEAL,True)
 text(W-121,H-39,'29 SEP 2026',9,MUTED)
 text(40,H-76,title,23,INK,True)
 text(40,H-99,subtitle,11,MUTED)
 text(40,28,kicker,8,MUTED);text(W-58,28,str(page),9,MUTED)

header(1,'STRATEGY / SHARED HOST TRANSPORT','One transport. Separate app permissions.','VXLAN is shared by applications on the hosts; it is not created per app pair.')
text(40,699,'Example: allow App A to talk to App B',13,INK,True)
# Host boundaries contain their application attachments and VXLAN device.
for x,host in [(40,'HOST 1'),(330,'HOST 2')]:
 c.setFillColor(PALE);c.setStrokeColor(LINE);c.roundRect(x,359,225,319,10,fill=1,stroke=1)
 text(x+14,654,host,12,INK,True)
for x,label in [(55,'App A'),(170,'App C'),(345,'App B'),(460,'App D')]:
 box(x,578,80,49,label,('app veth end',),INK)
 text(x+5,562,'own veth pair',8,MUTED)
 c.setStrokeColor(LINE);c.line(x+40,551,x+40,527)
for x in [55,345]:
 box(x,456,195,70,'Host veth ends + eBPF',('Maps hold app-pair permissions.', 'Unlisted pairs are dropped.'))
 c.setStrokeColor(TEAL);c.line(x+97,456,x+97,440)
 box(x,383,195,56,'ONE VXLAN device',('Receive-side eBPF gate.',),BLUE)
arrow(250,410,345,BLUE)
text(266,437,'IPsec',10,BLUE,True)
text(269,423,'host ↔ host',8,BLUE)
text(40,331,'App A → B: passes both hosts’ permission checks.',11,TEAL,True)
text(40,310,'App C → D: dropped. A shared VXLAN is not a shared app LAN.',10.5,RED)
text(40,288,'No Linux bridge: eBPF redirects packets between the veth and VXLAN interfaces.',9,MUTED)
text(40,270,'What is shared?',14,INK,True)
lines(40,246,['One VXLAN device per host; one encrypted transport per host pair.',
              'Each app namespace has its own attachment and separate pair permissions.'],10.5,INK,19)
text(40,198,'Attachment = a veth pair (virtual Ethernet cable).',12,INK,True)
lines(40,174,['App end: an interface in the app’s namespace, with a fixed local IP and route.',
              'Host end: a separate interface where eBPF checks the app’s packets.',
              'It connects the app to the gate; it grants no permission by itself.',
              'For the proxy, the app end is in the host’s own network namespace.'],10,INK,16)
box(40,55,515,63,'Created once, reused',('Attach the app when first needed. Opening / closing an app-pair connection',
 'adds / removes permissions; the attachment and host transport stay in place.'))
text(40,44,'First integration: opt-in, encrypted cross-host paths. Other paths retain the existing backend.',8,MUTED)
c.showPage()
header(2,'STRATEGY / PERMISSION LIFECYCLE','A permission is a packet-checking rule.','Nullnet installs these rules in eBPF maps on both hosts.')
text(40,698,'Example permission: App A ↔ App B, generation 42',13,INK,True)
text(40,682,'This grants connectivity between two app namespaces; it is not a per-port firewall rule.',9,MUTED)
box(40,583,515,95,'What the rule contains',(
 'Local app attachment + local and remote connection IP addresses.',
 'Expected remote host + authenticated encrypted transport.',
 'Generation number + send-enabled state. A host-wide deadline must also be valid.'),BLUE)
text(40,552,'What happens to a packet?',14,INK,True)
box(40,448,247,84,'1  Sending host',('Find rule for this attachment + peer.',
 'Check enabled state and deadline.', 'Translate app IP to connection IP.', 'Add generation; send via encrypted VXLAN.'))
box(307,448,248,84,'2  Receiving host',('Check authenticated host + app addresses.',
 'Match generation; check deadline.', 'Translate to the app’s local IP.', 'Deliver only to the named app attachment.'))
arrow(289,490,303)
text(40,423,'No matching rule, stale generation or expired deadline → drop.',11,RED,True)
text(40,382,'Open, use, close',14,INK,True)
box(40,299,515,63,'Prepare → activate',('Install receive rules on both hosts while sends remain blocked.',
 'After both hosts acknowledge preparation, enable sends.'))
box(40,219,515,63,'Revoke',('Delete the pair’s rules on both hosts. Further packets are dropped.',
 'Keep the shared devices and other app pairs’ permissions.'),RED)
box(40,139,515,63,'Reuse safely',('Use a new generation when a connection slot is reused.',
 'Old-generation packets fail the checks; socket reuse also needs explicit fencing.'),BLUE)
lines(40,105,['These are host-enforced rules, not credentials given to applications.',
              'Control heartbeats renew the shared deadline; loss of renewal blocks traffic.'],10,INK,17)
text(40,57,'Revocation stops further traffic; it does not close application sockets or recall admitted packets.',8,MUTED)
c.showPage()
header(3,'STRATEGY / GENERATIONS','A generation identifies one use of a slot.','The connection address may be reused. Its generation must change.')
text(40,699,'Example: the same connection slot, used twice',13,INK,True)
box(40,596,247,78,'First use: generation 42',('Nullnet grants App A ↔ App B.',
 'Both hosts store generation 42.', 'Packets carry 42 through the tunnel.'),TEAL)
box(307,596,248,78,'Next use: generation 43',('Nullnet revokes the first permission.',
 'Same pair or new pair; fresh permission.', 'Both hosts now expect generation 43.'),BLUE)
arrow(289,635,303,BLUE)
text(40,563,'The number is assigned by the controller, not the application.',10.5,INK)
text(40,528,'A delayed packet arrives after reuse',14,INK,True)
box(40,443,151,63,'Old packet: 42',('Already sent before revoke.',),RED)
box(234,428,321,93,'Receiver expects: 43',('42 ≠ 43 → DROP',
 'A matching app address is not enough.', 'New packets carrying 43 can pass the other checks.'),RED)
arrow(192,473,230,RED)
text(40,393,'How the check works',14,INK,True)
lines(40,368,['1. On allocation, the controller assigns a fresh, increasing number.',
              '2. It sends the same number to both hosts over the TLS control channel.',
              '3. The sender’s eBPF rule writes that number into VXLAN metadata.',
              '4. IPsec protects the packet and its generation in transit.',
              '5. The receiver requires an exact match with its current permission.'],10.5,INK,21)
text(40,242,'A generation is not a password or a timeout.',12,INK,True)
lines(40,218,['It separates successive grants that reuse an address or connection slot.',
              'IPsec authenticates the remote host; the heartbeat deadline controls expiry.'],10,INK,18)
box(40,108,515,72,'Why TCP needs another check',('An old socket can emit a new packet after reuse; the sender would label it 43.',
 'The integration therefore records the generation when a TCP SYN opens a flow.',
 'Later TCP packets must match that flow generation; an old established flow stays blocked.'),BLUE)
text(40,81,'Integration encoding: 40 bits = 24-bit VXLAN VNI + 16-bit GBP policy ID; do not wrap.',8.5,MUTED)
text(40,65,'Restart boundary: fresh transport keys distinguish controller / host incarnations.',8.5,MUTED)
c.showPage()
header(4,'EVIDENCE / INTEGRATION BOUNDARY','Hundreds of full cycles per second.','Measured in Nullnet on two Linux hosts with eight test containers each.')
text(40,700,'Complete connection cycles per second',14,INK,True)
text(40,678,'Encrypted HTTP connection setup + traffic + completed retirement',10,MUTED)
rows=[('Dedicated pooled backend','69','Median, 3 × 1,024'),('Policy backend','743','Median, 3 × 1,024'),('Policy, sustained load','≥1,716','60 s + final drain')]
for i,(label,value,note) in enumerate(rows):
 y=603-i*66;box(40,y,515,56,label,(),TEAL);text(308,y+20,value,22,TEAL,True);text(418,y+23,note,9,MUTED)
text(40,438,'107,696 complete cycles in the sustained run. Zero failures.',13,INK,True)
lines(40,414,['Two Linux hosts; eight test containers per host; concurrency 64.',
              'The sustained run reuses infrastructure; every counted connection is new.'],10,MUTED,16)
text(40,357,'Why the hot path is smaller',14,INK,True)
box(40,278,247,60,'Dedicated topology',('Create / configure / delete devices',),BLUE)
box(307,278,248,60,'Policy lifecycle',('Update / delete permission entries',),TEAL)
arrow(289,308,303)
text(40,254,'Control-plane map work: 6 updates + 4 deletions per two-host cycle.',10,MUTED)
text(40,237,'Host mappings and control acknowledgements still run for each connection.',9,MUTED)
text(40,218,'What the integration preserves',14,INK,True)
lines(40,192,['Bind identity to the application attachment; authenticate the remote host.',
              'Reject old generations; expire permissions when renewal stops.',
              'Retain dedicated paths for same-host networks, backend triggers and egress.',
              'Define socket reuse: revocation blocks traffic but does not close sockets.'],10.5,INK,19)
text(40,95,'Product checks: isolation, generation / TCP fencing, heartbeat expiry,',9,MUTED)
text(40,81,'mixed-topology drain and restart recovery passed. Experimental and opt-in.',9,MUTED)
text(40,60,'Evidence and current status: docs/reports/policy-integration-2026-09-29/README.md',8,MUTED)
c.save()
print(OUT)
