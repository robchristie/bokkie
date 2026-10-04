"""The disposable launcher must reject broader effective Docker authority."""
import copy
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('run_boundary', ROOT / 'tools/container-probe/run_boundary.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ContainerBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.expected = MODULE.configuration('sha256:' + 'a' * 64, 'bokkie-boundary-12345678', 'test')
        self.actual = {'HostConfig': copy.deepcopy(self.expected['HostConfig']),
                       'Config': {'User': '10001:10001', 'Labels': {'bokkie.boundary': 'test'}},
                       'Image': self.expected['Image'], 'State': {'Running': True}}
        self.actual['HostConfig'].update(Privileged=False, CapAdd=None, PortBindings={},
                                         PidMode='', IpcMode='private', UsernsMode='')

    def test_exact_configuration_is_accepted(self):
        MODULE.validate_effective(self.expected, self.actual)

    def test_effective_relaxations_fail_before_any_probe(self):
        mutations = {'CapDrop': [], 'CapAdd': ['SYS_ADMIN'], 'Privileged': True,
                     'ReadonlyRootfs': False, 'NetworkMode': 'bridge', 'PidMode': 'host',
                     'IpcMode': 'host', 'UsernsMode': 'host', 'SecurityOpt': [],
                     'MaskedPaths': [], 'PortBindings': {'80/tcp': [{'HostPort': '80'}]},
                     'Mounts': [{'Type': 'bind', 'Source': '/', 'Target': '/host'}],
                     'PidsLimit': -1, 'Memory': 0, 'NanoCpus': 0}
        for key, value in mutations.items():
            with self.subTest(key=key):
                actual = copy.deepcopy(self.actual)
                actual['HostConfig'][key] = value
                with self.assertRaises(ValueError):
                    MODULE.validate_effective(self.expected, actual)

    def test_identity_mismatch_fails(self):
        for path, value in ((('Config', 'User'), '0'), (('Image',), 'sha256:' + 'b' * 64),
                            (('State', 'Running'), False)):
            actual = copy.deepcopy(self.actual)
            target = actual
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = value
            with self.subTest(path=path), self.assertRaises(ValueError):
                MODULE.validate_effective(self.expected, actual)

    def test_sys_masks_and_measured_proc_masks_remain(self):
        host = self.expected['HostConfig']
        self.assertIn('/sys/firmware', host['MaskedPaths'])
        self.assertIn('/sys/devices/virtual/powercap', host['MaskedPaths'])
        self.assertIn('/proc/scsi', host['MaskedPaths'])
        self.assertNotIn('/proc/kcore', host['MaskedPaths'])
        self.assertEqual(host['ReadonlyPaths'], [])
        self.assertFalse(any('unconfined' in option for option in host['SecurityOpt']))


if __name__ == '__main__':
    unittest.main()
