"""Include the workspace host's zero-model regression suite in governance CI."""
from pathlib import Path
import subprocess
import sys
import unittest


class WorkspaceRuntimeTests(unittest.TestCase):
    def test_zero_model_host_protocol_and_ownership(self):
        root=Path(__file__).resolve().parents[2]
        result=subprocess.run([sys.executable,'-m','unittest','discover','-s',
                    str(root/'tools/workspace-runtime'),'-p','test_*.py'],
                    capture_output=True,text=True,timeout=15,cwd=root)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
