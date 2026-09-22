"""Include the offline conversation peers in the canonical backend check."""
from pathlib import Path
import subprocess
import sys
import unittest


class ConversationRuntimeTests(unittest.TestCase):
    def test_offline_protocol_suite(self):
        root = Path(__file__).resolve().parents[2]
        result = subprocess.run(
            [sys.executable, '-m', 'unittest', 'discover', '-s',
             str(root / 'tools/conversation-runtime'), '-p', 'test_*.py'],
            capture_output=True, text=True, timeout=15, cwd=root,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
