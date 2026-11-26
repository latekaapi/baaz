import unittest, sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from users import make_username, make_handle


class T(unittest.TestCase):
    def test_username(self):
        self.assertEqual(make_username(" Ada ", "Love-lace"), "ada.lovelace")

    def test_handle(self):
        self.assertEqual(make_handle("Bob!"), "@bob")
