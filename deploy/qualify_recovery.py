#!/usr/bin/env python3
"""Crash only the marked synthetic deployment and prove ordered systemd recovery.

The operator must start bokkie-calibration.service with the deployment controller
and ExecStopPost contract first. This does not interrupt Docker or the host.
"""
from pathlib import Path
import json,subprocess,time,os,signal,sys
root=Path(sys.argv[1]).resolve(strict=True)
assert (root/'.synthetic-bokkie-deployment').read_text() == 'Synthetic deployment qualification only\n'
config=json.loads((root/'release.json').read_text())
assert config['name']=='bokkie-calibration' and config['codex_auth'] is None
def run(args):return subprocess.check_output(args,text=True,timeout=10,stderr=subprocess.PIPE).strip()
def inspect(name):return json.loads(run(['docker','--host','unix:///var/run/docker.sock','inspect','bokkie-calibration-'+name]))[0]
def pair():return [inspect('runtime'),inspect('edge')]
def check_ready():
    a,b=pair()
    assert a['State']['Running'] and b['State']['Running']
    assert a['Config']['Labels']['bokkie.deployment']=='bokkie-calibration'
    assert b['Config']['Labels']['bokkie.deployment']=='bokkie-calibration'
    assert b['HostConfig']['NetworkMode']=='container:'+a['Id']
    ns=run(['docker','--host','unix:///var/run/docker.sock','exec',a['Id'],'readlink','/proc/self/ns/net'])
    assert ns==run(['docker','--host','unix:///var/run/docker.sock','exec',b['Id'],'readlink','/proc/self/ns/net'])
    return a,b
rows=[]
for failure in ('runtime','edge','controller','explicit-restart'):
    a,b=check_ready();before=[a['Id'],b['Id']]
    if failure in ('runtime','edge'):
        target=a if failure=='runtime' else b
        run(['docker','--host','unix:///var/run/docker.sock','kill','--signal=KILL',target['Id']])
    elif failure=='controller':
        pid=int(run(['systemctl','--user','show','bokkie-calibration','-p','MainPID','--value']))
        assert str(root/'source/deploy/manage.py').encode() in Path(f'/proc/{pid}/cmdline').read_bytes()
        os.kill(pid,signal.SIGKILL)
    else:run(['systemctl','--user','restart','bokkie-calibration'])
    deadline=time.monotonic()+35
    while True:
        try:
            c,d=check_ready()
            assert c['Id']!=before[0] and d['Id']!=before[1]
            response=subprocess.run(['python3',str(root/'source/deploy/qualify.py'),str(root)],capture_output=True,text=True,timeout=15)
            assert response.returncode==0,response.stderr
            break
        except (AssertionError,subprocess.CalledProcessError):
            if time.monotonic()>deadline:raise
            time.sleep(.3)
    rows.append({'failure':failure,'before':before,'after':[c['Id'],d['Id']],'namespace_shared':True,'persistence_and_rotated_session':True})
    print(json.dumps(rows[-1]),flush=True)
(root/'recovery.json').write_text(json.dumps({'rows':rows,'image':c['Image'],'unit':'Nostromo transient user systemd service; production boot dependencies not exercised'},indent=2))
