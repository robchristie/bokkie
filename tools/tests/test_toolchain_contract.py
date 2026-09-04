from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / "tools" / "toolchain_contract.py"
SPEC = importlib.util.spec_from_file_location("toolchain_contract", CHECKER)
assert SPEC and SPEC.loader
CONTRACT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CONTRACT
SPEC.loader.exec_module(CONTRACT)


class ToolchainContractTests(unittest.TestCase):
    def test_repository_contract_is_exact(self) -> None:
        self.assertEqual(CONTRACT.validate(ROOT), [])


if __name__ == "__main__":
    unittest.main()
