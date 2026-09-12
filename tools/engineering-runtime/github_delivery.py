"""Narrow host-side Pagefold delivery. Callers own durable intent and independent review.

The subprocess seam is for deterministic tests. Errors deliberately omit command output:
Git and GitHub diagnostics can contain credentials or private account details.
"""
from contextvars import ContextVar
import json
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = 'robchristie/pagefold'
URL = 'https://github.com/' + REPO + '.git'
_UNCERTAIN = ContextVar('delivery_mutation_started', default=False)
SHA = re.compile(r'[0-9a-f]{40}\Z')
BRANCH = re.compile(r'codex/[A-Za-z0-9][A-Za-z0-9_-]*(?:/[A-Za-z0-9][A-Za-z0-9_-]*)*\Z')


class DeliveryError(ValueError):
    """Safe, bounded error for the broker."""

    def __init__(self, message):
        super().__init__(message)
        self.uncertain = _UNCERTAIN.get()


def _config(config):
    if (not isinstance(config, dict) or set(config) != {'repo', 'base', 'branch'}
            or config['repo'] != REPO or config['base'] != 'main'
            or not isinstance(config['branch'], str)
            or len(config['branch']) > 160 or not BRANCH.fullmatch(config['branch'])):
        raise DeliveryError('invalid fixed Pagefold delivery scope')
    return config['branch']


def _head(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise DeliveryError('expected exact lowercase 40-character commit')
    return value


def _pr(value):
    if type(value) is not int or not 0 < value < 2**31:
        raise DeliveryError('invalid pull request number')
    return value


class Host:
    def __init__(self, config, workspace, run):
        self.branch = _config(config)
        self.root = Path(workspace).absolute()
        if self.root.is_symlink() or self.root.resolve() != self.root or not (self.root / '.git').is_dir() or (self.root / '.git').is_symlink():
            raise DeliveryError('workspace must be a standalone repository without symlinks')
        if (self.root / '.git' / 'commondir').exists() or any(p.is_symlink() for p in (self.root / '.git').rglob('*')):
            raise DeliveryError('indirect Git metadata is unsupported')
        self.run = run
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('GIT_', 'GH_', 'GITHUB_'))}
        self.env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null', GIT_TERMINAL_PROMPT='0', GH_PROMPT_DISABLED='1', GH_HOST='github.com', GIT_PAGER='cat')
        # Host authentication comes from the existing gh configuration, never the candidate.
        self.env['PATH'] = '/usr/bin:/bin'
        raw = self.git('config', '--local', '--no-includes', '--null', '--list')
        allowed = {'core.repositoryformatversion', 'core.filemode', 'core.bare', 'core.logallrefupdates', 'user.name', 'user.email'}
        for entry in raw.split('\0'):
            if not entry:
                continue
            key, _, value = entry.partition('\n')
            if key in allowed:
                if key == 'core.bare' and value != 'false':
                    raise DeliveryError('bare repository is unsupported')
            elif key == 'remote.origin.url' and value == URL:
                pass
            elif key == 'remote.origin.fetch' and value == '+refs/heads/*:refs/remotes/origin/*':
                pass
            elif re.fullmatch(r'branch\.[A-Za-z0-9/_-]+\.remote', key) and value == 'origin':
                pass
            elif re.fullmatch(r'branch\.[A-Za-z0-9/_-]+\.merge', key) and value in ('refs/heads/main', 'refs/heads/' + self.branch):
                pass
            else:
                raise DeliveryError('unsupported local Git configuration')
        if self.git('config', '--local', '--no-includes', '--get', 'remote.origin.url').strip() != URL:
            raise DeliveryError('fixed Pagefold origin is required')
        if self.git('rev-parse', '--show-toplevel').strip() != str(self.root):
            raise DeliveryError('workspace root mismatch')

    def command(self, argv, *, host=False):
        try:
            result = self.run(argv, cwd='/' if host else str(self.root), env=self.env,
                              capture_output=True, text=True, timeout=90, check=False)
        except (OSError, subprocess.SubprocessError):
            raise DeliveryError('host command failed or timed out; reconcile before retry') from None
        if result.returncode or len(result.stdout) > 2_000_000:
            raise DeliveryError('host command failed or exceeded output bound; reconcile before retry')
        return result.stdout

    def git(self, *args):
        if any(arg in ('switch', 'add', 'commit', 'push') for arg in args):
            _UNCERTAIN.set(True)
        return self.command(['/usr/bin/git', '-c', 'core.hooksPath=/dev/null', '-c', 'core.fsmonitor=false',
                             '-c', 'commit.gpgsign=false', '-c', 'tag.gpgsign=false', *args])

    def gh(self, *args):
        if args[:2] in (('pr', 'create'), ('pr', 'merge')):
            _UNCERTAIN.set(True)
        return self.command(['/usr/bin/gh', *args], host=True)

    def api(self, path):
        try:
            return json.loads(self.gh('api', '--hostname', 'github.com', path))
        except (json.JSONDecodeError, TypeError):
            raise DeliveryError('invalid GitHub response') from None

    def current(self):
        if self.git('symbolic-ref', '--short', 'HEAD').strip() != self.branch:
            raise DeliveryError('delivery branch mismatch')
        return _head(self.git('rev-parse', 'HEAD').strip())

    def pull(self, number):
        p = self.api(f'repos/{REPO}/pulls/{_pr(number)}')
        if (p.get('number') != number or p.get('base', {}).get('ref') != 'main'
                or p.get('head', {}).get('ref') != self.branch
                or any(p.get(side, {}).get('repo', {}).get('full_name') != REPO for side in ('base', 'head'))
                or p.get('html_url') != f'https://github.com/{REPO}/pull/{number}'):
            raise DeliveryError('GitHub pull request identity mismatch')
        _head(p['head']['sha'])
        return p

    def status(self, number):
        p = self.pull(number)
        head = p['head']['sha']
        checks = self.api(f'repos/{REPO}/commits/{head}/check-runs?filter=latest&per_page=100')
        reviews = self.api(f'repos/{REPO}/pulls/{number}/reviews?per_page=100')
        query = ('query { repository(owner:"robchristie",name:"pagefold") { pullRequest(number:' + str(number) + ') { reviewThreads(first:100) { pageInfo { hasNextPage } nodes { isResolved } } } } }')
        threads = json.loads(self.gh('api', '--hostname', 'github.com', 'graphql', '-f', 'query=' + query))['data']['repository']['pullRequest']['reviewThreads']
        if checks.get('total_count', 101) != len(checks.get('check_runs', [])) or len(reviews) >= 100 or threads['pageInfo']['hasNextPage']:
            raise DeliveryError('GitHub evidence pagination exceeds supported bound')
        latest = {}
        for review in reviews:
            if review['state'] in ('APPROVED', 'CHANGES_REQUESTED', 'DISMISSED'):
                latest[review['user']['login']] = review['state']
        post_merge = bool(p['merged'] and p.get('merge_commit_sha') and self.successful_ci(_head(p['merge_commit_sha'])))
        return {'post_merge_verified': post_merge, 'repo': REPO, 'base': 'main', 'branch': self.branch, 'pr': number, 'head': head,
                'state': p['state'], 'merged': p['merged'], 'merge_commit': p.get('merge_commit_sha'),
                'draft': p['draft'], 'mergeable': p.get('mergeable'), 'mergeable_state': p.get('mergeable_state'),
                'changes_requested': 'CHANGES_REQUESTED' in latest.values(),
                'human_review_required': any(label.get('name') == 'human-review-required' for label in p.get('labels', [])),
                'unresolved_threads': any(not t['isResolved'] for t in threads['nodes']),
                'checks': [{'name': c['name'], 'head': c['head_sha'], 'status': c['status'], 'conclusion': c['conclusion'],
                            'app_id': c.get('app', {}).get('id')} for c in checks['check_runs']]}

    def find_pr(self, head):
        pulls = self.api(f'repos/{REPO}/pulls?state=all&head=robchristie:{self.branch}&base=main&per_page=100')
        if len(pulls) >= 100:
            raise DeliveryError('pull request lookup exceeds supported bound')
        matching = [p for p in pulls if p.get('head', {}).get('sha') == head]
        if len(matching) > 1:
            raise DeliveryError('ambiguous duplicate pull requests')
        return self.pull(matching[0]['number']) if matching else None

    def successful_ci(self, head):
        runs = self.api(f'repos/{REPO}/actions/runs?head_sha={head}&per_page=100')
        if runs.get('total_count', 101) != len(runs.get('workflow_runs', [])):
            raise DeliveryError('workflow evidence exceeds supported bound')
        matching = [r for r in runs['workflow_runs'] if r.get('name') == 'CI'
                    and r.get('head_sha') == head and r.get('repository', {}).get('full_name') == REPO]
        if not matching:
            return False
        latest = max(matching, key=lambda r: r['id'])
        if latest.get('status') != 'completed' or latest.get('conclusion') != 'success':
            return False
        run_id = latest['id']
        if type(run_id) is not int or run_id <= 0:
            raise DeliveryError('invalid workflow identity')
        jobs = self.api(f'repos/{REPO}/actions/runs/{run_id}/jobs?filter=latest&per_page=100')
        if jobs.get('total_count', 101) != len(jobs.get('jobs', [])):
            raise DeliveryError('workflow job evidence exceeds supported bound')
        return any(j.get('name') == 'Fresh-checkout verification' and j.get('status') == 'completed'
                   and j.get('conclusion') == 'success' and j.get('head_sha') == head
                   and any(step.get('status') == 'completed' and step.get('conclusion') == 'success'
                           for step in j.get('steps', [])) for j in jobs['jobs'])

    def policy(self):
        repo = self.api(f'repos/{REPO}')
        if (repo.get('full_name') != REPO or repo.get('html_url') != 'https://github.com/' + REPO
                or repo.get('permissions', {}).get('push') is not True or repo.get('allow_squash_merge') is not True):
            raise DeliveryError('repository identity, push permission or squash policy is unavailable')
        branch = self.api(f'repos/{REPO}/branches/main')
        if branch.get('name') != 'main' or type(branch.get('protected')) is not bool:
            raise DeliveryError('missing authoritative base branch policy')
        if branch['protected']:
            protection = self.api(f'repos/{REPO}/branches/main/protection')
            if not isinstance(protection, dict) or 'required_status_checks' not in protection:
                raise DeliveryError('missing branch protection policy evidence')
        else:
            # A successful authoritative branch response establishes absence of
            # branch protection; an ambiguous protection-endpoint 404 never does.
            protection = {'required_status_checks': None}
        rules = self.api(f'repos/{REPO}/rulesets?includes_parents=true&per_page=100')
        if not isinstance(rules, list) or len(rules) >= 100 or any(r.get('enforcement') != 'disabled' for r in rules):
            raise DeliveryError('active repository rulesets are unsupported')
        return protection

    def merge_policy(self, status):
        protection = self.policy()
        required = protection.get('required_status_checks') or {}
        contexts = set(required.get('contexts', [])) | {c['context'] for c in required.get('checks', [])}
        checks = status['checks']
        if not self.successful_ci(status['head']):
            raise DeliveryError('successful exact-head CI workflow is missing')
        if (not checks or any(c['head'] != status['head'] or c['status'] != 'completed' or c['conclusion'] != 'success' for c in checks)
                or not contexts.issubset({c['name'] for c in checks})
                or not any(c['name'] == 'Fresh-checkout verification' and c['app_id'] == 15368 for c in checks)):
            raise DeliveryError('exact-head successful Pagefold CI evidence is missing')
        for required_check in required.get('checks', []):
            if required_check.get('app_id') not in (None, -1) and not any(c['name'] == required_check['context'] and c['app_id'] == required_check['app_id'] for c in checks):
                raise DeliveryError('required check application mismatch')


def preflight(config, workspace, *, run=subprocess.run):
    _UNCERTAIN.set(False)
    host = Host(config, workspace, run)
    host.gh('auth', 'status', '--hostname', 'github.com')
    policy = host.policy()
    base = host.api(f'repos/{REPO}/branches/main')
    head = _head(base.get('commit', {}).get('sha'))
    if not host.successful_ci(head):
        raise DeliveryError('base CI integration has no successful representative run')
    return {'repo': REPO, 'base': 'main', 'branch': host.branch, 'authenticated': True,
            'push_authorised': True, 'squash_merge': True, 'base_head': head,
            'base_ci_verified': True, 'protected': base['protected'],
            'policy_sha256': hashlib.sha256(json.dumps(policy, sort_keys=True).encode()).hexdigest()}


def _arguments(operation, args):
    schema = {'prepare_branch': set(), 'commit': {'paths', 'message'}, 'push': {'head'},
              'open_pr': {'head', 'title', 'body'}, 'status': {'pr'}, 'merge': {'pr', 'head'}}
    if operation not in schema or not isinstance(args, dict) or set(args) != schema[operation]:
        raise DeliveryError('invalid delivery operation arguments')
    if 'head' in args:
        _head(args['head'])
    if 'pr' in args:
        _pr(args['pr'])


def execute(config, workspace, operation, arguments, *, run=subprocess.run):
    _UNCERTAIN.set(False)
    _arguments(operation, arguments)
    h = Host(config, workspace, run)
    a = arguments
    if operation == 'status':
        return h.status(a['pr'])
    if operation == 'prepare_branch':
        branch = h.git('symbolic-ref', '--short', 'HEAD').strip()
        if branch != 'main' or h.git('status', '--porcelain').strip():
            raise DeliveryError('prepare requires a clean main checkout')
        h.git('switch', '-c', h.branch)
        return {'branch': h.branch, 'head': h.current()}
    head = h.current()
    if operation == 'commit':
        paths = a['paths']
        if (not isinstance(paths, list) or not 0 < len(paths) <= 200 or len(set(map(str, paths))) != len(paths)
                or any(not isinstance(p, str) or len(p) > 1024 or not re.fullmatch(r'[A-Za-z0-9_.][A-Za-z0-9_./ -]*', p)
                       or any(part in ('', '.', '..', '.git') for part in p.split('/')) for p in paths)):
            raise DeliveryError('unsafe commit paths')
        for path in paths:
            target = h.root / path
            if target.resolve() != target or target.is_symlink():
                raise DeliveryError('symlink commit path is unsupported')
        if not isinstance(a['message'], str) or not a['message'].strip() or len(a['message']) > 4000 or '\0' in a['message']:
            raise DeliveryError('invalid commit message')
        if h.git('diff', '--cached', '--name-only', '-z'):
            raise DeliveryError('existing staged changes are unsupported')
        h.git('--literal-pathspecs', 'add', '--', *paths)
        h.git('commit', '--no-verify', '-m', a['message'])
        return {'head': h.current(), 'parent': head}
    if head != a['head']:
        raise DeliveryError('stale local head')
    if h.git('status', '--porcelain').strip():
        raise DeliveryError('remote delivery requires a clean workspace')
    base = h.api(f'repos/{REPO}/branches/main')
    if base.get('name') != 'main':
        raise DeliveryError('base branch identity mismatch')
    base_head = _head(base.get('commit', {}).get('sha'))
    # Do not fetch or rewrite the candidate here. Missing or newer base objects
    # require explicit candidate preparation and renewed review by the caller.
    h.git('merge-base', '--is-ancestor', base_head, head)
    if operation == 'push':
        h.git('-c', 'credential.helper=', '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
              'push', '--porcelain', URL, head + ':refs/heads/' + h.branch)
        return {'head': head, 'branch': h.branch}
    if operation == 'open_pr':
        for key, bound in (('title', 256), ('body', 30000)):
            if not isinstance(a[key], str) or not a[key].strip() or len(a[key]) > bound or '\0' in a[key]:
                raise DeliveryError('invalid pull request text')
        existing = h.find_pr(head)
        if existing:
            return {'pr': existing['number'], 'head': head, 'existing': True}
        remote = h.api(f'repos/{REPO}/git/ref/heads/{h.branch}')
        if remote['object']['sha'] != head:
            raise DeliveryError('remote head mismatch')
        h.gh('pr', 'create', '--repo', 'github.com/' + REPO, '--base', 'main', '--head', h.branch,
             '--title', a['title'], '--body', a['body'])
        created = h.find_pr(head)
        if not created:
            raise DeliveryError('pull request creation needs reconciliation')
        return {'pr': created['number'], 'head': head, 'existing': False}
    status = h.status(a['pr'])
    if (status['head'] != head or status['state'] != 'open' or status['draft'] or status['mergeable'] is not True
            or status['mergeable_state'] != 'clean' or status['changes_requested'] or status['unresolved_threads']
            or status['human_review_required']):
        raise DeliveryError('pull request is not eligible for merge')
    h.merge_policy(status)
    h.gh('pr', 'merge', str(a['pr']), '--repo', 'github.com/' + REPO, '--squash', '--match-head-commit', head)
    result = h.status(a['pr'])
    if not result['merged'] or result['head'] != head:
        raise DeliveryError('merge needs reconciliation')
    return result


def reconcile(config, workspace, operation, arguments, *, run=subprocess.run):
    """Read back success only. None means unknown, never permission to replay."""
    _UNCERTAIN.set(False)
    _arguments(operation, arguments)
    h = Host(config, workspace, run)
    if operation == 'prepare_branch':
        return {'branch': h.branch, 'head': h.current()}
    if operation == 'commit':
        # Commit requests have no pre-recorded parent/tree identity. A matching
        # message or path list cannot prove this specific mutation completed.
        return None
    if operation == 'push':
        ref = h.api(f'repos/{REPO}/git/ref/heads/{h.branch}')
        return {'head': arguments['head'], 'branch': h.branch} if ref['object']['sha'] == arguments['head'] else None
    if operation == 'open_pr':
        p = h.find_pr(arguments['head'])
        return {'pr': p['number'], 'head': arguments['head']} if p else None
    status = h.status(arguments['pr'])
    if operation == 'status' or (status['merged'] and status['head'] == arguments['head']):
        return status
    return None


def main():
    try:
        raw = sys.stdin.buffer.read(65537)
        if len(raw) > 65536:
            raise DeliveryError('request exceeds bound')
        request = json.loads(raw)
        if set(request) - {'config', 'workspace', 'operation', 'arguments', 'reconcile'}:
            raise DeliveryError('unknown request field')
        if request['operation'] == 'preflight':
            result = preflight(request['config'], request['workspace'])
        else:
            action = reconcile if request.get('reconcile', False) else execute
            result = action(request['config'], request['workspace'], request['operation'], request['arguments'])
        encoded = json.dumps(result)
        if len(encoded.encode()) > 2_000_000:
            raise DeliveryError('response exceeds bound')
        print(encoded)
    except DeliveryError as error:
        print(json.dumps({'error': str(error), 'uncertain': error.uncertain}))
        return 1
    except Exception:
        print(json.dumps({'error': 'invalid request or unavailable delivery evidence', 'uncertain': _UNCERTAIN.get()}))
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
