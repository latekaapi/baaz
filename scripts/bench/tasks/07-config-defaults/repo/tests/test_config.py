import json, os, sys, tempfile, unittest
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from config import load_config, DEFAULTS


class T(unittest.TestCase):
    def write(self, data):
        fd, path = tempfile.mkstemp(suffix=".json")
        with os.fdopen(fd, "w") as f:
            json.dump(data, f)
        return path

    def test_merge(self):
        self.assertEqual(load_config(self.write({"port": 9000})), {"port": 9000, "debug": False, "name": "app"})

    def test_empty(self):
        self.assertEqual(load_config(self.write({})), DEFAULTS)

    def test_defaults_untouched(self):
        load_config(self.write({"debug": True}))
        self.assertFalse(DEFAULTS["debug"])
