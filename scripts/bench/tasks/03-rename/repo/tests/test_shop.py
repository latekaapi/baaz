import unittest, sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from shop.pricing import calc_total
from shop.cart import cart_total


class T(unittest.TestCase):
    def test_total(self):
        self.assertEqual(calc_total([(2, 3), (1, 4)]), 10)

    def test_cart(self):
        self.assertEqual(cart_total({"items": [(5, 2)]}), 10)
