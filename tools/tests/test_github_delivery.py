"""No network or remote mutations: disposable Git and scripted GitHub evidence."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location('github_delivery', Path(__file__).resolve().parents[1] / 'engineering-runtime/github_delivery.py')
delivery = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(delivery)
CONFIG = {'repo': 'robchristie/pagefold', 'base': 'main', 'branch': 'codex/test'}
HEAD = 'a' * 40


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.git('init', '-b', 'main')
        self.git('remote', 'add', 'origin', delivery.URL)
        self.git('config', 'user.name', 'Test')
        self.git('config', 'user.email', 'test@example.invalid')
        (self.root / 'file.txt').write_text('initial\n')
        self.git('add', 'file.txt')
        self.git('commit', '-m', 'Initial')
        self.responses = {}
        self.calls = []

    def git(self, *args):
        return subprocess.run(['/usr/bin/git', *args], cwd=self.root, capture_output=True, text=True, check=True).stdout.strip()

    def runner(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[0] == '/usr/bin/git':
            return subprocess.run(argv, **kwargs)
        self.assertEqual(kwargs['cwd'], '/')
        self.assertEqual(kwargs['env']['GIT_CONFIG_GLOBAL'], '/dev/null')
        key = argv[-1] if 'graphql' not in argv else 'graphql'
        self.assertIn(key, self.responses, argv)
        return subprocess.CompletedProcess(argv, 0, json.dumps(self.responses[key]), '')

    def host(self):
        return delivery.Host(CONFIG, self.root, self.runner)

    def prepare(self):
        return delivery.execute(CONFIG, self.root, 'prepare_branch', {}, run=self.runner)

    def evidence(self, *, head=HEAD, merged=False):
        base = 'repos/robchristie/pagefold/'
        self.responses[base + 'pulls/1'] = {
            'number': 1, 'base': {'ref': 'main', 'repo': {'full_name': delivery.REPO}},
            'head': {'ref': 'codex/test', 'sha': head, 'repo': {'full_name': delivery.REPO}},
            'html_url': 'https://github.com/robchristie/pagefold/pull/1', 'state': 'closed' if merged else 'open',
            'merged': merged, 'merge_commit_sha': 'b' * 40 if merged else None,
            'draft': False, 'mergeable': True, 'mergeable_state': 'clean'}
        self.responses[base + f'commits/{head}/check-runs?filter=latest&per_page=100'] = {
            'total_count': 1, 'check_runs': [{'name': 'Fresh-checkout verification', 'head_sha': head,
                'status': 'completed', 'conclusion': 'success', 'app': {'id': 15368}}]}
        self.responses[base + 'pulls/1/reviews?per_page=100'] = []
        self.responses['graphql'] = {'data': {'repository': {'pullRequest': {'reviewThreads': {
            'pageInfo': {'hasNextPage': False}, 'nodes': []}}}}}
        self.responses['repos/' + delivery.REPO] = {'full_name': delivery.REPO, 'html_url': 'https://github.com/' + delivery.REPO, 'permissions': {'push': True}, 'allow_squash_merge': True}
        self.responses[base + 'branches/main'] = {'name': 'main', 'protected': True, 'commit': {'sha': head}}
        self.responses[base + 'branches/main/protection'] = {'required_status_checks': {'contexts': ['Fresh-checkout verification']}}
        self.responses[base + 'rulesets?includes_parents=true&per_page=100'] = []
        for sha in (head, 'b' * 40):
            self.responses[base + f'actions/runs?head_sha={sha}&per_page=100'] = {'total_count': 1, 'workflow_runs': [
                {'id': 10 if sha == head else 11, 'name': 'CI', 'head_sha': sha, 'repository': {'full_name': delivery.REPO}, 'status': 'completed', 'conclusion': 'success'}]}
            run_id = 10 if sha == head else 11
            self.responses[base + f'actions/runs/{run_id}/jobs?filter=latest&per_page=100'] = {'total_count': 1, 'jobs': [
                {'name': 'Fresh-checkout verification', 'head_sha': sha, 'status': 'completed', 'conclusion': 'success',
                 'steps': [{'status': 'completed', 'conclusion': 'success'}]}]}
        return self.responses[base + 'pulls/1']

    def test_preflight_requires_supported_policy_and_actual_ci_job(self):
        self.evidence()
        self.responses['github.com'] = {}
        result = delivery.preflight(CONFIG, self.root, run=self.runner)
        self.assertTrue(result['base_ci_verified'])
        self.assertEqual(result['base_head'], HEAD)
        self.responses[f'repos/{delivery.REPO}/actions/runs/10/jobs?filter=latest&per_page=100']['jobs'] = []
        self.responses[f'repos/{delivery.REPO}/actions/runs/10/jobs?filter=latest&per_page=100']['total_count'] = 0
        with self.assertRaisesRegex(delivery.DeliveryError, 'representative run'):
            delivery.preflight(CONFIG, self.root, run=self.runner)

    def test_scope_rejected_before_commands(self):
        for key, value in [('repo', 'evil/pagefold'), ('base', 'dev'), ('branch', 'main'), ('branch', 'codex/a;id'), ('branch', 'codex/../x'), ('branch', 'codex/')]:
            with self.subTest(key=key, value=value), self.assertRaises(delivery.DeliveryError):
                delivery.preflight(dict(CONFIG, **{key: value}), self.root, run=self.runner)
        self.assertEqual(self.calls, [])

    def test_prepare_commit_and_readback(self):
        result = self.prepare()
        self.assertEqual(result['branch'], CONFIG['branch'])
        self.assertEqual(delivery.reconcile(CONFIG, self.root, 'prepare_branch', {}, run=self.runner), result)
        (self.root / 'file.txt').write_text('changed\n')
        (self.root / 'other.txt').write_text('unrelated\n')
        args = {'paths': ['file.txt'], 'message': 'Commit literal $(touch NEVER)'}
        committed = delivery.execute(CONFIG, self.root, 'commit', args, run=self.runner)
        self.assertNotEqual(committed['head'], result['head'])
        self.assertEqual(self.git('status', '--porcelain'), '?? other.txt')
        self.assertFalse((self.root / 'NEVER').exists())
        self.assertIsNone(delivery.reconcile(CONFIG, self.root, 'commit', args, run=self.runner))

    def test_commit_dotfiles_and_ci_workflow_preserves_unrelated_files(self):
        self.prepare()
        files = {'.gitignore': '/build/\n', '.gitattributes': '*.md text\n',
                 '.github/workflows/ci.yml': 'name: Synthetic CI\n'}
        for name, content in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        (self.root / 'unrelated.txt').write_text('preserve this work')
        result = delivery.execute(CONFIG, self.root, 'commit',
                                  {'paths': list(files), 'message': 'Prepare synthetic CI'}, run=self.runner)
        self.assertEqual(result['head'], self.git('rev-parse', 'HEAD'))
        for name, content in files.items():
            self.assertEqual(self.git('show', 'HEAD:' + name), content.strip())
        self.assertEqual(self.git('status', '--porcelain'), '?? unrelated.txt')
        self.assertEqual(set(self.git('diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD').splitlines()), set(files))

    def test_reject_paths_and_preexisting_index(self):
        self.prepare()
        for path in ('.', '..', './file.txt', '.git', '../oops', '/tmp/oops', ':!file.txt', '.git/config', 'x/../../y', '$(touch nope)'):
            with self.subTest(path=path), self.assertRaises(delivery.DeliveryError):
                delivery.execute(CONFIG, self.root, 'commit', {'paths': [path], 'message': 'Test'}, run=self.runner)
        (self.root / 'file.txt').write_text('changed')
        self.git('add', 'file.txt')
        with self.assertRaisesRegex(delivery.DeliveryError, 'staged'):
            delivery.execute(CONFIG, self.root, 'commit', {'paths': ['file.txt'], 'message': 'Test'}, run=self.runner)

    def test_reject_hook_config_and_linked_git(self):
        self.git('config', 'core.sshCommand', 'false')
        with self.assertRaisesRegex(delivery.DeliveryError, 'configuration'):
            self.host()
        self.git('config', '--unset', 'core.sshCommand')
        (self.root / '.git').rename(self.root / 'metadata')
        (self.root / '.git').symlink_to(self.root / 'metadata', target_is_directory=True)
        with self.assertRaisesRegex(delivery.DeliveryError, 'standalone'):
            self.host()

    def test_stale_head_stops_push(self):
        self.prepare()
        with self.assertRaisesRegex(delivery.DeliveryError, 'stale'):
            delivery.execute(CONFIG, self.root, 'push', {'head': HEAD}, run=self.runner)
        self.assertFalse(any('push' in argv for argv, _ in self.calls))

    def test_policy_red_missing_wrong_head_and_spoofed_app(self):
        for field, value in [('conclusion', 'failure'), ('head_sha', 'c' * 40), ('app', {'id': 1})]:
            self.evidence()
            checks = self.responses[f'repos/{delivery.REPO}/commits/{HEAD}/check-runs?filter=latest&per_page=100']
            checks['check_runs'][0][field] = value
            with self.subTest(field=field), self.assertRaises(delivery.DeliveryError):
                self.host().merge_policy(self.host().status(1))
        self.evidence()
        self.host().merge_policy(self.host().status(1))
        self.responses[f'repos/{delivery.REPO}/actions/runs?head_sha={HEAD}&per_page=100'] = {'total_count': 0, 'workflow_runs': []}
        with self.assertRaises(delivery.DeliveryError):
            self.host().merge_policy(self.host().status(1))

    def test_changes_requested_blocks_merge(self):
        head = self.prepare()['head']
        self.evidence(head=head)
        self.responses[f'repos/{delivery.REPO}/pulls/1/reviews?per_page=100'] = [{'state': 'CHANGES_REQUESTED', 'user': {'login': 'reviewer'}}]
        with self.assertRaisesRegex(delivery.DeliveryError, 'eligible'):
            delivery.execute(CONFIG, self.root, 'merge', {'pr': 1, 'head': head}, run=self.runner)
        self.assertFalse(any('merge' in argv for argv, _ in self.calls))

    def test_duplicate_pr_not_created_and_ambiguity_rejected(self):
        head = self.prepare()['head']
        pull = self.evidence(head=head)
        lookup = f'repos/{delivery.REPO}/pulls?state=all&head=robchristie:codex/test&base=main&per_page=100'
        self.responses[lookup] = [pull]
        args = {'head': head, 'title': 'Test', 'body': 'Body'}
        self.assertTrue(delivery.execute(CONFIG, self.root, 'open_pr', args, run=self.runner)['existing'])
        self.assertEqual(delivery.reconcile(CONFIG, self.root, 'open_pr', args, run=self.runner)['pr'], 1)
        self.responses[lookup] = [pull, pull]
        with self.assertRaisesRegex(delivery.DeliveryError, 'duplicate'):
            delivery.execute(CONFIG, self.root, 'open_pr', args, run=self.runner)
        self.assertFalse(any('create' in argv for argv, _ in self.calls))

    def test_merge_replay_and_post_merge_ci(self):
        self.evidence(merged=True)
        result = delivery.reconcile(CONFIG, self.root, 'merge', {'pr': 1, 'head': HEAD}, run=self.runner)
        self.assertTrue(result['merged'])
        self.assertTrue(result['post_merge_verified'])
        self.responses[f'repos/{delivery.REPO}/actions/runs?head_sha={"b" * 40}&per_page=100']['workflow_runs'][0]['conclusion'] = 'failure'
        self.assertFalse(self.host().status(1)['post_merge_verified'])
        self.assertIsNone(delivery.reconcile(CONFIG, self.root, 'merge', {'pr': 1, 'head': 'c' * 40}, run=self.runner))

    def test_status_rejects_wrong_remote_identity(self):
        pull = self.evidence()
        pull['head']['repo']['full_name'] = 'evil/pagefold'
        with self.assertRaisesRegex(delivery.DeliveryError, 'identity'):
            self.host().status(1)

    def test_push_replay_exact_sha(self):
        self.responses[f'repos/{delivery.REPO}/git/ref/heads/codex/test'] = {'object': {'sha': HEAD}}
        self.assertEqual(delivery.reconcile(CONFIG, self.root, 'push', {'head': HEAD}, run=self.runner)['head'], HEAD)
        self.assertIsNone(delivery.reconcile(CONFIG, self.root, 'push', {'head': 'c' * 40}, run=self.runner))

    def test_exact_merge_command_and_completed_readback(self):
        head = self.prepare()['head']
        self.evidence(head=head)
        merge_calls = []

        def runner(argv, **kwargs):
            if argv[:3] == ['/usr/bin/gh', 'pr', 'merge']:
                merge_calls.append(argv)
                self.evidence(head=head, merged=True)
                return subprocess.CompletedProcess(argv, 0, '', '')
            return self.runner(argv, **kwargs)

        result = delivery.execute(CONFIG, self.root, 'merge', {'pr': 1, 'head': head}, run=runner)
        self.assertTrue(result['post_merge_verified'])
        self.assertEqual(merge_calls, [['/usr/bin/gh', 'pr', 'merge', '1', '--repo',
            'github.com/robchristie/pagefold', '--squash', '--match-head-commit', head]])

    def test_unresolved_threads_and_policy_uncertainty_fail_closed(self):
        head = self.prepare()['head']
        self.evidence(head=head)
        self.responses['graphql']['data']['repository']['pullRequest']['reviewThreads']['nodes'] = [{'isResolved': False}]
        with self.assertRaisesRegex(delivery.DeliveryError, 'eligible'):
            delivery.execute(CONFIG, self.root, 'merge', {'pr': 1, 'head': head}, run=self.runner)
        self.evidence(head=head)
        self.responses[f'repos/{delivery.REPO}/rulesets?includes_parents=true&per_page=100'] = [{'enforcement': 'active'}]
        with self.assertRaisesRegex(delivery.DeliveryError, 'rulesets'):
            self.host().merge_policy(self.host().status(1))
        self.evidence(head=head)
        self.responses[f'repos/{delivery.REPO}/branches/main/protection'] = {}
        with self.assertRaisesRegex(delivery.DeliveryError, 'policy'):
            self.host().merge_policy(self.host().status(1))

    def test_human_review_hold_prevents_merge_before_mutation(self):
        head = self.prepare()['head']
        pull = self.evidence(head=head)
        pull['labels'] = [{'name': 'human-review-required'}]
        with self.assertRaisesRegex(delivery.DeliveryError, 'not eligible') as caught:
            delivery.execute(CONFIG, self.root, 'merge', {'pr': 1, 'head': head}, run=self.runner)
        self.assertFalse(caught.exception.uncertain)
        self.assertFalse(any(call[0][:3] == ['/usr/bin/gh', 'pr', 'merge'] for call in self.calls))

    def test_authoritative_unprotected_branch_is_supported(self):
        self.evidence()
        self.responses[f'repos/{delivery.REPO}/branches/main'] = {'name': 'main', 'protected': False}
        del self.responses[f'repos/{delivery.REPO}/branches/main/protection']
        self.host().merge_policy(self.host().status(1))
        self.assertFalse(any(argv[-1].endswith('/protection') for argv, _ in self.calls))
        self.responses[f'repos/{delivery.REPO}']['permissions']['push'] = False
        with self.assertRaisesRegex(delivery.DeliveryError, 'permission'):
            self.host().policy()

    def test_zero_jobs_or_steps_do_not_qualify_ci(self):
        self.evidence()
        jobs_key = f'repos/{delivery.REPO}/actions/runs/10/jobs?filter=latest&per_page=100'
        self.responses[jobs_key]['jobs'][0]['steps'] = []
        self.assertFalse(self.host().successful_ci(HEAD))
        self.responses[jobs_key] = {'total_count': 0, 'jobs': []}
        self.assertFalse(self.host().successful_ci(HEAD))

    def test_remote_mutations_require_clean_workspace_and_current_base(self):
        head = self.prepare()['head']
        self.evidence(head=head)
        (self.root / 'unreviewed.txt').write_text('unreviewed')
        with self.assertRaisesRegex(delivery.DeliveryError, 'clean workspace'):
            delivery.execute(CONFIG, self.root, 'push', {'head': head}, run=self.runner)
        (self.root / 'unreviewed.txt').unlink()
        self.responses[f'repos/{delivery.REPO}/branches/main']['commit']['sha'] = 'f' * 40
        with self.assertRaises(delivery.DeliveryError) as rejected:
            delivery.execute(CONFIG, self.root, 'push', {'head': head}, run=self.runner)
        self.assertFalse(rejected.exception.uncertain)
        self.assertFalse(any('push' in argv for argv, _ in self.calls))

    def test_failures_distinguish_validation_from_started_mutations(self):
        self.prepare()
        with self.assertRaises(delivery.DeliveryError) as rejected:
            delivery.execute(CONFIG, self.root, 'push', {'head': HEAD}, run=self.runner)
        self.assertFalse(rejected.exception.uncertain)
        (self.root / 'file.txt').write_text('changed')

        def failed_commit(argv, **kwargs):
            if argv[0] == '/usr/bin/git' and 'commit' in argv:
                return subprocess.CompletedProcess(argv, 1, '', 'private diagnostic')
            return self.runner(argv, **kwargs)

        with self.assertRaises(delivery.DeliveryError) as failed:
            delivery.execute(CONFIG, self.root, 'commit', {'paths': ['file.txt'], 'message': 'Change'}, run=failed_commit)
        self.assertTrue(failed.exception.uncertain)
        self.assertEqual(self.git('diff', '--cached', '--name-only'), 'file.txt')

    def test_errors_do_not_expose_diagnostics(self):
        def fail(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 1, 'SECRET', 'SECRET')
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.preflight(CONFIG, self.root, run=fail)
        self.assertNotIn('SECRET', str(caught.exception))


if __name__ == '__main__':
    unittest.main()
