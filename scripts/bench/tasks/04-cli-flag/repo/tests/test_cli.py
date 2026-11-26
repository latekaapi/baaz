import os, subprocess, sys, unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def run(args, text):
    return subprocess.run([sys.executable, os.path.join(ROOT, "sortlines.py")] + args, input=text, capture_output=True, text=True).stdout


class T(unittest.TestCase):
    def test_sorts(self):
        self.assertEqual(run([], "b\na\nc\n"), "a\nb\nc\n")
