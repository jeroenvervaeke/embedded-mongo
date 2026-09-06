"""The URI parsing both clients share.

A pure function, so this needs no engine and is the one place in this suite that runs in
milliseconds. Worth having precisely because both clients depend on it: a mistake here decides
whether an address reaches the embedded engine or a real server.
"""

import unittest

from pymongo_embedded.common import path_from_uri


class PathFromUriTest(unittest.TestCase):
    def test_both_embedded_spellings_name_the_same_directory(self):
        self.assertEqual("/data/app", path_from_uri("mongodb_embedded:///data/app"))
        self.assertEqual("/data/app", path_from_uri("mongodb+embedded:///data/app"))

    def test_a_percent_encoded_path_is_decoded(self):
        self.assertEqual("/data/my app", path_from_uri("mongodb_embedded:///data/my%20app"))

    def test_anything_else_is_left_to_pymongo(self):
        """`None` rather than an error, because that is what lets one client class serve both a
        real server and a directory."""
        for uri in (
            "mongodb://localhost:27017/",
            "mongodb+srv://cluster.example.com/",
            ["mongodb://a:27017", "mongodb://b:27017"],
            None,
            27017,
        ):
            with self.subTest(uri=uri):
                self.assertIsNone(path_from_uri(uri))

    def test_an_embedded_uri_carrying_more_than_a_directory_is_refused(self):
        """Options are the trap: `?replicaSet=...` on an embedded URI would otherwise be
        silently swallowed into the directory name and open a directory nobody meant.
        """
        for uri in (
            "mongodb_embedded://",
            "mongodb_embedded:///data?replicaSet=rs0",
            "mongodb_embedded:///data#fragment",
        ):
            with self.subTest(uri=uri):
                with self.assertRaises(ValueError):
                    path_from_uri(uri)


if __name__ == "__main__":
    unittest.main()
