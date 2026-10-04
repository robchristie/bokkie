#!/usr/bin/env python3
"""Host-side owner of Bokkie's exact-policy runtime and Compose ingress.

This operator tool has Docker administration authority. Neither application
container receives the Docker socket. A single foreground owner supervises the
pair; Docker restart policies are deliberately disabled.
"""
import argparse
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import threading
import time

SOURCE = Path(__file__).resolve().parents[1]
POLICY = SOURCE / 'tools/container-probe/policy'
STOP = threading.Event()
DOCKER = ['docker', '--host', 'unix:///var/run/docker.sock']


class EngineConnection(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect('/var/run/docker.sock')


def api(method, path, value=None, missing=False):
    connection = EngineConnection('localhost', timeout=45)
    try:
        connection.request(method, '/v1.53' + path,
                           body=None if value is None else json.dumps(value),
                           headers={'Content-Type': 'application/json'})
        response = connection.getresponse()
        body = response.read()
        if response.status == 404 and missing:
            return None
        if response.status >= 300:
            raise RuntimeError(f'Engine {method} {path}: {response.status} {body.decode()}')
        return json.loads(body) if body else None
    finally:
        connection.close()


def load(root):
    config = json.loads((root / 'release.json').read_text())
    required = {'name', 'source', 'image', 'edge_image', 'hostname', 'uid', 'gid',
                'data', 'web_auth', 'codex_auth', 'conversation_profile'}
    if set(config) != required:
        raise ValueError('release.json fields differ from the deployment contract')
    if not re.fullmatch(r'bokkie(?:-[a-z0-9]{1,24})?', config['name']):
        raise ValueError('invalid deployment name')
    if not re.fullmatch(r'[0-9a-f]{40}', config['source']):
        raise ValueError('full source revision required')
    for key in ('image', 'edge_image'):
        if not re.fullmatch(r'sha256:[0-9a-f]{64}', config[key]):
            raise ValueError('immutable local image ID required: ' + key)
    if not re.fullmatch(r'[a-z0-9]+(?:[.-][a-z0-9]+)*\.yutani\.tech', config['hostname']):
        raise ValueError('a canonical yutani.tech hostname is required')
    if any(type(config[key]) is not int or not 1 <= config[key] <= 65534
           for key in ('uid', 'gid')):
        raise ValueError('runtime UID/GID must be explicitly non-root')
    for key in ('data', 'web_auth', 'codex_auth'):
        value = config[key]
        if key == 'codex_auth' and value is None:
            continue
        if not isinstance(value, str) or not Path(value).is_absolute() or not Path(value).exists():
            raise ValueError('existing absolute path required: ' + key)
    if not Path(config['data']).is_dir() or not Path(config['web_auth']).is_file():
        raise ValueError('data must be a directory and web_auth a file')
    if config['codex_auth'] is not None and not Path(config['codex_auth']).is_file():
        raise ValueError('codex_auth must be a single existing credential file')
    if (config['codex_auth'] is None) != (config['conversation_profile'] is None):
        raise ValueError('account and conversation profile must be enabled together')
    if config['conversation_profile'] is not None:
        profile = config['conversation_profile']
        if not isinstance(profile, dict) or not isinstance(profile.get('model'), str):
            raise ValueError('explicit conversation profile required')
        if any(profile.get(k) != v for k, v in {
                'broker': '/opt/conversation/broker.py', 'codex': '/usr/local/bin/codex',
                'bwrap': '/usr/bin/bwrap'}.items()):
            raise ValueError('conversation executable paths must use the packaged runtime')
    return config


def profile_name(config):
    digest = hashlib.sha256((POLICY / 'apparmor.profile').read_bytes()).hexdigest()[:16]
    return config['name'] + '-' + digest


def runtime(config, root):
    name = config['name'] + '-runtime'
    uid, gid = config['uid'], config['gid']
    mounts = [{'Type': 'bind', 'Source': config['data'], 'Target': '/data'}]
    command = ['--database', '/data/bokkie.sqlite', 'serve', '--bind', '127.0.0.1:7744',
               '--public-origin', 'https://' + config['hostname'], '--ui-dir', '/opt/ui',
               '--enable-local-notes']
    if config['codex_auth'] is not None:
        mounts += [
            {'Type': 'bind', 'Source': config['codex_auth'],
             'Target': '/home/probe/.codex/auth.json', 'ReadOnly': True},
            {'Type': 'bind', 'Source': str(root / 'conversation.json'),
             'Target': '/opt/conversation-profile.json', 'ReadOnly': True}]
        command += ['--conversation-profile', '/opt/conversation-profile.json']
    labels = {'bokkie.deployment': config['name'],
              'traefik.enable': 'true', 'traefik.docker.network': 'proxy',
              f'traefik.http.routers.{config["name"]}.rule': f'Host(`{config["hostname"]}`)',
              f'traefik.http.routers.{config["name"]}.entrypoints': 'websecure',
              f'traefik.http.routers.{config["name"]}.tls': 'true',
              f'traefik.http.services.{config["name"]}.loadbalancer.server.port': '8080'}
    return {'Image': config['image'], 'User': f'{uid}:{gid}',
            'Entrypoint': ['/usr/local/bin/bokkie'], 'Cmd': command,
            'Env': ['HOME=/home/probe', 'PATH=/usr/local/bin:/usr/bin:/bin'],
            'Labels': labels, 'ExposedPorts': {'8080/tcp': {}},
            'HostConfig': {
                'Init': True, 'NetworkMode': 'proxy', 'ReadonlyRootfs': True,
                'CapDrop': ['ALL'], 'SecurityOpt': [
                    'no-new-privileges:true', 'apparmor=' + profile_name(config),
                    'seccomp=' + json.dumps(json.loads((POLICY / 'seccomp.json').read_text()),
                                             separators=(',', ':'))],
                'RestartPolicy': {'Name': 'no', 'MaximumRetryCount': 0},
                'PidsLimit': 96, 'Memory': 768 * 1024**2, 'NanoCpus': 1000000000,
                'LogConfig': {'Type': 'json-file', 'Config': {'max-size': '10m', 'max-file': '3'}},
                'Tmpfs': {'/tmp': 'rw,nosuid,nodev,size=128m,mode=1777',
                          '/home/probe/.codex': f'rw,nosuid,nodev,size=16m,uid={uid},gid={gid},mode=700'},
                'Mounts': mounts, **json.loads((POLICY / 'paths.json').read_text())},
            'NetworkingConfig': {'EndpointsConfig': {'proxy': {'Aliases': [name]}}}}


def edge(config, root):
    # The existing credential file is root-owned mode0600. A single nginx
    # process reads it directly, without copying credentials or gaining caps.
    return {'name': config['name'], 'services': {'edge': {
        'image': config['edge_image'], 'pull_policy': 'never',
        'container_name': config['name'] + '-edge',
        'network_mode': 'container:' + config['name'] + '-runtime',
        'user': '0:0', 'read_only': True, 'cap_drop': ['ALL'],
        'security_opt': ['no-new-privileges:true'], 'restart': 'no',
        'entrypoint': ['nginx'], 'command': ['-c', '/etc/nginx/nginx.conf', '-g', 'daemon off;'],
        'pids_limit': 32, 'mem_limit': '64m', 'cpus': 0.25,
        'labels': {'bokkie.deployment': config['name'], 'traefik.enable': 'false'},
        'tmpfs': ['/tmp:rw,nosuid,nodev,noexec,size=16m,mode=1777'],
        'volumes': [
            {'type': 'bind', 'source': str(root / 'nginx.conf'),
             'target': '/etc/nginx/nginx.conf', 'read_only': True},
            {'type': 'bind', 'source': config['web_auth'],
             'target': '/run/bokkie-web-auth', 'read_only': True}],
        'logging': {'driver': 'json-file', 'options': {'max-size': '10m', 'max-file': '3'}}}}}


def render(config, root):
    hostname = config['hostname']
    (root / 'nginx.conf').write_text(f'''user root root;
master_process off;
pid /tmp/nginx.pid;
error_log /dev/stderr warn;
events {{ worker_connections 128; }}
http {{
  access_log off;
  client_body_temp_path /tmp/client;
  proxy_temp_path /tmp/proxy;
  fastcgi_temp_path /tmp/fastcgi;
  uwsgi_temp_path /tmp/uwsgi;
  scgi_temp_path /tmp/scgi;
  server {{
    listen 8080 default_server;
    server_name {hostname};
    if ($http_host != "{hostname}") {{ return 421; }}
    auth_basic "Bokkie";
    auth_basic_user_file /run/bokkie-web-auth;
    client_max_body_size 128k;
    location = / {{ rewrite ^ /ui/ last; }}
    location / {{
      proxy_pass http://127.0.0.1:7744;
      proxy_set_header Host $http_host;
      proxy_set_header Authorization "";
      proxy_set_header Forwarded "";
      proxy_read_timeout 200s;
      proxy_buffering off;
    }}
  }}
}}
''')
    (root / 'compose.json').write_text(json.dumps(edge(config, root), indent=2) + '\n')
    (root / 'apparmor.profile').write_text((POLICY / 'apparmor.profile').read_text().replace(
        'BOKKIE_PROFILE', profile_name(config)))
    if config['conversation_profile'] is not None:
        (root / 'conversation.json').write_text(json.dumps(config['conversation_profile']) + '\n')


def compose(root, *arguments):
    environment = {k: v for k, v in os.environ.items()
                   if not k.startswith(('DOCKER_', 'COMPOSE_'))}
    subprocess.run([*DOCKER, 'compose', '--project-directory', str(root),
                    '-f', str(root / 'compose.json'), *arguments],
                   env=environment, check=True, timeout=60)


def inspect_owned(config, suffix):
    observed = api('GET', '/containers/' + config['name'] + suffix + '/json', missing=True)
    if observed and observed['Config']['Labels'].get('bokkie.deployment') != config['name']:
        raise RuntimeError('refusing to touch a container with a different owner')
    return observed


def stop(config):
    # Stop and remove edge first: it holds a reference to the runtime namespace.
    # Never remove persistent data or touch another deployment's containers.
    for suffix in ('-edge', '-runtime'):
        observed = inspect_owned(config, suffix)
        if observed:
            identity = observed['Id']
            if observed['State']['Running']:
                api('POST', '/containers/' + identity + '/stop?t=15')
            api('DELETE', '/containers/' + identity)


def validate_runtime(expected, observed):
    for key, value in expected['HostConfig'].items():
        if observed['HostConfig'].get(key) != value:
            raise RuntimeError('effective runtime HostConfig differs: ' + key)
    for key in ('User', 'Entrypoint', 'Cmd'):
        if observed['Config'].get(key) != expected[key]:
            raise RuntimeError('effective runtime configuration differs: ' + key)
    # Docker merges the immutable image's labels/environment with these values.
    if not set(expected['Env']).issubset(observed['Config']['Env']):
        raise RuntimeError('effective runtime environment differs')
    if any(observed['Config']['Labels'].get(key) != value
           for key, value in expected['Labels'].items()):
        raise RuntimeError('effective runtime labels differ')
    host = observed['HostConfig']
    if (host['Privileged'] or host['CapAdd'] or host['PortBindings'] or host['PidMode']
            or host['IpcMode'] == 'host' or host['UsernsMode']):
        raise RuntimeError('unexpected outer runtime authority')
    if observed['Image'] != expected['Image'] or not observed['State']['Running']:
        raise RuntimeError('runtime identity/state differs')


def identity(observed):
    return (observed['Id'], observed['State']['StartedAt'])


def readiness(config, identifier):
    subprocess.run([*DOCKER, 'exec', config['name'] + '-edge',
                    '/bin/sh', '-c', 'test -r /run/bokkie-web-auth'],
                   check=True, capture_output=True, timeout=10)
    # The probe returns statuses only, never bootstrap/session/account secrets.
    script = '''import http.client,json,sys
from pathlib import Path
assert Path('/proc/self/attr/current').read_text().strip() == sys.argv[2] + ' (enforce)'
for port,path,status in [(7744,'/health',200),(8080,'/bootstrap',401),(8080,'/ui/',401),(8080,'/',401)]:
    conn=http.client.HTTPConnection('127.0.0.1',port,timeout=2)
    conn.request('GET',path,headers={'Host':sys.argv[1]})
    response=conn.getresponse()
    assert response.status == status, (port,path,response.status)
    response.read();conn.close()
print(json.dumps({'health':200,'unauthenticated_api':401,'unauthenticated_ui':401}))
'''
    for attempt in range(20):
        if STOP.is_set():
            raise RuntimeError('startup interrupted')
        result = subprocess.run([*DOCKER, 'exec', identifier, 'python3', '-c', script,
                                 config['hostname'], profile_name(config)],
                                capture_output=True, text=True, timeout=12)
        if result.returncode == 0:
            return
        if attempt == 19:
            raise RuntimeError('service readiness failed: ' + result.stderr[-2000:])
        STOP.wait(0.5)


def start(config, root):
    info = api('GET', '/info')
    if (info['ServerVersion'] != '29.8.1' or info['KernelVersion'] != '6.12.73+deb13-amd64'
            or set(info['SecurityOptions']) != {'name=apparmor,profile=default',
                                               'name=seccomp,profile=builtin', 'name=cgroupns'}):
        raise RuntimeError('host differs from the qualified Nostromo target')
    image = api('GET', '/images/' + config['image'] + '/json')
    if image['Config']['Labels'].get('org.opencontainers.image.revision') != config['source']:
        raise RuntimeError('runtime image source identity differs')
    api('GET', '/images/' + config['edge_image'] + '/json')
    stop(config)
    render(config, root)
    expected = runtime(config, root)
    identifier = api('POST', '/containers/create?name=' + config['name'] + '-runtime', expected)['Id']
    api('POST', '/containers/' + identifier + '/start')
    observed = inspect_owned(config, '-runtime')
    validate_runtime(expected, observed)
    compose(root, 'up', '-d', '--no-build', '--force-recreate')
    edge_observed = inspect_owned(config, '-edge')
    if (not edge_observed or not edge_observed['State']['Running']
            or edge_observed['HostConfig']['NetworkMode'] != 'container:' + identifier):
        raise RuntimeError('edge is not attached to the current runtime')
    host = edge_observed['HostConfig']
    if (edge_observed['Image'] != config['edge_image']
            or edge_observed['Config']['User'] != '0:0'
            or not host['ReadonlyRootfs'] or host['CapDrop'] != ['ALL']
            or host['CapAdd'] or host['Privileged'] or host['PortBindings']
            or host['SecurityOpt'] != ['no-new-privileges:true']
            or host['RestartPolicy']['Name'] != 'no'):
        raise RuntimeError('effective edge controls differ')
    readiness(config, identifier)
    print(json.dumps({'ready': config['name'], 'source': config['source'],
                      'runtime': identifier, 'edge': edge_observed['Id']}), flush=True)
    return identity(observed), identity(edge_observed)


def run(config, root):
    for signum in (signal.SIGTERM, signal.SIGINT):
        signal.signal(signum, lambda *_: STOP.set())
    try:
        identities = start(config, root)
        while not STOP.wait(3):
            pair = [inspect_owned(config, suffix) for suffix in ('-runtime', '-edge')]
            if any(not item or not item['State']['Running'] for item in pair):
                raise RuntimeError('service container stopped; ordered recovery required')
            if tuple(map(identity, pair)) != identities:
                raise RuntimeError('container identity changed; ordered recovery required')
    finally:
        stop(config)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('render', 'run', 'stop'))
    parser.add_argument('--root', type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    with (root / 'manager.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        config = load(root)
        if args.operation == 'render':
            render(config, root)
        elif args.operation == 'stop':
            stop(config)
        else:
            run(config, root)


if __name__ == '__main__':
    main()
