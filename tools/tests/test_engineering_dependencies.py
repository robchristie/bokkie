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

    def test_restricted_build_growth_preserves_dependency_readiness(self):
        self.peer.manifest['worker_scratch'] = str(self.workspace / 'target/worker-scratch')
        Path(self.peer.manifest['worker_scratch']).mkdir(parents=True)
        prepared = d.ready(self.peer, prepare=True)
        environment = self.peer.environment()
        storage = Path(self.peer.manifest['dependency_preparation']['storage'])
        build = Path(environment['CARGO_TARGET_DIR'])
        self.assertEqual(build, storage.with_name('dependencies-build'))
        self.assertFalse(build.is_relative_to(storage))
        self.assertFalse(storage.is_relative_to(build))
        identity = self.peer.command_environment_identity()
        args = d.command(self.peer, 'metadata')
        args[args.index('metadata')] = 'build'
        args = args[:-2]  # Build uses the same restricted boundary without metadata's format.
        subprocess.run(args, env=environment, check=True, capture_output=True, timeout=30)
        self.assertTrue((build / 'debug/libdependency_fixture.rlib').is_file())
        # Build output may exceed the entire dependency budget without entering
        # either its allocation check or its reusable material identity.
        (build / 'large-output').write_bytes(b'x' * (2 * self.peer.manifest['dependency_preparation']['max_bytes']))
        self.assertEqual(identity, self.peer.command_environment_identity())
        reused = d.ready(self.peer)
        self.assertTrue(reused['reused'])
        self.assertEqual(prepared['material'], reused['material'])

    def test_build_path_rejects_aliases_special_nodes_and_unignored_paths(self):
        storage, build = d.storage_paths(self.peer.manifest)
        storage.mkdir(parents=True)
        outside = self.root / 'outside'
        outside.mkdir()
        for destination in (outside, storage, self.root / 'missing'):
            with self.subTest(destination=destination):
                build.symlink_to(destination, target_is_directory=True)
                for operation in (d.configuration, d.environment):
                    with self.assertRaisesRegex(ValueError, 'build storage.*canonical'):
                        operation(self.peer.manifest)
                build.unlink()
        for kind in ('file', 'fifo'):
            with self.subTest(kind=kind):
                if kind == 'file':
                    build.write_text('invalid')
                else:
                    os.mkfifo(build)
                with self.assertRaisesRegex(ValueError, 'real directories'):
                    d.environment(self.peer.manifest)
                build.unlink()
        (self.workspace / '.gitignore').write_text('/target/dependencies/\n')
        with self.assertRaisesRegex(ValueError, 'build storage must be Git ignored'):
            d.configuration(self.peer.manifest)

    def test_storage_paths_cannot_escape_workspace_or_alias_an_ancestor(self):
        for location in (self.root / 'outside', self.workspace,
                         self.workspace / 'target/../../outside'):
            with self.subTest(location=location):
                self.peer.manifest['dependency_preparation']['storage'] = str(location)
                with self.assertRaisesRegex(ValueError, 'canonical and strictly inside'):
                    d.environment(self.peer.manifest)
        (self.workspace / 'target').symlink_to(self.root, target_is_directory=True)
        self.peer.manifest['dependency_preparation']['storage'] = str(self.workspace / 'target/dependencies')
        with self.assertRaisesRegex(ValueError, 'canonical and strictly inside'):
            d.environment(self.peer.manifest)

    def test_legacy_build_allocation_requires_explicit_repair(self):
        d.ready(self.peer, prepare=True)
        storage = Path(self.peer.manifest['dependency_preparation']['storage'])
        legacy = storage / 'target'
        legacy.mkdir()
        retained = legacy / 'old-build'
        retained.write_bytes(b'x' * (2 * self.peer.manifest['dependency_preparation']['max_bytes']))
        with self.assertRaisesRegex(ValueError, 'storage bound exceeded'):
            d.ready(self.peer, prepare=True)
        self.assertTrue(retained.is_file())

    def test_rejects_alternate_sources_and_unignored_storage(self):
        with (self.workspace / 'Cargo.lock').open('a') as stream:
            stream.write('source="git+ssh://private.example/package#abc"\n')
        with self.assertRaisesRegex(ValueError, 'unsupported public source'):
            d.configuration(self.peer.manifest)
        self.peer.manifest['dependency_preparation']['storage'] = str(self.workspace / 'unignored')
        with self.assertRaisesRegex(ValueError, 'Git ignored'):
            d.configuration(self.peer.manifest)

    def test_parsed_patch_and_replace_forms_are_rejected_before_fetch(self):
        manifest = self.workspace / 'Cargo.toml'
        original = manifest.read_text()
        declarations = (
            '[ patch.crates-io ]\nunsupported = { git = "https://unsupported.invalid/dependency" }\n',
            '["patch"."crates-io"]\nunsupported = { git = "https://unsupported.invalid/dependency" }\n',
            'patch.crates-io.unsupported = { git = "https://unsupported.invalid/dependency" }\n',
            '[ replace ]\n"unsupported:1.0.0" = { git = "https://unsupported.invalid/dependency" }\n',
            '["replace"]\n"unsupported:1.0.0" = { git = "https://unsupported.invalid/dependency" }\n',
            '"replace"."unsupported:1.0.0" = { git = "https://unsupported.invalid/dependency" }\n',
        )
        for declaration in declarations:
            with self.subTest(declaration=declaration):
                # Dotted assignments must precede [package] to remain top-level.
                manifest.write_text(declaration + original)
                with patch.object(d, 'execute') as execute:
                    with self.assertRaisesRegex(ValueError, 'unpatched root package'):
                        d.ready(self.peer, prepare=True)
                execute.assert_not_called()
                self.assertFalse((Path(self.peer.manifest['dependency_preparation']['storage']) / 'ready.json').exists())

    def test_explicit_package_workspace_is_rejected_before_fetch(self):
        with (self.workspace / 'Cargo.toml').open('a') as stream:
            stream.write('workspace = ".."\n')
        with patch.object(d, 'execute') as execute:
            with self.assertRaisesRegex(ValueError, 'unpatched root package'):
                d.ready(self.peer, prepare=True)
        execute.assert_not_called()

    def test_implicit_ancestor_workspace_is_rejected_offline_before_fetch(self):
        (self.root / 'Cargo.toml').write_text('[workspace]\nmembers = ["workspace"]\n[patch.crates-io]\nunsupported = { git = "https://unsupported.invalid/dependency" }\n')
        execute = d.execute
        operations = []
        def observed(peer, operation, online=False):
            operations.append((operation, online))
            self.assertFalse(online, 'ancestor resolution must fail before online fetching')
            return execute(peer, operation, online)
        # Even profiles allowing worker networking must locate the workspace
        # with OS-enforced network isolation and the installed Cargo executable.
        self.peer.manifest['worker_network_access'] = True
        with patch.object(d, 'execute', side_effect=observed):
            with self.assertRaisesRegex(ValueError, 'outside the registered root manifest'):
                d.ready(self.peer, prepare=True)
        self.assertEqual(operations, [('locate-project', False)])
        args = d.command(self.peer, 'locate-project')
        self.assertIn('--unshare-net', args)
        self.assertIn('--manifest-path', args)
        self.assertIn(self.peer.manifest['dependency_preparation']['cargo'], args)
        self.assertFalse((Path(self.peer.manifest['dependency_preparation']['storage']) / 'ready.json').exists())

    def test_root_input_symlinks_and_special_files_rejected_before_parse(self):
        for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml'):
            path = self.workspace / name
            original = path.read_bytes()
            outside = self.root / ('outside-' + name)
            outside.write_bytes(original)
            for kind in ('symlink', 'fifo'):
                with self.subTest(name=name, kind=kind):
                    path.unlink()
                    if kind == 'symlink':
                        path.symlink_to(outside)
                    else:
                        os.mkfifo(path)
                    with patch.object(d.tomllib, 'loads') as parse, patch.object(d, 'execute') as execute:
                        with self.assertRaisesRegex(ValueError, 'regular non-symlink file'):
                            d.ready(self.peer, prepare=True)
                    parse.assert_not_called()
                    execute.assert_not_called()
                    path.unlink()
                    path.write_bytes(original)

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

    def test_empty_directories_count_towards_storage_bound(self):
        storage = Path(self.peer.manifest['dependency_preparation']['storage'])
        storage.mkdir(parents=True)
        with patch.object(Path, 'rglob', return_value=iter([storage] * 100001)):
            with self.assertRaisesRegex(ValueError, 'entry bound'):
                d.storage_entries(storage, 1024**2)

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
