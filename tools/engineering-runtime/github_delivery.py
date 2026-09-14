"""Narrow host-side Pagefold delivery. Callers own durable intent and independent review.

The subprocess seam is for deterministic tests. Errors deliberately omit command output:
Git and GitHub diagnostics can contain credentials or private account details.
"""
from contextvars import ContextVar
from contextlib import contextmanager
import fcntl
import pwd
import stat
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
DIGEST = re.compile(r'[0-9a-f]{64}\Z')
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
        self.lock_fd = None
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

    def command(self, argv, *, host=False, input=None):
        try:
            result = self.run(argv, cwd='/' if host else str(self.root), env=self.env,
                              capture_output=True, text=True, timeout=90, check=False, input=input,
                              **({'pass_fds': (self.lock_fd,)} if self.lock_fd is not None else {}))
        except (OSError, subprocess.SubprocessError):
            raise DeliveryError('host command failed or timed out; reconcile before retry') from None
        if result.returncode or len(result.stdout) > 2_000_000:
            raise DeliveryError('host command failed or exceeded output bound; reconcile before retry')
        return result.stdout

    def git(self, *args):
        if any(arg in ('switch', 'add', 'commit', 'push', 'fetch', 'merge', 'update-ref') for arg in args):
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

    def api_write(self, path, method, payload):
        _UNCERTAIN.set(True)
        self.command(['/usr/bin/gh', 'api', '--hostname', 'github.com', path,
                      '--method', method, '--input', '-'], host=True,
                     input=json.dumps(payload, ensure_ascii=False))

    @staticmethod
    def text_digest(pull):
        return hashlib.sha256(json.dumps({'title': pull.get('title', ''),
            'body': pull.get('body') or ''}, sort_keys=True, ensure_ascii=False).encode()).hexdigest()

    def pr_text_receipt(self, pull, arguments, disposition):
        applied = all((pull.get(k) or '') == arguments[k] for k in ('title', 'body'))
        return {'pr': pull['number'], 'head': pull['head']['sha'],
                'disposition': disposition, 'existing': disposition != 'created',
                'text_applied': applied, 'text_digest': self.text_digest(pull),
                'next_action': None if applied else 'update_pr with observed text_digest'}

    def update_pr(self, a, *, mutate):
        pull = self.pull(a['pr'])
        if pull['head']['sha'] != a['head'] or pull['state'] != 'open' or pull['merged']:
            raise DeliveryError('PR update requires the exact open head')
        receipt = self.pr_text_receipt(pull, a, 'unchanged')
        if receipt['text_applied']:
            return receipt
        if not mutate:
            return None
        if receipt['text_digest'] != a['expected_text_digest']:
            raise DeliveryError('PR text changed; inspect before updating')
        self.api_write(f'repos/{REPO}/pulls/{a["pr"]}', 'PATCH',
                       {k: a[k] for k in ('title', 'body')})
        result = self.update_pr(a, mutate=False)
        if result is None:
            raise DeliveryError('PR text update needs reconciliation')
        result['disposition'] = 'updated'
        return result

    def closeout(self, a, *, mutate):
        status = self.status(a['pr'])
        if (not status['post_merge_verified'] or status['head'] != a['head']
                or status['head_tree'] != a['tree'] or status['merge_commit'] != a['merge_commit']
                or status['human_review_required']):
            raise DeliveryError('closeout requires exact verified merge and successful CI')
        marker = f'<!-- bokkie-closeout-v1:{a["pr"]}:{a["head"]}:{a["merge_commit"]} -->'
        # Stable CI identities are supplied from the retained merge receipt by
        # the runtime and checked against current evidence before publication.
        for phase in ('pre_merge_ci', 'post_merge_ci'):
            if a[phase] != status[phase]['run']:
                raise DeliveryError('closeout CI changed; inspect retained merge evidence')
        body = (f'{marker}\nBokkie delivery closeout\n\n'
                f'- Independent exact-head review: PASS for `{a["head"]}`.\n'
                f'- Registered review evidence SHA-256: `{a["review_digest"]}`.\n'
                f'- Reviewed and merged tree: `{a["tree"]}`.\n'
                f'- Squash merge: `{a["merge_commit"]}`.\n')
        for label, phase in (('PR CI', 'pre_merge_ci'), ('Post-merge CI', 'post_merge_ci')):
            run = a[phase]
            body += f'- {label}: [run {run["id"]}]({run["url"]}), attempt {run["attempt"]}, head `{run["head"]}` — success.\n'
        body += '\nThis receipt records verified delivery. Product acceptance and cleanup are tracked separately in Bokkie.\n'
        user = self.api('user')
        if type(user.get('id')) is not int or user['id'] <= 0:
            raise DeliveryError('closeout publisher identity unavailable')
        comments = self.api(f'repos/{REPO}/issues/{a["pr"]}/comments?per_page=100')
        if not isinstance(comments, list) or len(comments) >= 100:
            raise DeliveryError('closeout comments exceed supported bound')
        matches = [c for c in comments if marker in (c.get('body') or '')]
        if matches:
            if len(matches) != 1:
                raise DeliveryError('ambiguous closeout comments')
            comment = matches[0]
            identity = comment.get('id')
            if (type(identity) is not int or identity <= 0 or comment.get('body') != body
                    or comment.get('user', {}).get('id') != user['id']
                    or comment.get('html_url') != f'https://github.com/{REPO}/pull/{a["pr"]}#issuecomment-{identity}'):
                raise DeliveryError('closeout comment identity or content mismatch')
            return {'pr': a['pr'], 'head': a['head'], 'merge_commit': a['merge_commit'],
                    'closeout': {'state': 'published', 'comment_id': identity,
                                 'url': comment['html_url'], 'publisher_id': user['id'],
                                 'body_sha256': hashlib.sha256(body.encode()).hexdigest()}}
        if not mutate:
            return None
        self.api_write(f'repos/{REPO}/issues/{a["pr"]}/comments', 'POST', {'body': body})
        result = self.closeout(a, mutate=False)
        if result is None:
            raise DeliveryError('closeout publication needs reconciliation')
        return result

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
        head_tree = self.commit_tree(head)
        merge_tree = self.commit_tree(_head(p['merge_commit_sha'])) if p['merged'] and p.get('merge_commit_sha') else None
        pre_ci = self.ci_receipt(head)
        post_ci = self.ci_receipt(_head(p['merge_commit_sha'])) if merge_tree else {'state': 'unavailable', 'head': None, 'run': None, 'runs': [], 'jobs': []}
        tree_equal = merge_tree is not None and head_tree == merge_tree
        post_merge = bool(tree_equal and post_ci['state'] == 'success')
        return {'pr_text': {'title': p.get('title', ''), 'body': p.get('body') or ''},
                'text_digest': self.text_digest(p), 'head_tree': head_tree, 'merge_tree': merge_tree, 'tree_equal': tree_equal,
                'pre_merge_ci': pre_ci, 'post_merge_ci': post_ci, 'post_merge_verified': post_merge, 'repo': REPO, 'base': 'main', 'branch': self.branch, 'pr': number, 'head': head,
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

    def commit_tree(self, head):
        commit = self.api(f'repos/{REPO}/git/commits/{_head(head)}')
        if commit.get('sha') != head:
            raise DeliveryError('commit evidence identity mismatch')
        return _head(commit.get('tree', {}).get('sha'))

    def successful_ci(self, head):
        return self.ci_receipt(head)['state'] == 'success'

    def ci_receipt(self, head):
        receipt = {'state': 'unavailable', 'head': _head(head), 'run': None, 'runs': [], 'jobs': []}
        try:
            runs = self.api(f'repos/{REPO}/actions/runs?head_sha={head}&per_page=100')
        except DeliveryError:
            return receipt
        if runs.get('total_count', 101) != len(runs.get('workflow_runs', [])):
            raise DeliveryError('workflow evidence exceeds supported bound')
        matching = [r for r in runs['workflow_runs'] if r.get('name') == 'CI'
                    and r.get('head_sha') == head and r.get('repository', {}).get('full_name') == REPO]
        if not matching:
            receipt['state'] = 'pending'
            return receipt
        if any(type(r.get('id')) is not int or r['id'] <= 0 for r in matching):
            raise DeliveryError('invalid workflow identity')
        for candidate in matching:
            candidate_id = candidate['id']
            if (type(candidate.get('run_attempt')) is not int or candidate['run_attempt'] <= 0
                    or candidate.get('html_url') != f'https://github.com/{REPO}/actions/runs/{candidate_id}'):
                raise DeliveryError('invalid workflow attempt or URL')
            receipt['runs'].append({'id': candidate_id, 'url': candidate['html_url'],
                                    'attempt': candidate['run_attempt'], 'head': head,
                                    'status': candidate.get('status'), 'conclusion': candidate.get('conclusion')})
        latest = max(matching, key=lambda r: r['id'])
        run_id, attempt = latest['id'], latest.get('run_attempt')
        url = f'https://github.com/{REPO}/actions/runs/{run_id}'
        if type(attempt) is not int or attempt <= 0 or latest.get('html_url') != url:
            raise DeliveryError('invalid workflow attempt or URL')
        receipt['run'] = {'id': run_id, 'url': url, 'attempt': attempt, 'head': head,
                          'status': latest.get('status'), 'conclusion': latest.get('conclusion')}
        # Attempt-specific jobs cannot accidentally mix a restarted run with old jobs.
        try:
            jobs = self.api(f'repos/{REPO}/actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100')
        except DeliveryError:
            return receipt
        if jobs.get('total_count', 101) != len(jobs.get('jobs', [])):
            raise DeliveryError('workflow job evidence exceeds supported bound')
        for job in jobs['jobs']:
            job_id = job.get('id')
            if (type(job_id) is not int or job_id <= 0 or job.get('run_id') != run_id
                    or job.get('run_attempt', attempt) != attempt or job.get('head_sha') != head
                    or job.get('html_url') not in (url + '/job/' + str(job_id),
                        f'https://github.com/{REPO}/runs/{run_id}/jobs/{job_id}')):
                raise DeliveryError('workflow job identity mismatch')
            receipt['jobs'].append({'id': job_id, 'url': job['html_url'], 'run_id': run_id,
                                    'attempt': attempt, 'head': head, 'name': job.get('name'),
                                    'status': job.get('status'), 'conclusion': job.get('conclusion')})
        required = [j for j in jobs['jobs'] if j.get('name') == 'Fresh-checkout verification']
        if latest.get('status') != 'completed':
            receipt['state'] = 'pending'
        elif latest.get('conclusion') != 'success':
            receipt['state'] = 'failed'
        elif any(j.get('status') != 'completed' for j in required):
            receipt['state'] = 'pending'
        elif len(required) != 1 or required[0].get('conclusion') != 'success' or not any(
                step.get('status') == 'completed' and step.get('conclusion') == 'success'
                for step in required[0].get('steps', [])):
            receipt['state'] = 'failed'
        else:
            receipt['state'] = 'success'
        return receipt

    def cleanup(self, arguments, *, mutate):
        """Only task-owned refs; retain all objects, ignored files and broker evidence."""
        a = arguments
        status = self.status(a['pr'])
        if (not status['merged'] or status['head'] != a['head']
                or status['head_tree'] != a['tree'] or status['merge_commit'] != a['merge_commit']
                or not status['post_merge_verified'] or status['human_review_required']):
            raise DeliveryError('cleanup requires exact merged tree and successful merge CI')
        if self.git('status', '--porcelain', '--untracked-files=all').strip():
            raise DeliveryError('cleanup requires clean workspace')
        worktrees = self.git('worktree', 'list', '--porcelain').splitlines()
        if [line for line in worktrees if line.startswith('worktree ')] != ['worktree ' + str(self.root)]:
            raise DeliveryError('cleanup refuses additional worktrees')
        current = self.git('symbolic-ref', '--short', 'HEAD').strip()
        if current not in ('main', self.branch):
            raise DeliveryError('cleanup checkout ownership mismatch')
        refs = dict(line.split(' ', 1) for line in self.git('for-each-ref', '--format=%(refname) %(objectname)',
                                                          'refs/heads/').splitlines())
        tracking_ref = 'refs/remotes/origin/' + self.branch
        tracking = self.git('for-each-ref', '--format=%(objectname)', tracking_ref).strip()
        if tracking and tracking != a['head']:
            raise DeliveryError('cleanup tracking branch identity mismatch')
        local = refs.get('refs/heads/' + self.branch)
        if local is not None and local != a['head']:
            raise DeliveryError('cleanup local branch identity mismatch')
        # Successful ls-remote with no exact ref establishes absence, unlike a 404.
        remote_raw = self.git('ls-remote', '--heads', URL, 'refs/heads/' + self.branch).strip()
        remote = remote_raw.split('\t') if remote_raw else None
        if remote and remote != [a['head'], 'refs/heads/' + self.branch]:
            raise DeliveryError('cleanup remote branch identity mismatch')
        base = self.api(f'repos/{REPO}/branches/main')
        if base.get('name') != 'main':
            raise DeliveryError('cleanup base identity mismatch')
        base_head = _head(base.get('commit', {}).get('sha'))
        retained = {}
        for label, commit in (('reviewed', a['head']), ('merged', a['merge_commit'])):
            ref = f'refs/bokkie/delivery/pr-{a["pr"]}/{label}'
            existing = self.git('for-each-ref', '--format=%(objectname)', ref).strip()
            if existing and existing != commit:
                raise DeliveryError('cleanup retained evidence identity mismatch')
            retained[ref] = (commit, bool(existing))
        done = all(exists for _, exists in retained.values()) and current == 'main' and refs.get('refs/heads/main') == base_head and local is None and remote is None and not tracking
        if done:
            self.git('merge-base', '--is-ancestor', a['merge_commit'], base_head)
        if mutate and not done:
            self.git('-c', 'remote.origin.fetch=', 'fetch', '--no-tags', '--prune', 'origin',
                     '+refs/heads/main:refs/remotes/origin/main')
            if self.git('rev-parse', 'refs/remotes/origin/main').strip() != base_head:
                raise DeliveryError('cleanup base moved; reconcile fresh evidence')
            self.git('merge-base', '--is-ancestor', a['merge_commit'], base_head)
            self.git('merge-base', '--is-ancestor', 'refs/heads/main', base_head)
            if _head(self.git('rev-parse', a['head'] + '^{tree}').strip()) != a['tree']:
                raise DeliveryError('cleanup local reviewed tree mismatch')
            if _head(self.git('rev-parse', a['merge_commit'] + '^{tree}').strip()) != a['tree']:
                raise DeliveryError('cleanup local merge tree mismatch')
            for ref, (commit, exists) in retained.items():
                if not exists:
                    self.git('update-ref', ref, commit, '0' * 40)
            self.git('switch', '--no-overwrite-ignore', 'main')
            self.git('merge', '--no-overwrite-ignore', '--ff-only', base_head)
            if remote:
                self.git('-c', 'credential.helper=', '-c', 'credential.helper=!/usr/bin/gh auth git-credential',
                         'push', '--porcelain', '--force-with-lease=refs/heads/' + self.branch + ':' + a['head'],
                         URL, ':refs/heads/' + self.branch)
            if local:
                self.git('update-ref', '-d', 'refs/heads/' + self.branch, a['head'])
            self.git('-c', 'remote.origin.fetch=', 'fetch', '--no-tags', '--prune', 'origin',
                     '+refs/heads/main:refs/remotes/origin/main')
            if self.git('ls-remote', '--heads', URL, 'refs/heads/' + self.branch).strip():
                raise DeliveryError('cleanup remote task branch reappeared')
            if tracking:
                self.git('update-ref', '-d', tracking_ref, a['head'])
            return self.cleanup(a, mutate=False)
        status['cleanup'] = {'state': 'success' if done else 'pending', 'base_head': base_head,
                             'local_branch_deleted': local is None, 'remote_branch_deleted': remote is None, 'remote_tracking_deleted': not tracking,
                             'evidence_retained': all(exists for _, exists in retained.values()),
                             'actions': {
                                 'base': {'state': 'completed' if current == 'main' and refs.get('refs/heads/main') == base_head else 'blocked', 'reason': 'exact base checked out' if current == 'main' and refs.get('refs/heads/main') == base_head else 'fast-forward pending'},
                                 'local_branch': {'state': 'completed' if local is None else 'blocked', 'reason': 'absent' if local is None else 'exact task branch deletion pending'},
                                 'remote_branch': {'state': 'completed' if remote is None else 'blocked', 'reason': 'absent' if remote is None else 'lease-protected task branch deletion pending'},
                                 'remote_tracking': {'state': 'completed' if not tracking else 'blocked', 'reason': 'absent' if not tracking else 'exact task tracking ref prune pending'},
                                 'workspace_and_evidence': {'state': 'intentionally_retained', 'reason': 'checkout, ignored files and external broker receipts retained'},
                                 'git_evidence': {'state': 'intentionally_retained' if all(exists for _, exists in retained.values()) else 'blocked', 'reason': 'exact reviewed and merged objects retained' if all(exists for _, exists in retained.values()) else 'immutable evidence refs pending'}}}

        return status

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


def workspace_lock_root():
    # Same stable registry as broker.WorkspaceWriter, independent of caller HOME.
    return Path(pwd.getpwuid(os.getuid()).pw_dir) / '.local/state/bokkie/workspace-locks'


@contextmanager
def cleanup_ownership(host):
    root = workspace_lock_root()
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    if root.resolve() != root or root.is_relative_to(host.root):
        raise DeliveryError('cleanup lock storage must be canonical and outside workspace')
    metadata = root.stat()
    if metadata.st_uid != os.getuid() or stat.S_IMODE(metadata.st_mode) != 0o700:
        raise DeliveryError('cleanup lock storage must be private')
    path = root / (hashlib.sha256(os.fsencode(host.root)).hexdigest() + '.lock')
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_nlink != 1:
            raise DeliveryError('invalid cleanup lock inode')
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise DeliveryError('cleanup workspace has an active owner') from None
        previous = os.read(fd, 4097)
        try:
            if previous and json.loads(previous) != {}:
                raise DeliveryError('cleanup prior workspace owner lacks verified cessation')
        except (ValueError, UnicodeError):
            raise DeliveryError('cleanup prior workspace ownership is uncertain') from None
        # Do not overwrite the worker's durable marker. Child commands inherit
        # this lock so an adapter death cannot release it while Git is active.
        host.lock_fd = fd
        yield
    finally:
        host.lock_fd = None
        os.close(fd)


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
              'open_pr': {'head', 'title', 'body'},
              'update_pr': {'pr', 'head', 'title', 'body', 'expected_text_digest'},
              'closeout': {'pr', 'head', 'tree', 'merge_commit', 'review_digest', 'pre_merge_ci', 'post_merge_ci'}, 'status': {'pr'}, 'merge': {'pr', 'head'},
              'cleanup': {'pr', 'head', 'tree', 'merge_commit'}}
    if operation not in schema or not isinstance(args, dict) or set(args) != schema[operation]:
        raise DeliveryError('invalid delivery operation arguments')
    if 'head' in args:
        _head(args['head'])
    if 'pr' in args:
        _pr(args['pr'])
    if operation in ('cleanup', 'closeout'):
        _head(args['tree'])
        _head(args['merge_commit'])
    if operation in ('open_pr', 'update_pr'):
        for key, bound in (('title', 256), ('body', 30000)):
            if not isinstance(args[key], str) or not args[key].strip() or len(args[key].encode()) > bound or '\0' in args[key]:
                raise DeliveryError('invalid pull request text')
    for key in ('expected_text_digest', 'review_digest'):
        if key in args and (not isinstance(args[key], str) or not DIGEST.fullmatch(args[key])):
            raise DeliveryError('invalid delivery digest')


def execute(config, workspace, operation, arguments, *, run=subprocess.run):
    _UNCERTAIN.set(False)
    _arguments(operation, arguments)
    h = Host(config, workspace, run)
    a = arguments
    if operation == 'status':
        return h.status(a['pr'])
    if operation == 'cleanup':
        with cleanup_ownership(h):
            return h.cleanup(a, mutate=True)
    if operation == 'update_pr':
        return h.update_pr(a, mutate=True)
    if operation == 'closeout':
        with cleanup_ownership(h):
            return h.closeout(a, mutate=True)
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
        existing = h.find_pr(head)
        if existing:
            return h.pr_text_receipt(existing, a, 'existing')
        remote = h.api(f'repos/{REPO}/git/ref/heads/{h.branch}')
        if remote['object']['sha'] != head:
            raise DeliveryError('remote head mismatch')
        h.gh('pr', 'create', '--repo', 'github.com/' + REPO, '--base', 'main', '--head', h.branch,
             '--title', a['title'], '--body', a['body'])
        created = h.find_pr(head)
        if not created:
            raise DeliveryError('pull request creation needs reconciliation')
        return h.pr_text_receipt(created, a, 'created')
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
    if operation == 'cleanup':
        with cleanup_ownership(h):
            return h.cleanup(arguments, mutate=False)
    if operation == 'update_pr':
        return h.update_pr(arguments, mutate=False)
    if operation == 'closeout':
        return h.closeout(arguments, mutate=False)
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
        return h.pr_text_receipt(p, arguments, 'reconciled') if p else None
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
