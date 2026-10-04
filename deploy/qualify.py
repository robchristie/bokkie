#!/usr/bin/env python3
"""Finite HTTP/persistence qualification using only the marked synthetic stack.

Usage: python3 deploy/qualify.py /absolute/calibration-root
Run once after creation and once after ordered recreation. The synthetic login
must never be installed on the real Bokkie route. No model calls are made.
"""
from pathlib import Path
import base64,hashlib,http.client,json,socket,ssl,subprocess,sys,uuid
root=Path(sys.argv[1]).resolve(strict=True)
assert (root/'.synthetic-bokkie-deployment').read_text() == 'Synthetic deployment qualification only\n'
cfg=json.loads((root/'release.json').read_text()); host=cfg['hostname']
assert cfg['name']=='bokkie-calibration' and cfg['codex_auth'] is None
assert cfg['conversation_profile'] is None
inspect=json.loads(subprocess.check_output(['docker','--host','unix:///var/run/docker.sock','inspect',cfg['name']+'-runtime']))[0]
ip=inspect['NetworkSettings']['Networks']['proxy']['IPAddress']
auth='Basic '+base64.b64encode(b'calibration:bokkie-synthetic-only').decode()
class TLS(http.client.HTTPSConnection):
    def connect(self):
        self.sock=self._context.wrap_socket(socket.create_connection(('192.168.50.20',443),timeout=5),server_hostname=self.host)
def request(path, *, mode='tls', method='GET', authenticated=True, extra=None, body=None):
    c=TLS(host,timeout=8) if mode=='tls' else http.client.HTTPConnection(ip,8080,timeout=5)
    h={'Host':host}
    if authenticated:h['Authorization']=auth
    if extra:h.update(extra)
    if body is not None:body=json.dumps(body);h['Content-Type']='application/json'
    c.request(method,path,body=body,headers=h); r=c.getresponse(); data=r.read(); c.close()
    return r.status,data
rows=[]
def check(name,expected,**kw):
    status,data=request(**kw); assert status==expected,(name,status,data[:200]);rows.append({'case':name,'status':status}); return data
for mode in ('tls','direct'):
    for path in ('/','/ui/','/bootstrap','/health'):
        check(mode+' anonymous '+path,401,path=path,mode=mode,authenticated=False)
    check(mode+' wrong host',404 if mode=='tls' else 421,path='/bootstrap',mode=mode,extra={'Host':'wrong.yutani.tech'})
    check(mode+' wrong origin',403,path='/bootstrap',mode=mode,extra={'Origin':'https://wrong.yutani.tech','X-Forwarded-Host':host,'X-Forwarded-Proto':'https'})
    check(mode+' UI',200,path='/ui/',mode=mode)
    check(mode+' root UI redirect',307,path='/',mode=mode)
try:
    socket.create_connection((ip,7744),timeout=2)
    raise AssertionError('backend reachable via bridge')
except (ConnectionRefusedError,TimeoutError):rows.append({'case':'bridge backend denied','passed':True})
bootstrap=json.loads(check('bootstrap',200,path='/bootstrap'))
token=bootstrap['mutation_token']
check('missing mutation token',403,path='/obligations',method='POST',body={'description':'must not exist'})
check('cross site mutation',403,path='/obligations',method='POST',body={'description':'must not exist'},extra={'X-Bokkie-Mutation-Token':token,'Sec-Fetch-Site':'cross-site'})
marker=root/'persistence.json'
if not marker.exists():
    identifier='deployment-'+uuid.uuid4().hex
    result=json.loads(check('persist future obligation',201,path='/obligations',method='POST',body={'id':identifier,'description':'Synthetic deployment persistence check','scheduled_at':4102444800},extra={'X-Bokkie-Mutation-Token':token,'Origin':'https://'+host,'Sec-Fetch-Site':'same-origin'}))
    marker.write_text(json.dumps({'id':identifier,'session':bootstrap['service']['session_id'],'token_sha256':hashlib.sha256(token.encode()).hexdigest()}))
else:
    old=json.loads(marker.read_text());identifier=old['id']
    assert old['session']!=bootstrap['service']['session_id'],'session did not rotate'
    assert old['token_sha256']!=hashlib.sha256(token.encode()).hexdigest()
    rows.append({'case':'restart rotates session/token','passed':True})
check('persisted obligation',200,path='/obligations/'+identifier)
record={'image':inspect['Image'],'source':cfg['source'],'container':inspect['Id'],'rows':rows,'tls':'system trust, exact hostname/SNI, explicit diagnostic address; no normal DNS claim','credentials':'synthetic only','model_calls':0}
print(json.dumps(record,indent=2))
