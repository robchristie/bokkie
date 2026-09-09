#!/usr/bin/env python3
"""No-model-turn profile and escaped-descendant cessation probe."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

spec = importlib.util.spec_from_file_location('probe', Path(__file__).with_name('probe.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def rpc(wire, request_id, method, params):
    wire.send({'id':request_id, 'method':method, 'params':params})
    while True:
        response = wire.receive()
        if response.get('id') == request_id and 'method' not in response:
            if 'error' in response:
                raise RuntimeError(response['error'])
            return response['result']


def run():
    records = []
    with tempfile.TemporaryDirectory(prefix='bokkie-cessation-') as directory:
        root = Path(directory)
        worker = root/'daemon.py'
        worker.write_text('import os,time\nfrom pathlib import Path\np=Path(__file__).parent\nos.setsid()\n(p/"ready").write_text(str(os.getpid()))\nwhile not (p/"gate").exists(): time.sleep(.01)\n(p/"escaped-write").write_text("old writer survived")\n')
        # Positive control: this child really can escape a process group and write.
        control = subprocess.Popen(['python3',str(worker)],start_new_session=False)
        try:
            for _ in range(100):
                if (root/'ready').exists():break
                time.sleep(.01)
            (root/'gate').touch()
            control.wait(timeout=5)
            records.append({'case':'positive_control','escaped_write':(root/'escaped-write').exists()})
        finally:
            if control.poll() is None:control.kill();control.wait()
        for name in ['ready','gate','escaped-write']:(root/name).unlink(missing_ok=True)
        wrapper = root/'start.py'
        wrapper.write_text('import os,subprocess\nfrom pathlib import Path\np=Path(__file__).parent\nsubprocess.Popen(["python3",str(p/"daemon.py")],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\nos.execvp("codex",["codex","app-server","--listen","unix://"+str(p/"rpc.sock"),"-c",\'approvals_reviewer="user"\',"-c",\'approval_policy="on-request"\',"-c",\'sandbox_mode="read-only"\',"-c",\'model="gpt-6-astra"\',"-c",\'model_reasoning_effort="medium"\'])\n')
        command = ['/usr/bin/bwrap','--die-with-parent','--unshare-pid','--new-session','--dev-bind','/','/','--proc','/proc','--chdir',str(root),'--','python3',str(wrapper)]
        child = subprocess.Popen(command,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
        try:
            for _ in range(100):
                if (root/'rpc.sock').exists() and (root/'ready').exists():break
                if child.poll() is not None:raise RuntimeError('boundary start failed')
                time.sleep(.05)
            wire = module.Wire(root/'rpc.sock')
            rpc(wire,1,'initialize',{'clientInfo':{'name':'bokkie_boundary_calibration','version':'0.1'},'capabilities':{'experimentalApi':True}})
            wire.send({'method':'initialized','params':{}})
            result = rpc(wire,2,'thread/start',{'cwd':str(root),'model':'gpt-6-astra','approvalPolicy':'on-request','approvalsReviewer':'user','sandbox':'read-only'})
            records.append({'case':'boundary_profile','command':command,'effective':{k:result.get(k) for k in ['model','reasoningEffort','approvalPolicy','approvalsReviewer','sandbox','instructionSources']},'threadId':result['thread']['id'],'daemon_namespace_pid':(root/'ready').read_text()})
            wire.close()
            os.killpg(child.pid,signal.SIGTERM)
            child.wait(timeout=10)
            # The outside writer opportunity starts only after the boundary was reaped.
            (root/'gate').touch()
            time.sleep(.3)
            records.append({'case':'after_boundary_reaped','exit_code':child.returncode,'escaped_write':(root/'escaped-write').exists()})
        finally:
            if child.poll() is None:os.killpg(child.pid,signal.SIGKILL);child.wait()
    print(json.dumps(records,indent=2))
    assert records[0]['escaped_write'] and not records[-1]['escaped_write']
    assert records[1]['effective']['approvalsReviewer'] == 'user'


if __name__ == '__main__':
    run()
