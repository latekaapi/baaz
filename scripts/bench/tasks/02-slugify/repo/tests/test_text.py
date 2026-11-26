import unittest, sys, os
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from text import slugify


class T(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_punctuation(self):
        self.assertEqual(slugify("Rust, Go & Python!"), "rust-go-python")

    def test_spaces(self):
        self.assertEqual(slugify("  many   spaces  "), "many-spaces")

    def test_empty(self):
        self.assertEqual(slugify(""), "")
