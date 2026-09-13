"""Public dependency preparation through the actual filesystem/network boundary."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('dependency_broker', Path(__file__).resolve().parents[1] / 'engineering-runtime/broker.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)
d = b.dependencies


class DependencyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.workspace = self.root / 'workspace'
        self.workspace.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.workspace)], check=True)
        (self.workspace / '.gitignore').write_text('/target/\n')
        (self.workspace / 'Cargo.toml').write_text('[package]\nname="dependency-fixture"\nversion="0.1.0"\nedition="2021"\n')
        (self.workspace / 'Cargo.lock').write_text('version = 4\n[[package]]\nname="dependency-fixture"\nversion="0.1.0"\n')
        (self.workspace / 'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.85.0"\n')
        (self.workspace / 'src').mkdir()
        (self.workspace / 'src/lib.rs').write_text('pub fn fixture() {}\n')
        cargo = Path.home() / '.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin/cargo'
        self.peer = object.__new__(b.Broker)
        self.peer.manifest = {'workspace': str(self.workspace), 'worker_network_access': False,
            'bwrap': shutil.which('bwrap'), 'role': 'worker', 'dependency_preparation': {
                'storage': str(self.workspace / 'target/dependencies'), 'cargo': str(cargo),
                'timeout_seconds': 30, 'max_bytes': 1024**2}}
        self.peer.root = self.root / 'broker/run'
        if not cargo.exists() or not shutil.which('bwrap'):
            self.skipTest('installed exact Rust 1.85.0 and Bubblewrap required')

    def test_real_restricted_fixture_missing_prepare_reuse_stale(self):
        with self.assertRaisesRegex(ValueError, 'missing'):
            d.ready(self.peer)
        result = d.ready(self.peer, prepare=True)
        self.assertEqual(result['model_turns'], 0)
        self.assertIn('--unshare-net', result['checks'][-1]['command'])
        self.assertTrue(d.ready(self.peer)['reused'])
        with (self.workspace / 'Cargo.toml').open('a') as stream:
            stream.write('\n# changed binding\n')
        with self.assertRaisesRegex(ValueError, 'stale'):
            d.ready(self.peer)
        self.assertFalse(d.ready(self.peer, prepare=True)['reused'])

    def test_incomplete_material_rejected(self):
        d.ready(self.peer, prepare=True)
        material = Path(self.peer.manifest['dependency_preparation']['storage']) / 'cargo'
        before = self.peer.command_environment_identity()
        (material / 'unexpected').write_text('changed')
        self.assertNotEqual(before, self.peer.command_environment_identity())
        with self.assertRaisesRegex(ValueError, 'incomplete'):
            d.ready(self.peer)

    def test_rejects_alternate_sources_and_unignored_storage(self):
        with (self.workspace / 'Cargo.lock').open('a') as stream:
            stream.write('source="git+ssh://private.example/package#abc"\n')
        with self.assertRaisesRegex(ValueError, 'unsupported public source'):
            d.configuration(self.peer.manifest)
        self.peer.manifest['dependency_preparation']['storage'] = str(self.workspace / 'unignored')
        with self.assertRaisesRegex(ValueError, 'Git ignored'):
            d.configuration(self.peer.manifest)

    def test_rejects_ancestor_configuration_and_sanitises_environment(self):
        (self.workspace / '.cargo').mkdir()
        (self.workspace / '.cargo/config.toml').write_text('[net]\noffline=true\n')
        with self.assertRaisesRegex(ValueError, 'ancestor Cargo configuration'):
            d.ready(self.peer, prepare=True)
        (self.workspace / '.cargo/config.toml').unlink()
        (self.root / '.cargo').mkdir()
        (self.root / '.cargo/config.toml').write_text('[net]\noffline=true\n')
        with self.assertRaisesRegex(ValueError, 'ancestor Cargo configuration'):
            d.ready(self.peer, prepare=True)
        with patch.dict(os.environ, {'CARGO_BUILD_RUSTC_WRAPPER': '/evil', 'RUSTFLAGS': 'evil'}):
            environment = self.peer.environment()
        self.assertNotIn('RUSTFLAGS', environment)
        self.assertNotIn('CARGO_BUILD_RUSTC_WRAPPER', environment)
        self.assertEqual(environment['CARGO_NET_OFFLINE'], 'true')

    def test_rejects_symlink_directories_and_fifos_before_spawn(self):
        storage = Path(self.peer.manifest['dependency_preparation']['storage'])
        storage.mkdir(parents=True)
        outside = self.root / 'outside'
        outside.mkdir()
        for name in ('home', 'tmp', 'cargo'):
            with self.subTest(name=name):
                (storage / name).symlink_to(outside, target_is_directory=True)
                with patch.object(d.subprocess, 'Popen') as spawn:
                    with self.assertRaisesRegex(ValueError, 'symlink or non-regular'):
                        d.ready(self.peer, prepare=True)
                spawn.assert_not_called()
                (storage / name).unlink()
        os.mkfifo(storage / 'blocked')
        with self.assertRaisesRegex(ValueError, 'non-regular'):
            d.inventory(storage, 1024**2)
        with patch.object(d.subprocess, 'Popen') as spawn:
            with self.assertRaisesRegex(ValueError, 'non-regular'):
                d.execute(self.peer, 'metadata')
        spawn.assert_not_called()

    def test_command_failure_has_bounded_sanitised_cause(self):
        d.ready(self.peer, prepare=True)
        cause = d.diagnostic_tail(b'error: missing dependency; token=private https://user:password@example.com/file?secret=value')
        self.assertIn('missing dependency', cause)
        self.assertNotIn('private', cause)
        self.assertNotIn('user:password', cause)
        self.assertNotIn('secret=value', cause)
        self.assertLessEqual(len(d.diagnostic_tail(b'x' * 10000).encode()), 2048)
        with patch.object(d, 'command', return_value=['/bin/sh', '-c', 'echo "error: missing lockfile"; exit 1']):
            with self.assertRaisesRegex(ValueError, 'cause: error: missing lockfile'):
                d.execute(self.peer, 'metadata')

    def test_failed_readiness_never_writes_receipt(self):
        with patch.object(d, 'execute', side_effect=ValueError('boundary failed')):
            with self.assertRaisesRegex(ValueError, 'boundary failed'):
                d.ready(self.peer, prepare=True)
        self.assertFalse((Path(self.peer.manifest['dependency_preparation']['storage']) / 'ready.json').exists())


if __name__ == '__main__':
    unittest.main()
