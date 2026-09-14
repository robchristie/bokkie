"""No network or remote mutations: disposable Git and scripted GitHub evidence."""
import importlib.util
import fcntl
import hashlib
import os
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

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
        lock_tmp = tempfile.TemporaryDirectory()
        self.addCleanup(lock_tmp.cleanup)
        self.lock_root = Path(lock_tmp.name)
        self.lock_patch = patch.object(delivery, 'workspace_lock_root', return_value=self.lock_root)
        self.lock_patch.start()
        self.addCleanup(self.lock_patch.stop)
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
            self.responses[base + f'git/commits/{sha}'] = {'sha': sha, 'tree': {'sha': 'd' * 40}}
            self.responses[base + f'actions/runs?head_sha={sha}&per_page=100'] = {'total_count': 1, 'workflow_runs': [
                {'id': 10 if sha == head else 11, 'run_attempt': 1, 'html_url': f'https://github.com/{delivery.REPO}/actions/runs/{10 if sha == head else 11}', 'name': 'CI', 'head_sha': sha, 'repository': {'full_name': delivery.REPO}, 'status': 'completed', 'conclusion': 'success'}]}
            run_id = 10 if sha == head else 11
            self.responses[base + f'actions/runs/{run_id}/attempts/1/jobs?per_page=100'] = {'total_count': 1, 'jobs': [
                {'id': run_id + 100, 'run_id': run_id, 'run_attempt': 1, 'html_url': f'https://github.com/{delivery.REPO}/actions/runs/{run_id}/job/{run_id + 100}', 'name': 'Fresh-checkout verification', 'head_sha': sha, 'status': 'completed', 'conclusion': 'success',
                 'steps': [{'status': 'completed', 'conclusion': 'success'}]}]}
        return self.responses[base + 'pulls/1']

    def test_preflight_requires_supported_policy_and_actual_ci_job(self):
        self.evidence()
        self.responses['github.com'] = {}
        result = delivery.preflight(CONFIG, self.root, run=self.runner)
        self.assertTrue(result['base_ci_verified'])
        self.assertEqual(result['base_head'], HEAD)
        self.responses[f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100']['jobs'] = []
        self.responses[f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100']['total_count'] = 0
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

    def text_fixture(self):
        head = self.prepare()['head']
        pull = self.evidence(head=head)
        pull.update(title='Original', body='Review pending for old candidate')
        self.responses[f'repos/{delivery.REPO}/pulls?state=all&head=robchristie:codex/test&base=main&per_page=100'] = [pull]
        args = {'pr': 1, 'head': head, 'title': 'Addressable pages',
                'body': 'Review PASS\nUnicode: café. Literal $(touch NEVER)',
                'expected_text_digest': self.host().text_digest(pull)}
        return pull, args

    def publication_runner(self, *, lose_ack=False, retain=True, after_write=None):
        self.writes = []
        def runner(argv, **kwargs):
            if argv[0] == '/usr/bin/gh' and '--method' in argv:
                payload = json.loads(kwargs['input'])
                self.writes.append((argv, payload))
                if retain:
                    if argv[argv.index('--method') + 1] == 'PATCH':
                        self.responses[f'repos/{delivery.REPO}/pulls/1'].update(payload)
                    else:
                        self.responses[f'repos/{delivery.REPO}/issues/1/comments?per_page=100'].append({
                            'id': 12, 'user': {'id': 7}, 'body': payload['body'],
                            'html_url': f'https://github.com/{delivery.REPO}/pull/1#issuecomment-12'})
                if after_write:
                    after_write()
                return subprocess.CompletedProcess(argv, 1 if lose_ack else 0, '', '')
            return self.runner(argv, **kwargs)
        return runner

    def test_existing_pr_reports_unapplied_text_then_explicit_update_reads_back(self):
        pull, args = self.text_fixture()
        opening = {k: args[k] for k in ('head', 'title', 'body')}
        for action in (delivery.execute, delivery.reconcile):
            result = action(CONFIG, self.root, 'open_pr', opening, run=self.runner)
            self.assertFalse(result['text_applied'])
            self.assertIn('update_pr', result['next_action'])
            self.assertEqual(result['text_digest'], args['expected_text_digest'])
        runner = self.publication_runner()
        result = delivery.execute(CONFIG, self.root, 'update_pr', args, run=runner)
        self.assertTrue(result['text_applied'])
        self.assertEqual(result['disposition'], 'updated')
        self.assertEqual(pull['body'], args['body'])
        self.assertEqual(len(self.writes), 1)
        for action in (delivery.execute, delivery.reconcile):
            self.assertTrue(action(CONFIG, self.root, 'update_pr', args, run=runner)['text_applied'])
        self.assertEqual(len(self.writes), 1)
        self.assertFalse((self.root / 'NEVER').exists())

    def test_update_lost_ack_reconciles_without_second_mutation(self):
        _, args = self.text_fixture()
        runner = self.publication_runner(lose_ack=True)
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.execute(CONFIG, self.root, 'update_pr', args, run=runner)
        self.assertTrue(caught.exception.uncertain)
        self.assertTrue(delivery.reconcile(CONFIG, self.root, 'update_pr', args, run=runner)['text_applied'])
        self.assertEqual(len(self.writes), 1)

    def test_update_requires_observed_text_open_head_and_readback(self):
        pull, args = self.text_fixture()
        runner = self.publication_runner()
        for field, bad in [('expected_text_digest', 'f' * 64), ('head', 'f' * 40),
                           ('title', ''), ('body', 'é' * 15001)]:
            with self.subTest(field=field), self.assertRaises(delivery.DeliveryError):
                delivery.execute(CONFIG, self.root, 'update_pr', dict(args, **{field: bad}), run=runner)
        pull['state'] = 'closed'
        with self.assertRaises(delivery.DeliveryError):
            delivery.execute(CONFIG, self.root, 'update_pr', args, run=runner)
        self.assertEqual(self.writes, [])
        pull['state'] = 'open'
        runner = self.publication_runner(retain=False)
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.execute(CONFIG, self.root, 'update_pr', args, run=runner)
        self.assertTrue(caught.exception.uncertain)
        self.assertIsNone(delivery.reconcile(CONFIG, self.root, 'update_pr', args, run=runner))
        self.assertEqual(len(self.writes), 1)

    def test_update_detects_head_moving_during_patch(self):
        pull, args = self.text_fixture()
        runner = self.publication_runner(after_write=lambda: pull['head'].update(sha='f' * 40))
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.execute(CONFIG, self.root, 'update_pr', args, run=runner)
        self.assertTrue(caught.exception.uncertain)

    def closeout_fixture(self):
        self.evidence(merged=True)
        self.responses['user'] = {'id': 7}
        self.responses[f'repos/{delivery.REPO}/issues/1/comments?per_page=100'] = []
        status = self.host().status(1)
        return {'pr': 1, 'head': HEAD, 'tree': 'd' * 40, 'merge_commit': 'b' * 40,
                'review_digest': 'e' * 64, 'pre_merge_ci': status['pre_merge_ci']['run'],
                'post_merge_ci': status['post_merge_ci']['run']}

    def test_closeout_exact_evidence_idempotent_after_cleanup_on_main(self):
        args = self.closeout_fixture()
        runner = self.publication_runner()
        result = delivery.execute(CONFIG, self.root, 'closeout', args, run=runner)
        self.assertEqual(result['closeout']['state'], 'published')
        body = self.writes[0][1]['body']
        for text in (HEAD, 'd' * 40, 'b' * 40, 'e' * 64, 'actions/runs/10', 'actions/runs/11',
                     'Product acceptance and cleanup are tracked separately'):
            self.assertIn(text, body)
        for action in (delivery.execute, delivery.reconcile):
            self.assertEqual(action(CONFIG, self.root, 'closeout', args, run=runner), result)
        self.assertEqual(len(self.writes), 1)
        self.assertEqual(self.git('symbolic-ref', '--short', 'HEAD'), 'main')

    def test_closeout_lost_ack_and_absence_never_blindly_repost(self):
        args = self.closeout_fixture()
        runner = self.publication_runner(lose_ack=True)
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.execute(CONFIG, self.root, 'closeout', args, run=runner)
        self.assertTrue(caught.exception.uncertain)
        self.assertEqual(delivery.reconcile(CONFIG, self.root, 'closeout', args, run=runner)['closeout']['state'], 'published')
        comments = self.responses[f'repos/{delivery.REPO}/issues/1/comments?per_page=100']
        comments.clear()
        self.assertIsNone(delivery.reconcile(CONFIG, self.root, 'closeout', args, run=runner))
        self.assertEqual(len(self.writes), 1)

    def test_closeout_rejects_unverified_merge_and_changed_ci(self):
        args = self.closeout_fixture()
        runner = self.publication_runner()
        for field, value in [('head', 'f' * 40), ('tree', 'f' * 40), ('merge_commit', 'f' * 40),
                             ('review_digest', 'not-a-digest'), ('pre_merge_ci', {})]:
            with self.subTest(field=field), self.assertRaises(delivery.DeliveryError):
                delivery.execute(CONFIG, self.root, 'closeout', dict(args, **{field: value}), run=runner)
        self.responses[f'repos/{delivery.REPO}/actions/runs?head_sha={"b" * 40}&per_page=100']['workflow_runs'][0]['conclusion'] = 'failure'
        with self.assertRaises(delivery.DeliveryError):
            delivery.execute(CONFIG, self.root, 'closeout', args, run=runner)
        self.assertEqual(self.writes, [])

    def test_closeout_rejects_spoofed_modified_duplicate_and_unbounded_comments(self):
        args = self.closeout_fixture()
        runner = self.publication_runner()
        delivery.execute(CONFIG, self.root, 'closeout', args, run=runner)
        comments = self.responses[f'repos/{delivery.REPO}/issues/1/comments?per_page=100']
        original = json.loads(json.dumps(comments[0]))
        for field, value in [('user', {'id': 8}), ('body', original['body'] + 'changed'),
                             ('html_url', 'https://evil.invalid/')]:
            comments[:] = [dict(original, **{field: value})]
            with self.subTest(field=field), self.assertRaises(delivery.DeliveryError):
                delivery.reconcile(CONFIG, self.root, 'closeout', args, run=runner)
        for count in (2, 100):
            comments[:] = [original] * count
            with self.assertRaises(delivery.DeliveryError):
                delivery.execute(CONFIG, self.root, 'closeout', args, run=runner)
        self.assertEqual(len(self.writes), 1)

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
        jobs_key = f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100'
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

    def test_ci_receipts_bind_attempts_urls_and_distinct_observations(self):
        self.evidence(merged=True)
        status = self.host().status(1)
        self.assertEqual(status['head_tree'], 'd' * 40)
        self.assertTrue(status['tree_equal'])
        self.assertEqual(status['pre_merge_ci']['run']['id'], 10)
        self.assertEqual(status['post_merge_ci']['jobs'][0]['run_id'], 11)
        self.assertEqual(status['post_merge_ci']['jobs'][0]['attempt'], 1)
        job = self.responses[f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100']['jobs'][0]
        del job['run_attempt']  # Attempt is authoritative in the requested endpoint.
        job['html_url'] = f'https://github.com/{delivery.REPO}/runs/10/jobs/110'
        self.assertEqual(self.host().ci_receipt(HEAD)['state'], 'success')
        key = f'repos/{delivery.REPO}/actions/runs?head_sha={HEAD}&per_page=100'
        self.responses[key]['workflow_runs'][0]['status'] = 'queued'
        self.assertEqual(self.host().ci_receipt(HEAD)['state'], 'pending')
        self.responses[key]['workflow_runs'][0].update(status='completed', conclusion='failure')
        self.assertEqual(self.host().ci_receipt(HEAD)['state'], 'failed')
        self.responses[key] = {'total_count': 0, 'workflow_runs': []}
        self.assertEqual(self.host().ci_receipt(HEAD)['state'], 'pending')
        def unavailable(argv, **kwargs):
            if argv[0] == '/usr/bin/gh':
                return subprocess.CompletedProcess(argv, 1, '', 'private')
            return self.runner(argv, **kwargs)
        self.assertEqual(delivery.Host(CONFIG, self.root, unavailable).ci_receipt(HEAD)['state'], 'unavailable')

    def test_tree_mismatch_attempt_spoofing_and_pagination_cannot_qualify(self):
        self.evidence(merged=True)
        self.responses[f'repos/{delivery.REPO}/git/commits/{"b" * 40}']['tree']['sha'] = 'e' * 40
        self.assertFalse(self.host().status(1)['post_merge_verified'])
        for field, value in [('run_id', 99), ('run_attempt', 2), ('head_sha', 'f' * 40), ('html_url', 'https://evil.invalid/')]:
            self.evidence()
            self.responses[f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100']['jobs'][0][field] = value
            with self.subTest(field=field), self.assertRaisesRegex(delivery.DeliveryError, 'identity'):
                self.host().ci_receipt(HEAD)
        self.evidence()
        self.responses[f'repos/{delivery.REPO}/actions/runs/10/attempts/1/jobs?per_page=100']['total_count'] = 101
        with self.assertRaisesRegex(delivery.DeliveryError, 'bound'):
            self.host().ci_receipt(HEAD)

    def cleanup_fixture(self):
        base = self.git('rev-parse', 'main')
        self.prepare()
        (self.root / 'file.txt').write_text('reviewed change')
        self.git('add', 'file.txt')
        self.git('commit', '-m', 'Change')
        head = self.git('rev-parse', 'HEAD')
        tree = self.git('rev-parse', 'HEAD^{tree}')
        merged = self.git('commit-tree', tree, '-p', base, '-m', 'Squash')
        self.evidence(head=head, merged=True)
        prefix = f'repos/{delivery.REPO}/'
        self.responses[prefix + 'pulls/1']['merge_commit_sha'] = merged
        self.responses[prefix + 'branches/main']['commit']['sha'] = merged
        self.responses[prefix + f'git/commits/{head}']['tree']['sha'] = tree
        self.responses[prefix + f'git/commits/{merged}'] = {'sha': merged, 'tree': {'sha': tree}}
        runs = self.responses[prefix + f'actions/runs?head_sha={"b" * 40}&per_page=100']
        runs['workflow_runs'][0]['head_sha'] = merged
        self.responses[prefix + f'actions/runs?head_sha={merged}&per_page=100'] = runs
        self.responses[prefix + 'actions/runs/11/attempts/1/jobs?per_page=100']['jobs'][0]['head_sha'] = merged
        self.git('update-ref', 'refs/remotes/origin/codex/test', head)
        remote = {'head': head, 'lose_ack': False}
        def runner(argv, **kwargs):
            if argv[0] == '/usr/bin/git' and 'ls-remote' in argv:
                output = remote['head'] + '\trefs/heads/codex/test\n' if remote['head'] else ''
                return subprocess.CompletedProcess(argv, 0, output, '')
            if argv[0] == '/usr/bin/git' and 'fetch' in argv:
                self.assertEqual(argv[-1], '+refs/heads/main:refs/remotes/origin/main')
                self.assertIn('remote.origin.fetch=', argv)
                self.git('update-ref', 'refs/remotes/origin/main', merged)
                return subprocess.CompletedProcess(argv, 0, '', '')
            if argv[0] == '/usr/bin/git' and 'push' in argv:
                self.assertIn('--force-with-lease=refs/heads/codex/test:' + head, argv)
                self.assertIn(':refs/heads/codex/test', argv)
                remote['head'] = None
                return subprocess.CompletedProcess(argv, 1 if remote['lose_ack'] else 0, '', '')
            return self.runner(argv, **kwargs)
        return {'pr': 1, 'head': head, 'tree': tree, 'merge_commit': merged}, remote, runner

    def test_cleanup_exact_refs_retains_evidence_and_is_idempotent(self):
        args, remote, runner = self.cleanup_fixture()
        (self.root / '.git/info/exclude').write_text('scratch/\n')
        (self.root / 'scratch').mkdir()
        (self.root / 'scratch/receipt').write_text('retained')
        result = delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        self.assertEqual(result['cleanup']['state'], 'success')
        self.assertIsNone(remote['head'])
        self.assertTrue(result['cleanup']['remote_tracking_deleted'])
        self.assertEqual(self.git('symbolic-ref', '--short', 'HEAD'), 'main')
        self.assertEqual(self.git('rev-parse', 'main'), args['merge_commit'])
        self.assertEqual(self.git('rev-parse', 'refs/bokkie/delivery/pr-1/reviewed'), args['head'])
        self.assertEqual((self.root / 'scratch/receipt').read_text(), 'retained')
        self.assertEqual(delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner), result)
        self.assertEqual(delivery.reconcile(CONFIG, self.root, 'cleanup', args, run=runner), result)

    def test_cleanup_lost_ack_reads_partial_then_resumes_safely(self):
        args, remote, runner = self.cleanup_fixture()
        remote['lose_ack'] = True
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        self.assertTrue(caught.exception.uncertain)
        observed = delivery.reconcile(CONFIG, self.root, 'cleanup', args, run=runner)
        self.assertEqual(observed['cleanup']['state'], 'pending')
        self.assertTrue(observed['cleanup']['remote_branch_deleted'])
        self.assertFalse(observed['cleanup']['local_branch_deleted'])
        self.assertEqual(delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)['cleanup']['state'], 'success')

    def test_cleanup_rejects_dirty_changed_refs_and_foreign_worktrees(self):
        args, remote, runner = self.cleanup_fixture()
        (self.root / 'unrelated').write_text('preserve')
        with self.assertRaisesRegex(delivery.DeliveryError, 'clean'):
            delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        (self.root / 'unrelated').unlink()
        remote['head'] = 'e' * 40
        with self.assertRaisesRegex(delivery.DeliveryError, 'remote branch'):
            delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        remote['head'] = args['head']
        with tempfile.TemporaryDirectory() as other:
            self.git('worktree', 'add', '--detach', other)
            with self.assertRaisesRegex(delivery.DeliveryError, 'worktrees'):
                delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
            self.git('worktree', 'remove', other)
        self.git('commit', '--allow-empty', '-m', 'Unrelated new work')
        with self.assertRaisesRegex(delivery.DeliveryError, 'local branch'):
            delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        self.assertEqual(remote['head'], args['head'])

    def test_cleanup_respects_shared_lock_and_uncertain_worker_marker(self):
        args, _, runner = self.cleanup_fixture()
        path = self.lock_root / (hashlib.sha256(os.fsencode(self.root)).hexdigest() + '.lock')
        with path.open('w+') as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(delivery.DeliveryError, 'active owner'):
                delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        path.write_text('{"workspace": "prior", "owner": "uncertain"}')
        with self.assertRaisesRegex(delivery.DeliveryError, 'ownership|cessation'):
            delivery.execute(CONFIG, self.root, 'cleanup', args, run=runner)
        self.assertIn('uncertain', path.read_text())
        path.write_text('{}')
        observed = []
        def inherited(argv, **kwargs):
            if 'pass_fds' in kwargs:
                with path.open() as handle:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                observed.append(kwargs['pass_fds'])
            return runner(argv, **kwargs)
        self.assertEqual(delivery.execute(CONFIG, self.root, 'cleanup', args, run=inherited)['cleanup']['state'], 'success')
        self.assertTrue(observed)
        self.assertEqual(path.read_text(), '{}')

    def test_cleanup_lock_survives_adapter_fd_close_until_child_exits(self):
        host = self.host()
        path = self.lock_root / (hashlib.sha256(os.fsencode(self.root)).hexdigest() + '.lock')
        with delivery.cleanup_ownership(host):
            child = subprocess.Popen([sys.executable, '-c', 'import sys; print("ready", flush=True); sys.stdin.read(1)'],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, pass_fds=(host.lock_fd,))
            self.assertEqual(child.stdout.readline().strip(), 'ready')
        try:
            with path.open() as handle:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        finally:
            child.communicate('x', timeout=5)
        with path.open() as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_errors_do_not_expose_diagnostics(self):
        def fail(argv, **kwargs):
            return subprocess.CompletedProcess(argv, 1, 'SECRET', 'SECRET')
        with self.assertRaises(delivery.DeliveryError) as caught:
            delivery.preflight(CONFIG, self.root, run=fail)
        self.assertNotIn('SECRET', str(caught.exception))


if __name__ == '__main__':
    unittest.main()
