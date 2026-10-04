#!/usr/bin/env python3
"""Run the qualified disposable Engine configuration on the operator's host.

This is deliberately not a service/deployment launcher. The Docker socket never
enters the container; only synthetic disposable state is mounted.
"""
import argparse
import hashlib
import http.client
import json
from pathlib import Path
import re
import socket
import subprocess
import uuid

HERE = Path(__file__).resolve().parent


class EngineConnection(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect('/var/run/docker.sock')


def api(method, path, value=None):
    connection = EngineConnection('localhost', timeout=30)
    try:
        connection.request(method, '/v1.53' + path,
                           body=None if value is None else json.dumps(value),
                           headers={'Content-Type': 'application/json'})
        response = connection.getresponse()
        body = response.read()
        if response.status >= 300:
            raise RuntimeError(f'Engine {method} {path}: {response.status} {body.decode()}')
        return json.loads(body) if body else None
    finally:
        connection.close()


def configuration(image, profile, name):
    paths = json.loads((HERE / 'policy/paths.json').read_text())
    seccomp = json.loads((HERE / 'policy/seccomp.json').read_text())
    return {'Image': image, 'User': '10001:10001', 'Cmd': ['hold'],
            'Labels': {'bokkie.boundary': name},
            'HostConfig': {
                'Init': True, 'NetworkMode': 'none', 'ReadonlyRootfs': True,
                'CapDrop': ['ALL'], 'SecurityOpt': [
                    'no-new-privileges:true', 'apparmor=' + profile,
                    'seccomp=' + json.dumps(seccomp, separators=(',', ':'))],
                'PidsLimit': 96, 'Memory': 768 * 1024**2, 'NanoCpus': 1000000000,
                'Tmpfs': {'/tmp': 'rw,nosuid,nodev,size=128m,mode=1777',
                          '/home/probe/.codex': 'rw,nosuid,nodev,size=16m,uid=10001,gid=10001,mode=700'},
                'Mounts': [{'Type': 'volume', 'Source': name, 'Target': '/data'}],
                **paths}}


def validate_effective(expected, actual):
    for key, value in expected['HostConfig'].items():
        if actual['HostConfig'][key] != value:
            raise ValueError('effective HostConfig differs: ' + key)
    host = actual['HostConfig']
    if (host['Privileged'] or host['CapAdd'] or host['PortBindings'] or
            host['PidMode'] or host['IpcMode'] == 'host' or host['UsernsMode']):
        raise ValueError('unexpected outer authority')
    if (actual['Config']['User'] != '10001:10001' or
            actual['Config']['Labels']['bokkie.boundary'] != expected['Labels']['bokkie.boundary'] or
            actual['Image'] != expected['Image'] or not actual['State']['Running']):
        raise ValueError('effective identity differs')


def run(image, profile, source, destination):
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image):
        raise ValueError('immutable image ID required')
    if not re.fullmatch(r'bokkie-boundary-[a-z0-9-]{8,48}', profile):
        raise ValueError('dedicated boundary profile required')
    if not re.fullmatch(r'[0-9a-f]{40}', source):
        raise ValueError('full source revision required')
    destination.mkdir(parents=True, exist_ok=False)

    def record(name, value):
        (destination / (name + '.json')).write_text(json.dumps(value, indent=2) + '\n')

    info = api('GET', '/info')
    version = api('GET', '/version')
    record('daemon', {'info': info, 'version': version})
    if (version['Version'] != '29.8.1' or version['Arch'] != 'amd64' or
            info['KernelVersion'] != '6.12.73+deb13-amd64' or
            set(info['SecurityOptions']) != {'name=apparmor,profile=default',
                                            'name=seccomp,profile=builtin', 'name=cgroupns'}):
        raise ValueError('target differs from the qualified rootful Nostromo configuration')
    image_info = api('GET', '/images/' + image + '/json')
    if image_info['Config']['Labels'].get('org.opencontainers.image.revision') != source:
        raise ValueError('image source label differs')
    record('inputs', {'source': source, 'image': image_info, 'profile': profile,
                      'rendered_apparmor_sha256': hashlib.sha256(
                          (HERE / 'policy/apparmor.profile').read_text().replace('BOKKIE_PROFILE', profile).encode()).hexdigest(),
                      'policy_sha256': {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                        for p in (HERE / 'policy').iterdir() if p.is_file()}})
    name = 'bokkie-boundary-' + uuid.uuid4().hex
    config = configuration(image, profile, name)
    record('requested', config)
    container = None
    volume_created = False
    try:
        api('POST', '/volumes/create', {'Name': name, 'Labels': {'bokkie.boundary': name}})
        volume_created = True
        container = api('POST', '/containers/create?name=' + name, config)['Id']
        api('POST', '/containers/' + container + '/start')
        actual = api('GET', '/containers/' + container + '/json')
        record('effective', actual)
        validate_effective(config, actual)
        for probe in ('boundary', 'lifecycle', 'preflight'):
            result = subprocess.run(['docker', '--host', 'unix:///var/run/docker.sock', 'exec',
                                     container, 'python3', '/opt/probe.py', probe],
                                    capture_output=True, text=True, timeout=60)
            record(probe, {'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr})
            print(json.dumps({'probe': probe, 'exit': result.returncode}), flush=True)
            if result.returncode:
                raise RuntimeError(probe + ' failed; see ' + str(destination / (probe + '.json')))
            assert api('GET', '/containers/' + container + '/json')['State']['Running']
        record('result', {'result': 'passed', 'source': source, 'image': image,
                          'credentials': False, 'model_calls': 0})
    finally:
        if container:
            api('DELETE', '/containers/' + container + '?force=true')
        if volume_created:
            api('DELETE', '/volumes/' + name)
        record('cleanup', {'container': container, 'volume': name, 'removed': True})


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--profile', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    run(args.image, args.profile, args.source, args.evidence)
