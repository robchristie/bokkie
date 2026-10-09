"""Host-owned bounded source observations and immutable evidence report objects."""
import base64
import binascii
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import signal
import sys
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, build_opener, HTTPRedirectHandler
from common import atomic, canonical, digest, encoded, locked, overlaps, read, text

SOURCE_FORMAT = 'evidence-source-capture-v1'
REPORT_FORMAT = 'evidence-report-v1'
MANIFEST_FORMAT = 'evidence-source-manifest-v1'
MAX_SOURCE_BYTES = 256 * 1024
MAX_SOURCES = 32
MAX_REPORT_CHARS = 32768
HEX64 = re.compile(r'[0-9a-f]{64}')
REPOSITORY = re.compile(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+')


def selector(value, repositories):
    if not isinstance(value, dict) or value.get('repository') not in repositories:
        raise ValueError('source repository is outside the selected trusted scope')
    repository = value['repository']
    if not REPOSITORY.fullmatch(repository) or any(part in ('.','..') for part in repository.split('/')):
        raise ValueError('invalid source repository')
    if value.get('kind') == 'repository_file':
        if set(value) != {'kind', 'repository', 'commit', 'path'}:
            raise ValueError('repository source requires only exact commit and path')
        path = value['path']
        if (not isinstance(value['commit'], str) or not re.fullmatch(r'[0-9a-f]{40}', value['commit']) or
                not isinstance(path, str) or not 1 <= len(path) <= 1024 or
                not re.fullmatch(r'[A-Za-z0-9_. /-]+', path) or
                any(part in ('', '.', '..') for part in path.split('/')) or
                any(part.startswith('-') for part in path.split('/'))):
            raise ValueError('source requires an exact commit and unambiguous relative path')
    elif value.get('kind') == 'issue_comment':
        if set(value) != {'kind', 'repository', 'comment_id'} or type(value['comment_id']) is not int or not 1 <= value['comment_id'] <= 2**63-1:
            raise ValueError('comment source requires only a positive comment identity')
    else:
        raise ValueError('unsupported source kind')
    return dict(value)


def endpoint(value):
    if value['kind'] == 'repository_file':
        return f"repos/{value['repository']}/contents/{quote(value['path'], safe='/')}?ref={value['commit']}"
    return f"repos/{value['repository']}/issues/comments/{value['comment_id']}"


def source_profile(profile, protected, resources):
    value = profile['source_read']
    expected = {'gh', 'config_dir', 'cwd', 'repositories', 'max_requests', 'max_bytes', 'timeout_seconds'}
    if set(value) != expected:
        raise ValueError('source read profile requires fixed trusted client, scope and finite bounds')
    canonical(value['gh'], directory=False)
    for key in ('config_dir', 'cwd'):
        canonical(value[key])
    for path in (value['gh'], value['config_dir'], value['cwd']):
        if any(overlaps(path, writable) for writable in resources):
            raise ValueError('trusted source client and configuration must be outside task writes')
    if any(overlaps(value['cwd'], root) for root in profile.get('read_roots', [])):
        raise ValueError('trusted source cwd must be outside selected candidate trees')
    repositories = value['repositories']
    if (not isinstance(repositories, list) or not 1 <= len(repositories) <= 32 or
            len(set(repositories)) != len(repositories) or
            any(not isinstance(r, str) or not REPOSITORY.fullmatch(r) or any(part in ('.','..') for part in r.split('/')) for r in repositories)):
        raise ValueError('invalid trusted source repository allowlist')
    for key, maximum in [('max_requests', 64), ('max_bytes', 8 * 1024 * 1024), ('timeout_seconds', 30)]:
        if type(value[key]) is not int or not 1 <= value[key] <= maximum:
            raise ValueError('invalid finite trusted source bound')
    # These paths are hidden from the task boundary even when another read mount
    # happens to contain them. Credential contents never enter task replies.
    protected.extend([value['config_dir'], value['cwd']])
    return value


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError('source redirects are forbidden')


def bounded_process(command, cwd, env, deadline, maximum, *, own_group=True):
    """Drain under a byte/deadline limit rather than bounding after communicate."""
    process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=own_group)
    output = bytearray()
    try:
        with selectors.DefaultSelector() as poll:
            poll.register(process.stdout, selectors.EVENT_READ)
            while poll.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError('trusted source helper timed out')
                for key, _ in poll.select(min(.1, remaining)):
                    chunk = os.read(key.fd, min(4096, maximum + 1 - len(output)))
                    output.extend(chunk)
                    if len(output) > maximum:
                        raise ValueError('trusted source helper response exceeds bound')
                    if not chunk:
                        poll.unregister(key.fileobj)
        try:status=process.wait(timeout=max(.01, deadline-time.monotonic()))
        except subprocess.TimeoutExpired:
            raise ValueError('trusted source helper timed out') from None
        if status:
            raise ValueError('trusted source helper is unavailable')
        return bytes(output)
    finally:
        if own_group:
            try:os.killpg(process.pid,signal.SIGKILL)
            except ProcessLookupError:pass
        elif process.poll() is None:
            process.kill()
        process.wait()
        process.stdout.close()


def _github_get(policy, selected, deadline):
    """Only this host method knows the credential and constructs a fixed GET."""
    end = min(time.monotonic() + policy['timeout_seconds'], deadline)
    environment = {key: value for key, value in os.environ.items()
                   if key in ('PATH', 'HOME', 'LANG', 'LC_ALL', 'SSL_CERT_FILE', 'SSL_CERT_DIR')}
    environment.update(GH_CONFIG_DIR=policy['config_dir'], GH_PROMPT_DISABLED='1')
    token = bounded_process([policy['gh'], 'auth', 'token', '--hostname', 'github.com'],
                            policy['cwd'], environment, end, 4096,own_group=False).decode('ascii').strip()
    if not token or '\n' in token or '\r' in token:
        raise ValueError('trusted source helper is unavailable')
    request = Request('https://api.github.com/' + endpoint(selected), method='GET',
                      headers={'Accept': 'application/vnd.github+json', 'Authorization': 'Bearer ' + token,
                               'X-GitHub-Api-Version': '2022-11-28'})
    # No proxy variables, alternate host or configurable method enter this seam.
    from urllib.request import ProxyHandler
    opener = build_opener(ProxyHandler({}), NoRedirect())
    maximum = min(policy['max_bytes'], MAX_SOURCE_BYTES * 2)
    raw = bytearray()
    try:
        with opener.open(request, timeout=max(.01, end-time.monotonic())) as response:
            if response.status != 200 or response.geturl() != request.full_url:
                raise ValueError('source GET did not return the selected resource')
            length = response.headers.get('Content-Length')
            if length is not None and (not length.isdecimal() or int(length) > maximum):
                raise ValueError('source response exceeds byte bound')
            while True:
                if time.monotonic() >= end:
                    raise ValueError('source GET deadline exhausted')
                # The owned helper enforces the overall deadline. urllib may
                # close its private fp/socket as soon as Content-Length ends.
                chunk = response.read1(min(4096, maximum+1-len(raw)))
                raw.extend(chunk)
                if len(raw) > maximum:
                    raise ValueError('source response exceeds byte bound')
                if not chunk:
                    break
        return json.loads(raw)
    except (HTTPError, URLError, TimeoutError, OSError, UnicodeError, json.JSONDecodeError):
        raise ValueError('selected source GET is unavailable or malformed') from None


def github_get(policy, selected, deadline):
    # Bound the whole operation, including DNS, with a separately owned trusted
    # process. Its fixed arguments contain selectors and paths, never credentials.
    end=min(deadline,time.monotonic()+policy['timeout_seconds'])
    if end<=time.monotonic():raise ValueError('source GET deadline exhausted')
    environment={key:value for key,value in os.environ.items() if key in ('PATH','HOME','LANG','LC_ALL','SSL_CERT_FILE','SSL_CERT_DIR')}
    payload={'policy':policy,'selected':selected,'seconds':max(.01,end-time.monotonic())}
    command=[sys.executable,str(Path(__file__).resolve()),'--source-get',json.dumps(payload)]
    raw=bounded_process(command,policy['cwd'],environment,end,min(policy['max_bytes'],MAX_SOURCE_BYTES*2))
    try:return json.loads(raw)
    except (UnicodeError,json.JSONDecodeError):
        raise ValueError('selected source GET returned malformed host-helper output') from None


def captured_content(selected, response):
    if not isinstance(response, dict):
        raise ValueError('source response must be a regular entry')
    repository = selected['repository']
    if selected['kind'] == 'repository_file':
        if (response.get('type') != 'file' or response.get('encoding') != 'base64' or
                response.get('path') != selected['path'] or
                'target' in response or 'submodule_git_url' in response):
            raise ValueError('unsupported or mismatched repository source entry')
        try:
            raw = base64.b64decode(''.join(response['content'].splitlines()), validate=True)
        except (KeyError, TypeError, ValueError, binascii.Error):
            raise ValueError('repository source has invalid retained bytes') from None
        url = f"https://github.com/{repository}/blob/{selected['commit']}/{quote(selected['path'], safe='/')}"
        if response.get('html_url') != url or type(response.get('size')) is not int or response['size'] != len(raw):
            raise ValueError('repository source identity or length mismatch')
        blob = hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
        if response.get('sha') != blob:
            raise ValueError('repository source Git blob does not match retained bytes')
        metadata = {'blob_sha': blob}
    else:
        comment_id = selected['comment_id']
        url = response.get('html_url')
        if (response.get('id') != comment_id or not isinstance(url, str) or
                not re.fullmatch(r'https://github.com/'+re.escape(repository)+r'/(?:issues|pull)/[1-9][0-9]*#issuecomment-'+str(comment_id), url) or
                not isinstance(response.get('issue_url'), str) or
                not re.fullmatch(r'https://api.github.com/repos/'+re.escape(repository)+r'/issues/[1-9][0-9]*', response['issue_url'])):
            raise ValueError('comment identity or repository mismatch')
        body = response.get('body')
        user=response.get('user')
        author=user.get('login') if isinstance(user,dict) else None
        if not isinstance(body, str) or not isinstance(author, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,100}(?:\[bot\])?', author):
            raise ValueError('comment body and author must be retained')
        metadata = {'author': author}
        for key in ('created_at', 'updated_at'):
            value = response.get(key)
            if not isinstance(value, str) or not re.fullmatch(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ', value):
                raise ValueError('comment observation requires creation and update times')
            datetime.fromisoformat(value.replace('Z', '+00:00'))
            metadata[key] = value
        if metadata['updated_at'] < metadata['created_at']:
            raise ValueError('comment update precedes creation')
        raw = body.encode('utf-8')
    if len(raw) > MAX_SOURCE_BYTES:
        raise ValueError('source retained bytes exceed bound')
    return raw, url, metadata


class EvidenceStore:
    def __init__(self, root, admission, *, create=True):
        self.root = Path(root)
        self.admission = admission
        self.policy = admission['project_profile']['source_read']
        scope = admission['dispatch']['assignment']['repository_scope']
        self.repositories = [r for r in self.policy['repositories'] if r in scope]
        self.store = self.root / 'evidence'
        self.mirror = self.root / 'evidence-mirror'
        if not create:
            return
        for path in (self.store, self.mirror):
            path.mkdir(mode=0o700, exist_ok=True)
        for name in ('captures', 'selectors', 'reports', 'seals'):
            (self.store/name).mkdir(mode=0o700, exist_ok=True)
        (self.mirror/'sources').mkdir(mode=0o700, exist_ok=True)
        (self.mirror/'reports').mkdir(mode=0o700, exist_ok=True)

    def capture(self, request, *, query=github_get):
        selected = selector(request, self.repositories)
        key = digest(selected)
        with locked(self.store/'capture.lock'):
            reference = self.store/'selectors'/(key+'.json')
            if reference.exists():
                return self.source(read(reference)['id'])
            catalogue = list((self.store/'selectors').glob('*.json'))
            requests = self.store/'reads.json'
            used = read(requests)['requests'] if requests.exists() else 0
            if used >= self.policy['max_requests'] or len(catalogue) >= MAX_SOURCES:
                raise ValueError('finite source read budget exhausted')
            # Account for failed reads before the external effect as well.
            atomic(requests, {'requests': used+1})
            remaining = self.admission['deadline'] - time.time()
            if remaining <= 0:
                raise ValueError('source capture admission deadline exhausted')
            response = query(self.policy, selected, time.monotonic()+remaining)
            raw, url, metadata = captured_content(selected, response)
            if len(raw) + sum(self.source(read(p)['id'])['source']['bytes'] for p in catalogue) > self.policy['max_bytes']:
                raise ValueError('finite retained source byte budget exhausted')
            observed_at = int(time.time())
            capsule = {'format': SOURCE_FORMAT, 'method': 'github-rest-get-v1',
                       'execution_id': self.admission['dispatch']['execution_id'],
                       'dispatch_digest': self.admission['dispatch_digest'],
                       'admission_digest': digest(self.admission), 'selector': selected,
                       'url': url, 'observed_at': observed_at, 'metadata': metadata,
                       'bytes': len(raw), 'content_digest': hashlib.sha256(raw).hexdigest(),
                       'content_base64': base64.b64encode(raw).decode('ascii')}
            identity = digest(capsule)
            atomic(self.store/'captures'/(identity+'.json'), capsule, immutable=True)
            atomic(self.mirror/'sources'/(identity+'.json'), capsule, immutable=True)
            atomic(reference, {'id': identity}, immutable=True)
            return self.source(identity)

    def source(self, identity):
        if not isinstance(identity, str) or not HEX64.fullmatch(identity):
            raise ValueError('invalid retained source identity')
        capsule = read(self.store/'captures'/(identity+'.json'))
        raw = base64.b64decode(capsule['content_base64'], validate=True)
        if (digest(capsule) != identity or capsule['format'] != SOURCE_FORMAT or
                capsule['execution_id'] != self.admission['dispatch']['execution_id'] or
                capsule['admission_digest'] != digest(self.admission) or
                capsule['dispatch_digest'] != self.admission['dispatch_digest'] or
                capsule['method'] != 'github-rest-get-v1' or
                capsule['bytes'] != len(raw) or capsule['content_digest'] != hashlib.sha256(raw).hexdigest()):
            raise ValueError('retained source capsule does not match its identity')
        selector(capsule['selector'], self.repositories)
        if read(self.mirror/'sources'/(identity+'.json')) != capsule:
            raise ValueError('read-only source mirror differs from sealed capture')
        source = {key: capsule[key] for key in ('url', 'content_digest', 'bytes', 'observed_at')}
        source['id'] = identity
        return {'source': source, 'selector': capsule['selector'], 'metadata': capsule['metadata'],
                'content': raw.decode('utf-8', errors='replace'),
                'mirror': '/bokkie-evidence/sources/'+identity+'.json'}

    def seal(self, markdown, source_ids):
        text(markdown, MAX_REPORT_CHARS)
        if (not isinstance(source_ids, list) or not 1 <= len(source_ids) <= MAX_SOURCES or
                any(not isinstance(value,str) for value in source_ids) or len(set(source_ids)) != len(source_ids)):
            raise ValueError('report requires a distinct bounded captured source catalogue')
        sources = [self.source(identity)['source'] for identity in source_ids]
        manifest = {'format': MANIFEST_FORMAT, 'sources': sources}
        body = {'format': REPORT_FORMAT, 'markdown': markdown, 'source_manifest_digest': digest(manifest)}
        report = {**body, 'digest': digest(body), 'sources': sources}
        atomic(self.store/'reports'/(report['digest']+'.json'), report, immutable=True)
        atomic(self.mirror/'reports'/(report['digest']+'.json'), report, immutable=True)
        provenance=self.store/'seals'/(report['digest']+'.json')
        if not provenance.exists():
            atomic(provenance,{'format':'evidence-report-seal-v1','report_digest':report['digest'],
                'source_manifest_digest':report['source_manifest_digest'],'completed_at':int(time.time())},immutable=True)
        self.seal_provenance(report['digest'])
        return report

    def seal_provenance(self, identity):
        if not isinstance(identity,str) or not HEX64.fullmatch(identity):
            raise ValueError('invalid sealed report identity')
        value=read(self.store/'seals'/(identity+'.json'))
        report=read(self.store/'reports'/(identity+'.json'))
        if (not isinstance(value,dict) or set(value)!={'format','report_digest','source_manifest_digest','completed_at'} or
                value['format']!='evidence-report-seal-v1' or value['report_digest']!=identity or
                value['source_manifest_digest']!=report['source_manifest_digest'] or
                type(value['completed_at']) is not int or value['completed_at']<=0):
            raise ValueError('report seal has no valid immutable completion provenance')
        return value

    def report(self, identity):
        if not isinstance(identity, str) or not HEX64.fullmatch(identity):
            raise ValueError('invalid sealed report identity')
        report = read(self.store/'reports'/(identity+'.json'))
        sources=[self.source(source['id'])['source'] for source in report['sources']]
        manifest={'format':MANIFEST_FORMAT,'sources':sources}
        body={'format':REPORT_FORMAT,'markdown':report['markdown'],'source_manifest_digest':digest(manifest)}
        wanted={**body,'digest':digest(body),'sources':sources}
        if wanted != report or report['digest'] != identity:
            raise ValueError('sealed report bytes or source manifest changed')
        if read(self.mirror/'reports'/(identity+'.json')) != report:
            raise ValueError('read-only report mirror differs from sealed report')
        return report


if __name__=='__main__':
    if len(sys.argv)!=3 or sys.argv[1]!='--source-get':
        raise SystemExit(2)
    try:
        request=json.loads(sys.argv[2])
        selected=selector(request['selected'],request['policy']['repositories'])
        response=_github_get(request['policy'],selected,time.monotonic()+request['seconds'])
        sys.stdout.buffer.write(encoded(response))
    except Exception:
        # Raw transport or credential diagnostics never leave this host helper.
        raise SystemExit(1) from None
