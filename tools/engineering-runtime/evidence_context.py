#!/usr/bin/env python3
"""Bounded current source/environment observation for validation applicability."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys

spec = importlib.util.spec_from_file_location('evidence_broker', Path(__file__).with_name('broker.py'))
broker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(broker)
capture = object.__new__(broker.Broker)
capture.manifest = {**json.load(sys.stdin), 'role': 'worker'}
print(json.dumps({'source': capture.source_snapshot()}))
