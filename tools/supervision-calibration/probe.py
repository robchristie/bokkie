#!/usr/bin/env python3
"""Opt-in live calibration; temporary Unix socket, fixture-only writes, no grants.

Uses the existing Codex account/config with explicit narrower request overrides.
No account/config RPCs, TCP listener, installation or persistent service.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import tempfile
import time

MODEL = 'gpt-6-astra'


class Wire:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.settimeout(600)
        self.s.connect(str(path))
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall(('GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+key+'\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
        reply = b''
        while not reply.endswith(b'\r\n\r\n'):
            reply += self.s.recv(1)
        if b'101 Switching Protocols' not in reply:
            raise RuntimeError('Unix WebSocket upgrade failed')

    def send(self, value):
        data = json.dumps(value).encode()
        mask = os.urandom(4)
        size = len(data)
        prefix = bytes([0x81, 0x80 | size]) if size < 126 else bytes([0x81, 0xfe]) + struct.pack('!H', size)
        self.s.sendall(prefix + mask + bytes(v ^ mask[i % 4] for i, v in enumerate(data)))

    def exact(self, count):
        result = b''
        while len(result) < count:
            part = self.s.recv(count-len(result))
            if not part:
                raise EOFError('runtime connection closed')
            result += part
        return result

    def receive(self):
        while True:
            a, b = self.exact(2)
            size = b & 127
            if size == 126:
                size = struct.unpack('!H', self.exact(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.exact(8))[0]
            if b & 128:
                raise RuntimeError('unexpected masked server frame')
            payload = self.exact(size)
            if a & 15 == 8:
                raise EOFError('runtime closed WebSocket')
            if a & 15 == 1:
                return json.loads(payload)

    def close(self):
        self.s.close()


class Probe:
    def __init__(self, root, evidence):
        self.root, self.evidence = root, evidence
        self.serial, self.turns = 0, 0
        self.events = []
        self.child = None
        self.wire = None
        self.start_runtime()

    def record(self, kind, value):
        entry = {'sequence': len(self.events)+1, 'time': time.time(), 'kind': kind, 'value': value}
        self.events.append(entry)
        with self.evidence.open('a') as stream:
            stream.write(json.dumps(entry, sort_keys=True)+'\n')
            stream.flush()
            os.fsync(stream.fileno())
        print(json.dumps({'sequence':entry['sequence'], 'kind':kind}), flush=True)

    def start_runtime(self):
        path = self.root/'rpc.sock'
        path.unlink(missing_ok=True)
        self.child = subprocess.Popen(['codex', 'app-server', '--listen', 'unix://'+str(path), '-c', 'model="'+MODEL+'"', '-c', 'model_reasoning_effort="medium"', '-c', 'approval_policy="on-request"', '-c', 'sandbox_mode="workspace-write"'], cwd=self.root/'fixture', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        for _ in range(100):
            if path.exists():
                break
            if self.child.poll() is not None:
                raise RuntimeError('app-server failed to start')
            time.sleep(.1)
        self.connect()

    def connect(self):
        self.wire = Wire(self.root/'rpc.sock')
        self.rpc('initialize', {'clientInfo':{'name':'bokkie_supervision_calibration','version':'0.1'},'capabilities':{'experimentalApi':True}})
        self.wire.send({'method':'initialized','params':{}})

    def observe(self, message):
        method = message.get('method', '')
        if 'id' in message and method:
            self.record('server_request', message)
            if method == 'item/tool/requestUserInput':
                answers = {q['id']:{'answers':['teal']} for q in message['params']['questions']}
                response = {'answers':answers}
            elif 'requestApproval' in method:
                response = {'decision':'decline'}
            else:
                self.wire.send({'id':message['id'],'error':{'code':-32601,'message':'unsupported calibration request'}})
                return
            self.record('server_response', {'id':message['id'],'result':response})
            self.wire.send({'id':message['id'],'result':response})
        elif method in ['item/completed','turn/completed','turn/started','thread/status/changed']:
            self.record(method, message['params'])

    def rpc(self, method, params):
        self.serial += 1
        request_id = self.serial
        self.wire.send({'id':request_id,'method':method,'params':params})
        deadline = time.monotonic()+600
        while time.monotonic() < deadline:
            message = self.wire.receive()
            if message.get('id') == request_id and 'method' not in message:
                if 'error' in message:
                    self.record('rpc_error', {'method':method, 'error':message['error']})
                    raise RuntimeError(str(message['error']))
                return message['result']
            self.observe(message)
        raise TimeoutError(method)

    def thread(self, readonly=False):
        result = self.rpc('thread/start', {'cwd':str(self.root/'fixture'),'model':MODEL,'approvalPolicy':'on-request','sandbox':'read-only' if readonly else 'workspace-write','config':{'model_reasoning_effort':'medium'},'developerInstructions':'This is an explicitly local-only isolated calibration fixture. Do not publish, access credentials, change global config, or contact external applications. Preserve applicable engineering guidance. No delegation except the explicitly requested bounded review.'})
        self.record('thread_start', {k:v for k,v in result.items() if k != 'thread'} | {'threadId':result['thread']['id']})
        return result['thread']['id']

    def start(self, thread_id, prompt, plan=False):
        self.turns += 1
        if self.turns > 8:
            raise RuntimeError('live turn budget exhausted')
        params = {'threadId':thread_id, 'input':[{'type':'text','text':prompt}], 'effort':'medium'}
        if plan:
            params['collaborationMode'] = {'mode':'plan','settings':{'model':MODEL,'reasoning_effort':'medium','developer_instructions':None}}
        else:
            params['collaborationMode'] = {'mode':'default','settings':{'model':MODEL,'reasoning_effort':'medium','developer_instructions':None}}
        self.record('turn_request', {'number':self.turns, 'params':params})
        result = self.rpc('turn/start', params)
        return result['turn']['id']

    def finish(self, turn_id):
        deadline = time.monotonic()+600
        while time.monotonic() < deadline:
            message = self.wire.receive()
            self.observe(message)
            if message.get('method') == 'turn/completed' and message['params']['turn']['id'] == turn_id:
                return message['params']['turn']
        raise TimeoutError(turn_id)

    def command_started(self):
        deadline = time.monotonic()+120
        while time.monotonic() < deadline:
            message = self.wire.receive()
            self.observe(message)
            if message.get('method') == 'item/started' and message['params']['item']['type'] == 'commandExecution':
                self.record('command_started',message['params'])
                return
        raise TimeoutError('command did not start')

    def close(self):
        if self.wire:
            self.wire.close()
        if self.child and self.child.poll() is None:
            os.killpg(self.child.pid, signal.SIGTERM)
            try:
                self.child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.child.pid, signal.SIGKILL)
                self.child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run-live', action='store_true', required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    args.evidence = args.evidence.resolve()
    if args.evidence.exists():
        raise SystemExit('Refusing to overwrite existing evidence')
    with tempfile.TemporaryDirectory(prefix='bokkie-supervision-') as directory:
        root = Path(directory)
        fixture = root/'fixture'
        fixture.mkdir()
        (fixture/'AGENTS.md').write_text('# Calibration fixture\nUse Australian English. Include guidance-marker: gumleaf in your final report. This is local-only work; do not commit, push, open PRs or publish.\n')
        (fixture/'README.md').write_text('The probe repository computes 19 + 23. Result: 42.\n')
        subprocess.run(['git','init','-q',str(fixture)], check=True)
        probe = Probe(root,args.evidence)
        try:
            probe.record('fixture',{'path':str(fixture),'guidance_sha256':hashlib.sha256((fixture/'AGENTS.md').read_bytes()).hexdigest(),'readme_sha256':hashlib.sha256((fixture/'README.md').read_bytes()).hexdigest(),'model':MODEL,'effort':'medium','version':subprocess.check_output(['codex','--version'],text=True).strip()})
            skills = probe.rpc('skills/list', {'cwds':[str(fixture)],'forceReload':True})
            probe.record('skills',skills)
            tools = probe.rpc('mcpServerStatus/list', {'limit':100})
            probe.record('mcp_inventory', [{'name':x.get('name'),'tools':sorted(x.get('tools',{}))} for x in tools.get('data',[])])
            worker = probe.thread()
            turn = probe.start(worker,'Read README.md and report its concrete result. Read the land-reviewed-pr skill and identify its independent-review requirement, respecting this fixture local-only instruction. Report whether lantern-ui-inspection skill and a lantern executable are available, without starting any browser or server. Then call request_user_input with one question id colour, asking which colour to use (teal or amber). Wait for its answer, then report the answer and the fixture guidance marker. Do not edit files.',plan=True)
            probe.finish(turn)
            turn = probe.start(worker,'Follow up on the selected colour: create result.txt containing exactly "colour=teal; result=42\n". Emit a progress message before writing; read the file back and report the observed content and guidance marker. No other edits.')
            probe.finish(turn)
            probe.record('fixture_result',{'text':(fixture/'result.txt').read_text(),'sha256':hashlib.sha256((fixture/'result.txt').read_bytes()).hexdigest()})
            turn = probe.start(worker,'Run exactly this harmless local command: python3 -c "import time; time.sleep(20); print(42)". After it finishes, report disconnected-result=42. Do not modify files.')
            probe.command_started()
            probe.record('disconnect',{'threadId':worker,'turnId':turn})
            probe.wire.close()
            time.sleep(40)
            probe.connect()
            recovered = probe.rpc('thread/read', {'threadId':worker,'includeTurns':True})
            probe.record('read_after_disconnect',recovered)
            resumed = probe.rpc('thread/resume',{'threadId':worker})
            probe.record('resume_after_disconnect',{'status':resumed['thread']['status'],'turns':resumed['thread']['turns']})
            if resumed['thread']['status'].get('type') == 'active':
                probe.finish(turn)
            turn = probe.start(worker,'Run python3 -c "import time; time.sleep(45)" and then report interrupt-probe. No edits.')
            probe.command_started()
            probe.record('interrupt_ack',probe.rpc('turn/interrupt',{'threadId':worker,'turnId':turn}))
            probe.finish(turn)
            turn = probe.start(worker,'Run python3 -c "import time; time.sleep(45)" and then report loss-probe. No edits.')
            probe.command_started()
            probe.record('runtime_kill',{'threadId':worker,'turnId':turn,'pid':probe.child.pid})
            os.killpg(probe.child.pid,signal.SIGKILL)
            probe.child.wait()
            probe.wire.close()
            probe.start_runtime()
            probe.record('read_after_runtime_loss',probe.rpc('thread/read',{'threadId':worker,'includeTurns':True}))
            resumed = probe.rpc('thread/resume',{'threadId':worker})
            probe.record('resume_after_runtime_loss',{'status':resumed['thread']['status'],'turns':resumed['thread']['turns']})
            turn = probe.start(worker,'Read result.txt and report recovery-result=42 with the selected colour from earlier. Do not continue the cancelled sleep or edit files.')
            probe.finish(turn)
            reviewer = probe.thread(readonly=True)
            turn = probe.start(reviewer,'Perform one independent read-only review of result.txt against README.md. Verify its numerical result and colour=teal. Report concrete findings or no findings. Read land-reviewed-pr SKILL.md for review expectations, but this is a local-only calibration and no GitHub actions are authorised. Do not delegate.')
            probe.finish(turn)
            turn = probe.start(reviewer,'Approval calibration only. Request one explicit sandbox escalation via exec_command with sandbox_permissions=require_escalated to run the harmless command pwd. Do not request a prefix rule or session-wide approval. If declined, report that outcome and stop. Do not retry or modify files.')
            probe.finish(turn)
            probe.record('complete',{'live_turns':probe.turns,'max_concurrency':1,'final_fixture_result':(fixture/'result.txt').read_text()})
        except Exception as error:
            probe.record('failure',{'type':type(error).__name__,'message':str(error)})
            raise
        finally:
            probe.close()


if __name__ == '__main__':
    main()
