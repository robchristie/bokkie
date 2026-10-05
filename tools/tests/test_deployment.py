"""Offline deployment contracts; no Docker socket or account material is used."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import re
import tempfile
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('deployment_manager', ROOT / 'deploy/manage.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix='bokkie-deployment-test-')
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / 'data').mkdir()
        (self.root / 'web-auth').write_text('synthetic authentication fixture\n')
        (self.root / 'codex-auth').write_text('{}\n')
        self.config = {
            'name': 'bokkie-test', 'source': 'a' * 40,
            'image': 'sha256:' + 'b' * 64, 'edge_image': 'sha256:' + 'c' * 64,
            'hostname': 'bokkie-test.yutani.tech', 'uid': 10001, 'gid': 10001,
            'data': str(self.root / 'data'), 'web_auth': str(self.root / 'web-auth'),
            'codex_auth': None, 'conversation_profile': None,
        }
        self.profile = {
            'broker': '/opt/conversation/broker.py', 'codex': '/usr/local/bin/codex',
            'bwrap': '/usr/bin/bwrap', 'model': 'synthetic-model', 'effort': 'medium',
        }
        self.api = self.enterContext(patch.object(
            MODULE, 'api', side_effect=AssertionError('unexpected Engine access')))
        self.enterContext(patch.object(
            MODULE.subprocess, 'run', side_effect=AssertionError('unexpected subprocess')))

    def load(self, config):
        (self.root / 'release.json').write_text(json.dumps(config))
        return MODULE.load(self.root)

    def observed(self, suffix, running=True):
        return {'Id': suffix.removeprefix('-') + '-identity',
                'Config': {'Labels': {'bokkie.deployment': self.config['name']}},
                'State': {'Running': running, 'StartedAt': '2026-10-05T00:00:00Z'}}

    def engine_with_containers(self, containers):
        def respond(method, path, value=None, missing=False):
            if method == 'GET':
                for suffix, observed in containers.items():
                    if path == '/containers/' + self.config['name'] + suffix + '/json':
                        return copy.deepcopy(observed)
            elif method in ('POST', 'DELETE'):
                identities = {item['Id'] for item in containers.values() if item}
                if any(path in ('/containers/' + identifier,
                                '/containers/' + identifier + '/stop?t=15')
                       for identifier in identities):
                    return None
            raise AssertionError((method, path, value, missing))
        self.api.side_effect = respond

    def mutation_calls(self):
        return [(call.args[0], call.args[1]) for call in self.api.call_args_list
                if call.args[0] != 'GET']

    def expected_stop_calls(self):
        return [('POST', '/containers/edge-identity/stop?t=15'),
                ('DELETE', '/containers/edge-identity'),
                ('POST', '/containers/runtime-identity/stop?t=15'),
                ('DELETE', '/containers/runtime-identity')]

    def test_valid_release_accepts_disabled_and_paired_conversation(self):
        self.assertEqual(self.load(self.config), self.config)
        self.config.update(codex_auth=str(self.root / 'codex-auth'),
                           conversation_profile=self.profile)
        self.assertEqual(self.load(self.config), self.config)

    def test_mutable_or_malformed_image_and_source_are_rejected(self):
        for key, value in (('image', 'bokkie:latest'), ('edge_image', 'nginx:stable'),
                           ('image', 'sha256:' + 'b' * 63), ('source', 'main'),
                           ('source', 'a' * 39)):
            with self.subTest(key=key, value=value):
                config = {**self.config, key: value}
                with self.assertRaises(ValueError):
                    self.load(config)

    def test_image_revision_mismatch_fails_before_container_mutation(self):
        def inspect(method, path, value=None, missing=False):
            if method == 'GET' and path == '/info':
                return {'ServerVersion': '29.8.1', 'KernelVersion': '6.12.73+deb13-amd64',
                        'SecurityOptions': ['name=apparmor,profile=default',
                                            'name=seccomp,profile=builtin', 'name=cgroupns']}
            if method == 'GET' and path == '/images/' + self.config['image'] + '/json':
                return {'Config': {'Labels': {'org.opencontainers.image.revision': 'd' * 40}}}
            raise AssertionError((method, path))
        self.api.side_effect = inspect
        with self.assertRaisesRegex(RuntimeError, 'source identity differs'):
            MODULE.start(self.config, self.root)
        self.assertEqual(self.mutation_calls(), [])

    def test_root_and_non_integer_runtime_identities_are_rejected(self):
        for key in ('uid', 'gid'):
            for value in (0, -1, 65535, True, '10001', 10001.0):
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    self.load({**self.config, key: value})

    def test_account_and_profile_must_be_enabled_together(self):
        for change in ({'codex_auth': str(self.root / 'codex-auth')},
                       {'conversation_profile': self.profile}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.load({**self.config, **change})

    def test_profile_cannot_replace_packaged_executables(self):
        for executable in ('broker', 'codex', 'bwrap'):
            config = {**self.config, 'codex_auth': str(self.root / 'codex-auth'),
                      'conversation_profile': {**self.profile, executable: '/tmp/substitute'}}
            with self.subTest(executable=executable), self.assertRaises(ValueError):
                self.load(config)

    def test_paths_require_existing_files_and_separate_data_directory(self):
        for key, value in (('data', str(self.root / 'web-auth')),
                           ('web_auth', str(self.root / 'data')),
                           ('web_auth', 'relative-auth'),
                           ('data', str(self.root / 'missing'))):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                self.load({**self.config, key: value})
        with self.assertRaises(ValueError):
            self.load({**self.config, 'codex_auth': str(self.root / 'data'),
                       'conversation_profile': self.profile})

    def test_runtime_preserves_qualified_boundary_inputs(self):
        expected = MODULE.runtime(self.config, self.root)
        host = expected['HostConfig']
        policy = ROOT / 'tools/container-probe/policy'
        for key, value in json.loads((policy / 'paths.json').read_text()).items():
            self.assertEqual(host[key], value, key)
        security = host['SecurityOpt']
        seccomp = [item.removeprefix('seccomp=') for item in security
                   if item.startswith('seccomp=')]
        self.assertEqual(len(seccomp), 1)
        self.assertEqual(json.loads(seccomp[0]), json.loads((policy / 'seccomp.json').read_text()))
        self.assertIn('no-new-privileges:true', security)
        self.assertIn('apparmor=' + MODULE.profile_name(self.config), security)
        self.assertFalse(any('unconfined' in item for item in security))
        self.assertEqual(host['CapDrop'], ['ALL'])
        self.assertFalse(host.get('CapAdd'))
        self.assertTrue(host['ReadonlyRootfs'])
        self.assertEqual(host['RestartPolicy']['Name'], 'no')
        self.assertEqual(expected['User'], '10001:10001')
        self.assertIn('HOME=/home/probe', expected['Env'])
        self.assertFalse(host.get('PortBindings'))
        self.assertGreater(host['PidsLimit'], 0)
        self.assertGreater(host['Memory'], 0)
        self.assertGreater(host['NanoCpus'], 0)

    def test_credential_mount_is_one_read_only_file_and_never_host_home(self):
        self.config.update(codex_auth=str(self.root / 'codex-auth'),
                           conversation_profile=self.profile)
        mounts = MODULE.runtime(self.config, self.root)['HostConfig']['Mounts']
        auth = [mount for mount in mounts if mount['Target'].startswith('/home/')]
        self.assertEqual(len(auth), 1)
        self.assertEqual(auth[0]['Source'], self.config['codex_auth'])
        self.assertEqual(auth[0]['Target'], '/home/probe/.codex/auth.json')
        self.assertTrue(auth[0]['ReadOnly'])
        self.assertFalse(any(mount['Target'] == '/var/run/docker.sock' for mount in mounts))

    def test_account_policy_is_conditional_exact_file_and_changes_identity(self):
        baseline = MODULE.policy_text(self.config)
        identity = MODULE.profile_name(self.config)
        self.assertEqual(baseline, (MODULE.POLICY / 'apparmor.profile').read_text())
        self.config.update(codex_auth=str(self.root / 'codex-auth'),
                           conversation_profile=self.profile)
        policy = MODULE.policy_text(self.config)
        added = [line for line in policy.splitlines() if line not in baseline.splitlines()]
        self.assertEqual(added, [
            '  mount options=(rw,rbind,silent) /oldroot/home/probe/.codex/auth.json'
            ' -> /newroot/home/probe/.codex/auth.json,'])
        self.assertNotEqual(identity, MODULE.profile_name(self.config))
        MODULE.render(self.config, self.root)
        self.assertEqual((self.root / 'apparmor.profile').read_text(),
                         policy.replace('BOKKIE_PROFILE', MODULE.profile_name(self.config)))

    def test_account_missing_non_regular_and_symlink_sources_fail_closed(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.root / 'codex-auth')
        for path in (self.root / 'missing', self.root / 'data', alias):
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.load({**self.config, 'codex_auth': str(path),
                           'conversation_profile': self.profile})

    def test_unreadable_configured_account_fails_readiness(self):
        self.config.update(codex_auth=str(self.root / 'codex-auth'),
                           conversation_profile=self.profile)
        def execute(arguments, **_kwargs):
            if arguments[-1] == 'test -r /run/bokkie-web-auth':
                return Mock(returncode=0)
            # Exercise the actual generated account check locally with a denied
            # access result, without depending on the test runner's Unix UID.
            script = arguments[arguments.index('-c') + 1]
            check = script[script.index("if sys.argv[3]"):script.index('for port,path,status')]
            with patch('os.access', return_value=False), patch('pathlib.Path.is_file', return_value=True):
                with self.assertRaisesRegex(AssertionError, 'configured account is not readable'):
                    exec(check, {'sys': Mock(argv=['probe', 'host', 'profile', 'enabled']),
                                 'Path': Path, 'os': __import__('os')})
            return Mock(returncode=1, stderr='configured account is not readable')
        with patch.object(MODULE.subprocess, 'run', side_effect=execute), \
                patch.object(MODULE.STOP, 'wait'), \
                self.assertRaisesRegex(RuntimeError, 'configured account is not readable'):
            MODULE.readiness(self.config, 'synthetic-runtime')

    def test_effective_authority_relaxations_are_rejected(self):
        expected = MODULE.runtime(self.config, self.root)
        actual = {'HostConfig': copy.deepcopy(expected['HostConfig']),
                  'Config': {key: copy.deepcopy(expected[key])
                             for key in ('User', 'Entrypoint', 'Cmd', 'Env', 'Labels')},
                  'Image': expected['Image'], 'State': {'Running': True}}
        actual['HostConfig'].update(Privileged=False, CapAdd=None, PortBindings={},
                                    PidMode='', IpcMode='private', UsernsMode='')
        MODULE.validate_runtime(expected, actual)
        for key, value in {'MaskedPaths': [], 'SecurityOpt': [], 'CapDrop': [],
                           'CapAdd': ['SYS_ADMIN'], 'Privileged': True,
                           'ReadonlyRootfs': False, 'PidMode': 'host', 'IpcMode': 'host',
                           'PortBindings': {'7744/tcp': [{'HostPort': '7744'}]},
                           'Memory': 0, 'PidsLimit': -1}.items():
            changed = copy.deepcopy(actual)
            changed['HostConfig'][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                MODULE.validate_runtime(expected, changed)

    def test_ingress_authentication_applies_to_every_proxied_path(self):
        MODULE.render(self.config, self.root)
        nginx = (self.root / 'nginx.conf').read_text()
        # Every route, including the backend-owned root redirect, uses the catch-all;
        # no asset or API route may opt out or reach a separate upstream.
        locations = re.findall(r'\blocation\s+([^{}]+)\{([^{}]*)\}', nginx)
        self.assertEqual([selector.strip() for selector, _ in locations], ['/'])
        self.assertEqual(len(re.findall(r'\bserver\s*\{', nginx)), 1)
        self.assertRegex(nginx, r'auth_basic\s+"Bokkie";')
        self.assertIn('auth_basic_user_file /run/bokkie-web-auth;', nginx)
        self.assertLess(nginx.index('auth_basic '), nginx.index('location '))
        self.assertNotRegex(nginx, r'auth_basic\s+off\b|satisfy\s+any\b')
        self.assertEqual(nginx.count('proxy_pass '), 1)
        self.assertIn('proxy_pass http://127.0.0.1:7744;', nginx)
        self.assertIn('proxy_set_header Authorization "";', nginx)
        self.assertIn('proxy_set_header Forwarded "";', nginx)
        self.assertIn('if ($http_host != "bokkie-test.yutani.tech") { return 421; }', nginx)

    def test_ingress_config_is_readable_under_service_umask_without_exposing_credentials(self):
        account = self.root / 'codex-auth'
        web_auth = self.root / 'web-auth'
        for path in (account, web_auth):
            path.chmod(0o600)
        self.config.update(codex_auth=str(account), conversation_profile=self.profile)
        previous = os.umask(0o077)
        try:
            # Cover both first creation and repair of a previously private file.
            for existing in (False, True):
                with self.subTest(existing=existing):
                    if existing:
                        (self.root / 'nginx.conf').chmod(0o600)
                    MODULE.render(self.config, self.root)
                    self.assertEqual((self.root / 'nginx.conf').stat().st_mode & 0o777, 0o644)
                    for private in (account, web_auth, self.root / 'conversation.json'):
                        self.assertEqual(private.stat().st_mode & 0o777, 0o600)
            self.assertEqual(account.read_text(), '{}\n')
            self.assertEqual(web_auth.read_text(), 'synthetic authentication fixture\n')
        finally:
            os.umask(previous)

    def test_edge_has_no_separate_network_or_published_port(self):
        edge = MODULE.edge(self.config, self.root)['services']['edge']
        self.assertEqual(edge['network_mode'], 'container:bokkie-test-runtime')
        self.assertNotIn('networks', edge)
        self.assertFalse(edge.get('ports'))
        self.assertEqual(edge['restart'], 'no')
        self.assertEqual(edge['labels']['traefik.enable'], 'false')
        auth = [mount for mount in edge['volumes'] if mount['target'] == '/run/bokkie-web-auth']
        self.assertEqual(len(auth), 1)
        self.assertTrue(auth[0]['read_only'])

    def test_stop_removes_edge_before_stopping_namespace_owner(self):
        self.engine_with_containers({suffix: self.observed(suffix)
                                     for suffix in ('-runtime', '-edge')})
        MODULE.stop(self.config)
        self.assertEqual(self.mutation_calls(), self.expected_stop_calls())

    def test_foreign_edge_blocks_all_mutations(self):
        edge = self.observed('-edge')
        edge['Config']['Labels']['bokkie.deployment'] = 'another-service'
        self.engine_with_containers({'-edge': edge, '-runtime': self.observed('-runtime')})
        with self.assertRaisesRegex(RuntimeError, 'different owner'):
            MODULE.stop(self.config)
        self.assertEqual(self.mutation_calls(), [])

    def test_missing_owner_label_cannot_authorise_runtime_mutation(self):
        runtime = self.observed('-runtime')
        runtime['Config']['Labels'] = {}
        self.engine_with_containers({'-edge': None, '-runtime': runtime})
        with self.assertRaisesRegex(RuntimeError, 'different owner'):
            MODULE.stop(self.config)
        self.assertEqual(self.mutation_calls(), [])

    def test_edge_stop_failure_preserves_runtime_namespace(self):
        def fail_edge(method, path, value=None, missing=False):
            if method == 'GET' and path.endswith('-edge/json'):
                return self.observed('-edge')
            raise RuntimeError('edge did not stop')
        self.api.side_effect = fail_edge
        with self.assertRaisesRegex(RuntimeError, 'edge did not stop'):
            MODULE.stop(self.config)
        self.assertEqual(self.mutation_calls(), [self.expected_stop_calls()[0]])

    def test_supervisor_detects_restart_with_unchanged_container_id(self):
        for restarted in ('-runtime', '-edge'):
            with self.subTest(restarted=restarted):
                containers = {suffix: self.observed(suffix) for suffix in ('-runtime', '-edge')}
                initial = tuple(MODULE.identity(containers[suffix])
                                for suffix in ('-runtime', '-edge'))
                containers[restarted]['State']['StartedAt'] = '2026-10-05T00:00:01Z'
                self.api.reset_mock()
                self.engine_with_containers(containers)
                stop_event = Mock()
                stop_event.wait.side_effect = [False, True]
                with patch.object(MODULE, 'start', return_value=initial), \
                        patch.object(MODULE, 'STOP', stop_event), \
                        patch.object(MODULE.signal, 'signal'), \
                        self.assertRaisesRegex(RuntimeError, 'identity changed'):
                    MODULE.run(self.config, self.root)
                self.assertEqual(self.mutation_calls(), self.expected_stop_calls())


if __name__ == '__main__':
    unittest.main()
