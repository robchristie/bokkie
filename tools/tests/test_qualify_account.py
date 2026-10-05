"""Offline checks for synthetic bind qualification guards and its real probes."""
import contextlib
import copy
import errno
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('qualify_account', ROOT / 'deploy/qualify_account.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class AccountQualificationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='bokkie-account-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.enterContext(patch.object(MODULE.manage, 'api', side_effect=AssertionError('Engine access forbidden')))
        self.enterContext(patch.object(MODULE.subprocess, 'run', side_effect=AssertionError('subprocess forbidden')))

    def test_mode_selection_accepts_no_custom_paths_or_credentials(self):
        self.assertFalse(MODULE.parse_arguments([]).srv_bind_mounts)
        self.assertTrue(MODULE.parse_arguments(['--srv-bind-mounts']).srv_bind_mounts)
        for arguments in (['/srv/real-data'], ['--root', '/srv/real-data'],
                          ['--srv-bind-mounts', '/srv/real-data'], ['--auth', '/home/real-auth']):
            with self.subTest(arguments=arguments), contextlib.redirect_stderr(io.StringIO()), \
                    self.assertRaises(SystemExit):
                MODULE.parse_arguments(arguments)

    def test_mode_checks_only_fixed_marked_roots_before_loading_release(self):
        for arguments, roots in (([], [Path('/home/rob/bokkie-deployment-calibration')]),
                                (['--srv-bind-mounts'], [Path('/home/rob/bokkie-deployment-calibration'),
                                                       Path('/srv/stacks/bokkie-bind-calibration')])):
            with self.subTest(arguments=arguments), \
                    patch.object(MODULE.sys, 'argv', ['qualify_account.py', *arguments]), \
                    patch.object(MODULE, 'marked_root') as guard, \
                    patch.object(MODULE.manage, 'load', side_effect=RuntimeError('stop before host access')):
                with self.assertRaisesRegex(RuntimeError, 'stop before host access'):
                    MODULE.run()
                self.assertEqual([call.args[0] for call in guard.call_args_list], roots)

    def test_marker_rejects_unmarked_wrong_and_redirected_roots(self):
        with self.assertRaises(FileNotFoundError):
            MODULE.marked_root(self.root)
        marker = self.root / '.synthetic-bokkie-deployment'
        marker.write_text('not a synthetic fixture')
        with self.assertRaisesRegex(ValueError, 'synthetic marker'):
            MODULE.marked_root(self.root)
        marker.write_text(MODULE.MARKER)
        MODULE.marked_root(self.root)
        redirected = self.root / 'redirected'
        redirected.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'redirected'):
            MODULE.marked_root(redirected)
        marker.unlink()
        real_marker = self.root / 'other-marker'
        real_marker.write_text(MODULE.MARKER)
        marker.symlink_to(real_marker)
        with self.assertRaisesRegex(ValueError, 'synthetic marker'):
            MODULE.marked_root(self.root)

    def test_mount_evidence_uses_final_mount_and_requires_every_target(self):
        text = ('10 1 1:2 / /data rw,relatime - ext4 /dev/example rw\n'
                '11 1 1:3 / /data ro,nosuid,nodev,noatime shared:2 - xfs /dev/example rw\n'
                '12 1 1:3 /profile /opt/conversation-profile.json ro,nosuid,nodev,noatime - xfs /dev/example rw\n')
        mounts = MODULE.read_mounts(text, ['/data', '/opt/conversation-profile.json'])
        MODULE.require_readonly_mounts(mounts)
        self.assertEqual(mounts['/data']['filesystem'], 'xfs')
        with self.assertRaisesRegex(AssertionError, 'mount missing'):
            MODULE.read_mounts(text, ['/missing'])

    def test_readonly_evidence_rejects_weaker_mount_flags_or_wrong_filesystem(self):
        valid = {'/data': {'options': ['ro', 'nosuid', 'nodev', 'noatime'], 'filesystem': 'xfs'}}
        for absent in ('ro', 'nosuid', 'nodev', 'noatime'):
            changed = copy.deepcopy(valid)
            changed['/data']['options'].remove(absent)
            with self.subTest(absent=absent), self.assertRaises(AssertionError):
                MODULE.require_readonly_mounts(changed)
        for change in ({'options': ['ro', 'rw', 'nosuid', 'nodev', 'noatime']}, {'filesystem': 'ext4'}):
            changed = copy.deepcopy(valid)
            changed['/data'].update(change)
            with self.subTest(change=change), self.assertRaises(AssertionError):
                MODULE.require_readonly_mounts(changed)

    def test_real_writable_data_is_rejected(self):
        canary = self.root / 'canary'
        canary.write_bytes(b'synthetic bind data\n')
        with self.assertRaisesRegex(AssertionError, 'unexpectedly succeeded: write'):
            MODULE.readonly_data_operations(canary)

    def test_all_data_operations_must_fail_specifically_with_readonly_filesystem(self):
        for successful in (None, 'write', 'truncate', 'create', 'unlink', 'rename'):
            with self.subTest(successful=successful), contextlib.ExitStack() as stack:
                for operation, owner, method in (('write', Path, 'write_bytes'),
                                                  ('truncate', MODULE.os, 'truncate'),
                                                  ('create', Path, 'touch'), ('unlink', Path, 'unlink'),
                                                  ('rename', Path, 'rename')):
                    stack.enter_context(patch.object(owner, method, return_value=None,
                        side_effect=None if operation == successful else OSError(errno.EROFS, 'read-only fixture')))
                if successful is None:
                    self.assertEqual(MODULE.readonly_data_operations(self.root / 'canary'),
                                     {name: errno.EROFS for name in ('write', 'truncate', 'create', 'unlink', 'rename')})
                else:
                    with self.assertRaisesRegex(AssertionError, 'unexpectedly succeeded: ' + successful):
                        MODULE.readonly_data_operations(self.root / 'canary')
        with patch.object(Path, 'write_bytes', side_effect=PermissionError(errno.EACCES, 'permission denied')), \
                self.assertRaisesRegex(AssertionError, 'not read-only filesystem'):
            MODULE.readonly_data_operations(self.root / 'canary')

    def test_embedded_outer_and_payload_programs_compile(self):
        for script in (MODULE.OUTER, MODULE.PAYLOAD):
            compile(MODULE.probe_program(script), '<account-qualification>', 'exec')


if __name__ == '__main__':
    unittest.main()
