import unittest, sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from dates import parse_date


class T(unittest.TestCase):
    def test_nov(self):
        self.assertEqual(parse_date("26 Nov 2026"), (2026, 11, 26))

    def test_jan(self):
        self.assertEqual(parse_date("1 Jan 2025"), (2025, 1, 1))
